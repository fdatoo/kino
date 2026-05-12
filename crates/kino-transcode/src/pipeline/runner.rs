//! FFmpeg process runner, progress parsing, and output verification.

use std::{
    future::pending,
    path::Path,
    process::{ExitStatus, Stdio},
    sync::{Arc, Mutex},
    time::Duration,
};

#[cfg(unix)]
use nix::{
    errno::Errno,
    sys::signal::{Signal, kill},
    unistd::Pid,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, BufReader},
    process::Command,
    sync::oneshot,
    task::JoinError,
    time::{Instant, Sleep, sleep},
};

use crate::{Error, Result, pipeline::FfmpegEncodeCommand};

const STDERR_TAIL_LIMIT: usize = 8 * 1024;
const DEFAULT_GRACE: Duration = Duration::from_secs(2);

/// Parsed FFmpeg `-progress pipe:1` state.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Progress {
    /// Last reported encoded frame number.
    pub frame: u64,
    /// Last reported encode frames per second.
    pub fps: f32,
    /// Last reported output timestamp in microseconds.
    pub time_us: u64,
    /// Last reported encode speed multiplier.
    pub speed: f32,
}

/// Completed FFmpeg process result.
#[derive(Debug, Clone)]
pub struct RunOutcome {
    /// Successful process exit status.
    pub exit_status: ExitStatus,
    /// Final parsed progress snapshot.
    pub progress: Progress,
    /// Bounded tail of FFmpeg stderr.
    pub stderr_tail: String,
}

/// Runs FFmpeg encode commands and tracks progress.
pub struct PipelineRunner {
    progress: Arc<Mutex<Progress>>,
    /// Optional progress callback invoked at most once per second.
    pub progress_callback: Option<Box<dyn Fn(Progress) + Send + Sync>>,
    /// Cancellation grace period between SIGTERM and SIGKILL.
    pub grace: Duration,
}

impl PipelineRunner {
    /// Construct a runner with no progress callback and the default cancellation grace period.
    pub fn new() -> Self {
        Self {
            progress: Arc::new(Mutex::new(Progress::default())),
            progress_callback: None,
            grace: DEFAULT_GRACE,
        }
    }

    /// Return the most recent parsed progress snapshot.
    pub fn current_progress(&self) -> Progress {
        read_progress_snapshot(&self.progress)
    }

    /// Spawn and supervise an FFmpeg encode command.
    pub async fn run(
        &self,
        command: FfmpegEncodeCommand,
        cancel: oneshot::Receiver<()>,
    ) -> Result<RunOutcome> {
        self.run_process(command.into_command(), cancel).await
    }

    async fn run_process(
        &self,
        mut command: Command,
        cancel: oneshot::Receiver<()>,
    ) -> Result<RunOutcome> {
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = command.spawn()?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| Error::Io(std::io::Error::other("child stdout was not piped")))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| Error::Io(std::io::Error::other("child stderr was not piped")))?;
        let pid = child
            .id()
            .ok_or_else(|| Error::Io(std::io::Error::other("child process id unavailable")))?;

        write_progress_snapshot(&self.progress, Progress::default());

        let mut wait_task = tokio::spawn(async move { child.wait().await });
        let mut stdout_task = Box::pin(read_progress(
            stdout,
            Arc::clone(&self.progress),
            self.progress_callback.as_deref(),
        ));
        let mut stderr_task = Box::pin(read_stderr_tail(stderr));
        let mut cancel = cancel;
        let mut cancel_armed = true;
        let mut cancelled = false;
        let mut grace_sleep: Option<std::pin::Pin<Box<Sleep>>> = None;
        let mut stdout_done = false;
        let mut wait_status = None;
        let mut stderr_tail = None;

        loop {
            tokio::select! {
                wait_result = &mut wait_task, if wait_status.is_none() => {
                    wait_status = Some(join_wait_result(wait_result)?);
                }
                progress_result = &mut stdout_task, if !stdout_done => {
                    progress_result?;
                    stdout_done = true;
                }
                tail_result = &mut stderr_task, if stderr_tail.is_none() => {
                    stderr_tail = Some(tail_result?);
                }
                _ = &mut cancel, if cancel_armed && wait_status.is_none() => {
                    cancel_armed = false;
                    cancelled = true;
                    send_signal(pid, ProcessSignal::Terminate)?;
                    grace_sleep = Some(Box::pin(sleep(self.grace)));
                }
                _ = wait_optional_sleep(&mut grace_sleep), if grace_sleep.is_some() && wait_status.is_none() => {
                    grace_sleep = None;
                    send_signal(pid, ProcessSignal::Kill)?;
                }
            }

            if wait_status.is_some() && stdout_done && stderr_tail.is_some() {
                break;
            }
        }

        let stderr_tail = stderr_tail.unwrap_or_default();
        if cancelled {
            return Err(Error::Cancelled);
        }

        let exit_status = match wait_status {
            Some(status) => status,
            None => {
                return Err(Error::Io(std::io::Error::other(
                    "child wait did not complete",
                )));
            }
        };
        let progress = self.current_progress();
        if exit_status.success() {
            Ok(RunOutcome {
                exit_status,
                progress,
                stderr_tail,
            })
        } else {
            Err(Error::FfmpegFailed {
                status: exit_status.code().unwrap_or(-1),
                stderr_tail,
            })
        }
    }
}

impl Default for PipelineRunner {
    fn default() -> Self {
        Self::new()
    }
}

/// Verify an FFmpeg CMAF/HLS output directory.
pub fn verify_outputs(hls_output_dir: &Path) -> Result<()> {
    verify_non_empty_file(&hls_output_dir.join("init.mp4"), "init.mp4")?;
    let playlist_path = hls_output_dir.join("media.m3u8");
    verify_non_empty_file(&playlist_path, "media.m3u8")?;

    let playlist = std::fs::read_to_string(&playlist_path)?;
    let mut segment_count = 0usize;
    for line in playlist.lines().map(str::trim) {
        if is_segment_reference(line) {
            segment_count += 1;
            verify_non_empty_file(&hls_output_dir.join(line), line)?;
        }
    }

    if segment_count == 0 {
        return Err(Error::IntegrityFailed(
            "media.m3u8 declares no media segments".to_owned(),
        ));
    }

    Ok(())
}

async fn read_progress<R>(
    reader: R,
    progress: Arc<Mutex<Progress>>,
    callback: Option<&(dyn Fn(Progress) + Send + Sync)>,
) -> Result<()>
where
    R: AsyncRead + Unpin,
{
    let mut reader = BufReader::new(reader);
    let mut line = String::new();
    let mut last_callback = None;

    loop {
        line.clear();
        let bytes = reader.read_line(&mut line).await?;
        if bytes == 0 {
            break;
        }

        let callback_ready = apply_progress_line(&line, &progress);
        if callback_ready {
            maybe_call_progress_callback(&progress, callback, &mut last_callback);
        }
    }

    Ok(())
}

async fn read_stderr_tail<R>(mut reader: R) -> Result<String>
where
    R: AsyncRead + Unpin,
{
    let mut tail = Vec::with_capacity(STDERR_TAIL_LIMIT);
    let mut buffer = [0u8; 1024];
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        tail.extend_from_slice(&buffer[..read]);
        if tail.len() > STDERR_TAIL_LIMIT {
            let drop_len = tail.len() - STDERR_TAIL_LIMIT;
            tail.drain(..drop_len);
        }
    }

    Ok(String::from_utf8_lossy(&tail).into_owned())
}

fn apply_progress_line(line: &str, progress: &Arc<Mutex<Progress>>) -> bool {
    let Some((key, value)) = line.trim().split_once('=') else {
        return false;
    };

    let mut snapshot = read_progress_snapshot(progress);
    match key {
        "frame" => {
            if let Ok(frame) = value.parse() {
                snapshot.frame = frame;
            }
        }
        "fps" => {
            if let Ok(fps) = value.parse() {
                snapshot.fps = fps;
            }
        }
        "out_time_us" | "out_time_ms" => {
            if let Ok(time_us) = value.parse() {
                snapshot.time_us = time_us;
            }
        }
        "speed" => {
            let speed = value.trim_end_matches('x');
            if let Ok(speed) = speed.parse() {
                snapshot.speed = speed;
            }
        }
        "progress" => {
            return true;
        }
        _ => {}
    }

    write_progress_snapshot(progress, snapshot);
    false
}

fn maybe_call_progress_callback(
    progress: &Arc<Mutex<Progress>>,
    callback: Option<&(dyn Fn(Progress) + Send + Sync)>,
    last_callback: &mut Option<Instant>,
) {
    let Some(callback) = callback else {
        return;
    };

    let now = Instant::now();
    if last_callback
        .is_some_and(|last_callback| now.duration_since(last_callback) < Duration::from_secs(1))
    {
        return;
    }

    *last_callback = Some(now);
    callback(read_progress_snapshot(progress));
}

fn read_progress_snapshot(progress: &Arc<Mutex<Progress>>) -> Progress {
    match progress.lock() {
        Ok(progress) => *progress,
        Err(poisoned) => *poisoned.into_inner(),
    }
}

fn write_progress_snapshot(progress: &Arc<Mutex<Progress>>, snapshot: Progress) {
    match progress.lock() {
        Ok(mut progress) => *progress = snapshot,
        Err(poisoned) => *poisoned.into_inner() = snapshot,
    }
}

async fn wait_optional_sleep(sleep: &mut Option<std::pin::Pin<Box<Sleep>>>) {
    match sleep.as_mut() {
        Some(sleep) => sleep.as_mut().await,
        None => pending().await,
    }
}

fn join_wait_result(
    result: std::result::Result<std::io::Result<ExitStatus>, JoinError>,
) -> Result<ExitStatus> {
    match result {
        Ok(status) => status.map_err(Error::from),
        Err(source) => Err(Error::Io(std::io::Error::other(source))),
    }
}

#[derive(Debug, Clone, Copy)]
enum ProcessSignal {
    Terminate,
    Kill,
}

#[cfg(unix)]
fn send_signal(pid: u32, signal: ProcessSignal) -> Result<()> {
    let signal = match signal {
        ProcessSignal::Terminate => Signal::SIGTERM,
        ProcessSignal::Kill => Signal::SIGKILL,
    };
    match kill(Pid::from_raw(pid as i32), signal) {
        Ok(()) | Err(Errno::ESRCH) => Ok(()),
        Err(source) => Err(Error::Io(std::io::Error::from_raw_os_error(source as i32))),
    }
}

#[cfg(not(unix))]
fn send_signal(_pid: u32, _signal: ProcessSignal) -> Result<()> {
    Err(Error::Io(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "process cancellation is unsupported on this platform",
    )))
}

fn verify_non_empty_file(path: &Path, label: &str) -> Result<()> {
    let metadata = std::fs::metadata(path).map_err(|source| {
        if source.kind() == std::io::ErrorKind::NotFound {
            Error::IntegrityFailed(format!("{label} is missing"))
        } else {
            Error::Io(source)
        }
    })?;
    if !metadata.is_file() {
        return Err(Error::IntegrityFailed(format!("{label} is not a file")));
    }
    if metadata.len() == 0 {
        return Err(Error::IntegrityFailed(format!("{label} is empty")));
    }
    Ok(())
}

fn is_segment_reference(line: &str) -> bool {
    let Some(segment) = line
        .strip_prefix("seg-")
        .and_then(|line| line.strip_suffix(".m4s"))
    else {
        return false;
    };
    !segment.is_empty() && segment.bytes().all(|byte| byte.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use std::{
        fs, io,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::Instant as StdInstant,
    };

    use tempfile::tempdir;

    use super::*;

    #[tokio::test]
    async fn progress_parser_reads_ffmpeg_progress_fixture()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let fixture = b"frame=42\nfps=29.97\nstream_0_0_q=27.0\nout_time_us=1401000\nspeed=1.25x\nprogress=continue\nframe=84\nfps=31.5\nout_time_us=2802000\nspeed=1.50x\nprogress=end\n";
        let progress = Arc::new(Mutex::new(Progress::default()));
        let callback_count = Arc::new(AtomicUsize::new(0));
        let callback_count_for_callback = Arc::clone(&callback_count);
        let callback = move |_progress: Progress| {
            callback_count_for_callback.fetch_add(1, Ordering::SeqCst);
        };

        read_progress(&fixture[..], Arc::clone(&progress), Some(&callback)).await?;

        assert_eq!(
            read_progress_snapshot(&progress),
            Progress {
                frame: 84,
                fps: 31.5,
                time_us: 2_802_000,
                speed: 1.5,
            }
        );
        assert_eq!(callback_count.load(Ordering::SeqCst), 1);

        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancellation_terminates_child_before_grace_expires()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        if fs::metadata("/usr/bin/sleep").is_err() {
            return Ok(());
        }

        let runner = PipelineRunner {
            progress: Arc::new(Mutex::new(Progress::default())),
            progress_callback: None,
            grace: Duration::from_secs(2),
        };
        let mut command = Command::new("/usr/bin/sleep");
        command.arg("60");
        let (cancel_tx, cancel_rx) = oneshot::channel();
        let started = StdInstant::now();
        let task = tokio::spawn(async move { runner.run_process(command, cancel_rx).await });

        sleep(Duration::from_millis(100)).await;
        let _sent = cancel_tx.send(());
        let result = task.await?;

        assert!(matches!(result, Err(Error::Cancelled)));
        assert!(started.elapsed() < Duration::from_secs(3));

        Ok(())
    }

    #[test]
    fn verify_outputs_accepts_playlist_and_rejects_empty_segment()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let dir = tempdir()?;
        fs::write(dir.path().join("init.mp4"), b"init")?;
        fs::write(dir.path().join("seg-00001.m4s"), b"one")?;
        fs::write(dir.path().join("seg-00002.m4s"), b"two")?;
        fs::write(
            dir.path().join("media.m3u8"),
            b"#EXTM3U\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXTINF:6.0,\nseg-00001.m4s\n#EXTINF:6.0,\nseg-00002.m4s\n#EXT-X-ENDLIST\n",
        )?;

        verify_outputs(dir.path())?;

        fs::write(dir.path().join("seg-00002.m4s"), b"")?;
        assert!(matches!(
            verify_outputs(dir.path()),
            Err(Error::IntegrityFailed(_))
        ));

        Ok(())
    }

    #[test]
    fn is_transient_classifies_retryable_errors() {
        let cases = [
            (
                Error::FfmpegFailed {
                    status: 1,
                    stderr_tail: "CUDA out of memory".to_owned(),
                },
                true,
            ),
            (
                Error::FfmpegFailed {
                    status: 1,
                    stderr_tail: "device is busy".to_owned(),
                },
                true,
            ),
            (
                Error::FfmpegFailed {
                    status: 1,
                    stderr_tail: "Resource Temporarily Unavailable".to_owned(),
                },
                true,
            ),
            (
                Error::FfmpegFailed {
                    status: 1,
                    stderr_tail: "invalid data found when processing input".to_owned(),
                },
                false,
            ),
            (Error::Io(io::Error::from(io::ErrorKind::TimedOut)), true),
            (
                Error::Io(io::Error::from(io::ErrorKind::InvalidData)),
                false,
            ),
            (Error::Cancelled, false),
            (Error::IntegrityFailed("missing segment".to_owned()), false),
        ];

        for (error, expected) in cases {
            assert_eq!(error.is_transient(), expected, "{error:?}");
        }
    }
}
