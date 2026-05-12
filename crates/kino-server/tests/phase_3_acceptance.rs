use std::{
    env,
    net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener},
    path::{Path, PathBuf},
    process::Stdio,
    sync::OnceLock,
    time::{Duration, Instant},
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use kino_core::{Config, Id, Timestamp};
use m3u8_rs::Playlist;
use reqwest::Client;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::{process::Child, sync::Mutex, time::sleep};

const ACCEPTANCE_TOKEN: &str = "erli5NM_veiB9icLAIrqsVXhrdFIzey8qGrzYQgwrkY";
const TEST_TIMEOUT: Duration = Duration::from_secs(240);
const POLL_INTERVAL: Duration = Duration::from_millis(250);

static ACCEPTANCE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[cfg_attr(not(feature = "acceptance-tests"), ignore)]
async fn phase_3_hdr10_end_to_end_transcodes_policy_outputs()
-> Result<(), Box<dyn std::error::Error>> {
    let _guard = acceptance_lock().lock().await;
    let mut env = AcceptanceEnv::start("hdr10-e2e", SchedulerMode::Fast).await?;
    let source = env.generate_hdr10_source("hdr10-e2e.mkv", 10).await?;
    let (media_item_id, source_file_id) = env.seed_source_file(&source).await?;

    env.replan(source_file_id).await?;
    env.wait_for_completed_jobs(source_file_id, 3).await?;

    let jobs = env.jobs_for_source(source_file_id).await?;
    assert_eq!(jobs.len(), 3);
    assert!(jobs.iter().all(|job| job["state"] == "completed"));

    let outputs = transcode_output_count(&env.db, source_file_id).await?;
    assert_eq!(outputs, 3);
    let downgrades = downgrade_kinds(&env.db, source_file_id).await?;
    assert_eq!(downgrades, vec![String::from("hdr10_to_sdr")]);

    let master = env.master_playlist(media_item_id).await?;
    assert_master_variants(&master, 3)?;
    assert!(master.contains("VIDEO-RANGE=PQ"));
    assert!(master.contains("VIDEO-RANGE=SDR"));

    let encoders = env.get_json("/api/v1/admin/transcodes/encoders").await?;
    let encoders = encoders
        .as_array()
        .ok_or("encoders response must be an array")?;
    assert!(encoders.iter().any(|encoder| encoder["kind"] == "software"));

    env.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[cfg_attr(not(feature = "acceptance-tests"), ignore)]
async fn phase_3_reingest_idempotency_does_not_duplicate_jobs()
-> Result<(), Box<dyn std::error::Error>> {
    let _guard = acceptance_lock().lock().await;
    let mut env = AcceptanceEnv::start("reingest", SchedulerMode::Slow).await?;
    let source = env.generate_hdr10_source("reingest.mkv", 10).await?;
    let (_, source_file_id) = env.seed_source_file(&source).await?;

    env.replan(source_file_id).await?;
    env.replan(source_file_id).await?;

    let jobs = env.jobs_for_source(source_file_id).await?;
    assert_eq!(jobs.len(), 3);
    let rows = transcode_job_count(&env.db, source_file_id).await?;
    assert_eq!(rows, 3);

    env.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[cfg_attr(not(feature = "acceptance-tests"), ignore)]
async fn phase_3_cancellation_and_recovery_reset_running_work()
-> Result<(), Box<dyn std::error::Error>> {
    let _guard = acceptance_lock().lock().await;
    let mut env = AcceptanceEnv::start("cancel-recovery", SchedulerMode::Fast).await?;
    let source = env.generate_hdr10_source("cancel-recovery.mkv", 30).await?;
    let (_, source_file_id) = env.seed_source_file(&source).await?;
    insert_acceptance_job(&env.db, source_file_id, 1, "planned", "cpu", "high").await?;
    insert_acceptance_job(
        &env.db,
        source_file_id,
        2,
        "planned",
        "cpu",
        "compatibility",
    )
    .await?;

    let running = env.wait_for_state(source_file_id, "running").await?;
    env.post_json(&format!("/api/v1/admin/transcodes/jobs/{running}/cancel"))
        .await?;
    env.wait_for_job_state(running, "cancelled").await?;
    env.wait_for_other_started(source_file_id, running).await?;

    env.child.start_kill()?;
    let _ = env.child.wait().await;
    let stranded =
        insert_acceptance_job(&env.db, source_file_id, 3, "running", "cpu", "high").await?;
    env.restart(SchedulerMode::Slow).await?;
    env.wait_for_job_state(stranded, "planned").await?;

    env.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[cfg_attr(not(feature = "acceptance-tests"), ignore)]
async fn phase_3_live_transcode_deduplicates_and_caches() -> Result<(), Box<dyn std::error::Error>>
{
    let _guard = acceptance_lock().lock().await;
    let mut env = AcceptanceEnv::start("live-cache", SchedulerMode::Fast).await?;
    let source = env.generate_hdr10_source("live-cache.mkv", 10).await?;
    let (_, source_file_id) = env.seed_source_file(&source).await?;
    let profile = live_profile(source_file_id, 852);

    let live_path = format!("/api/v1/stream/live/{source_file_id}/{profile}/media.m3u8");
    let first = env.get_text(&live_path);
    let second = env.get_text(&live_path);
    let (first, second) = tokio::try_join!(first, second)?;
    assert_media_playlist(&first)?;
    assert_media_playlist(&second)?;

    env.wait_for_ephemeral_rows(source_file_id, 1).await?;
    let first_access = ephemeral_last_access(&env.db, source_file_id).await?;
    sleep(Duration::from_millis(20)).await;
    let cached = env
        .get_text(&format!(
            "/api/v1/stream/live/{source_file_id}/{profile}/media.m3u8"
        ))
        .await?;
    assert_media_playlist(&cached)?;
    let second_access = ephemeral_last_access(&env.db, source_file_id).await?;
    assert!(second_access >= first_access);

    env.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[cfg_attr(not(feature = "acceptance-tests"), ignore)]
async fn phase_3_hls_playlists_parse_for_each_variant() -> Result<(), Box<dyn std::error::Error>> {
    let _guard = acceptance_lock().lock().await;
    let mut env = AcceptanceEnv::start("hls-parse", SchedulerMode::Fast).await?;
    let source = env.generate_hdr10_source("hls-parse.mkv", 10).await?;
    let (media_item_id, source_file_id) = env.seed_source_file(&source).await?;

    env.replan(source_file_id).await?;
    env.wait_for_completed_jobs(source_file_id, 3).await?;

    let master = env.master_playlist(media_item_id).await?;
    let variants = parse_master_variants(&master)?;
    assert_eq!(variants.len(), 3);
    for uri in variants {
        let media = env.get_text(&uri).await?;
        assert_media_playlist(&media)?;
    }

    env.shutdown().await?;
    Ok(())
}

struct AcceptanceEnv {
    root: TempDir,
    db: kino_db::Db,
    config_path: PathBuf,
    listen: SocketAddr,
    client: Client,
    child: Child,
}

#[derive(Clone, Copy)]
enum SchedulerMode {
    Fast,
    Slow,
}

impl AcceptanceEnv {
    async fn start(
        name: &str,
        scheduler: SchedulerMode,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        require_ffmpeg().await?;
        let root = temp_under_kino_data(name)?;
        let library = root.path().join("library");
        let watch = root.path().join("watch");
        let cache = root.path().join("ephemeral");
        tokio::fs::create_dir_all(&library).await?;
        tokio::fs::create_dir_all(&watch).await?;
        tokio::fs::create_dir_all(&cache).await?;

        let db_path = root.path().join("kino.db");
        let listen = unused_loopback_addr()?;
        let config_path = root.path().join("kino.toml");
        write_config(
            &config_path,
            &db_path,
            &library,
            &watch,
            &cache,
            listen,
            scheduler,
        )
        .await?;

        let config = Config::load_from(&config_path)?;
        let db = kino_db::Db::open(&config).await?;
        seed_token(&db, ACCEPTANCE_TOKEN).await?;

        let client = Client::builder().timeout(Duration::from_secs(60)).build()?;
        let child = spawn_kino(&config_path).await?;
        let mut env = Self {
            root,
            db,
            config_path,
            listen,
            client,
            child,
        };
        env.wait_ready().await?;
        Ok(env)
    }

    async fn restart(
        &mut self,
        scheduler: SchedulerMode,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let library = self.root.path().join("library");
        let watch = self.root.path().join("watch");
        let cache = self.root.path().join("ephemeral");
        let db_path = self.root.path().join("kino.db");
        write_config(
            &self.config_path,
            &db_path,
            &library,
            &watch,
            &cache,
            self.listen,
            scheduler,
        )
        .await?;
        self.child = spawn_kino(&self.config_path).await?;
        self.wait_ready().await
    }

    async fn shutdown(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        if self.child.id().is_some() {
            self.child.start_kill()?;
            let _ = self.child.wait().await;
        }
        Ok(())
    }

    async fn wait_ready(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(status) = self.child.try_wait()? {
                return Err(format!("kino exited before becoming ready: {status}").into());
            }
            match self.client.get(self.url("/api/openapi.json")).send().await {
                Ok(response) if response.status().is_success() => return Ok(()),
                Ok(_) | Err(_) if Instant::now() < deadline => sleep(POLL_INTERVAL).await,
                Ok(response) => {
                    return Err(format!("server readiness returned {}", response.status()).into());
                }
                Err(err) => return Err(format!("server readiness failed: {err}").into()),
            }
        }
    }

    async fn generate_hdr10_source(
        &self,
        name: &str,
        seconds: u32,
    ) -> Result<PathBuf, Box<dyn std::error::Error>> {
        let path = self.root.path().join("watch").join(name);
        let status = tokio::process::Command::new("ffmpeg")
            .arg("-hide_banner")
            .arg("-loglevel")
            .arg("error")
            .arg("-y")
            .arg("-f")
            .arg("lavfi")
            .arg("-i")
            .arg(format!(
                "smptebars=size=320x180:rate=24:duration={seconds}"
            ))
            .arg("-f")
            .arg("lavfi")
            .arg("-i")
            .arg(format!("sine=frequency=1000:sample_rate=48000:duration={seconds}"))
            .arg("-vf")
            .arg("format=yuv420p10le")
            .arg("-c:v")
            .arg("libx265")
            .arg("-preset")
            .arg("ultrafast")
            .arg("-x265-params")
            .arg("hdr10=1:repeat-headers=1:colorprim=bt2020:transfer=smpte2084:colormatrix=bt2020nc:master-display=G(13250,34500)B(7500,3000)R(34000,16000)WP(15635,16450)L(10000000,1):max-cll=1000,400")
            .arg("-color_primaries")
            .arg("bt2020")
            .arg("-color_trc")
            .arg("smpte2084")
            .arg("-colorspace")
            .arg("bt2020nc")
            .arg("-c:a")
            .arg("aac")
            .arg("-shortest")
            .arg(&path)
            .status()
            .await?;
        if !status.success() {
            return Err(format!("ffmpeg HDR10 source generation failed with {status}").into());
        }
        Ok(path)
    }

    async fn seed_source_file(
        &self,
        source: &Path,
    ) -> Result<(Id, Id), Box<dyn std::error::Error>> {
        let media_item_id = Id::new();
        let source_file_id = Id::new();
        let now = Timestamp::now();
        sqlx::query(
            r#"
            INSERT INTO media_items (
                id,
                media_kind,
                canonical_identity_id,
                season_number,
                episode_number,
                created_at,
                updated_at
            )
            VALUES (?1, 'personal', NULL, NULL, NULL, ?2, ?3)
            "#,
        )
        .bind(media_item_id)
        .bind(now)
        .bind(now)
        .execute(self.db.write_pool())
        .await?;
        sqlx::query(
            r#"
            INSERT INTO source_files (
                id,
                media_item_id,
                path,
                created_at,
                updated_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5)
            "#,
        )
        .bind(source_file_id)
        .bind(media_item_id)
        .bind(source.display().to_string())
        .bind(now)
        .bind(now)
        .execute(self.db.write_pool())
        .await?;
        Ok((media_item_id, source_file_id))
    }

    async fn replan(&self, source_file_id: Id) -> Result<(), Box<dyn std::error::Error>> {
        self.post_json(&format!(
            "/api/v1/admin/transcodes/sources/{source_file_id}/replan"
        ))
        .await?;
        Ok(())
    }

    async fn wait_for_completed_jobs(
        &self,
        source_file_id: Id,
        count: usize,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.wait_until(|| async {
            let jobs = self.jobs_for_source(source_file_id).await?;
            Ok(jobs.len() == count && jobs.iter().all(|job| job["state"] == "completed"))
        })
        .await
    }

    async fn wait_for_state(
        &self,
        source_file_id: Id,
        state: &str,
    ) -> Result<Id, Box<dyn std::error::Error>> {
        let deadline = Instant::now() + TEST_TIMEOUT;
        loop {
            let jobs = self.jobs_for_source(source_file_id).await?;
            if let Some(id) = jobs
                .iter()
                .find(|job| job["state"] == state)
                .and_then(|job| job["id"].as_str())
            {
                return Ok(id.parse()?);
            }
            if Instant::now() >= deadline {
                return Err(format!("timed out waiting for {state} job").into());
            }
            sleep(POLL_INTERVAL).await;
        }
    }

    async fn wait_for_job_state(
        &self,
        job_id: Id,
        state: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.wait_until(|| async {
            let job = self
                .get_json(&format!("/api/v1/admin/transcodes/jobs/{job_id}"))
                .await?;
            Ok(job["state"] == state)
        })
        .await
    }

    async fn wait_for_other_started(
        &self,
        source_file_id: Id,
        cancelled: Id,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.wait_until(|| async {
            let jobs = self.jobs_for_source(source_file_id).await?;
            Ok(jobs.iter().any(|job| {
                job["id"] != cancelled.to_string()
                    && job["attempts"].as_u64().unwrap_or_default() > 0
                    && matches!(
                        job["state"].as_str(),
                        Some("running" | "verifying" | "completed")
                    )
            }))
        })
        .await
    }

    async fn wait_for_ephemeral_rows(
        &self,
        source_file_id: Id,
        count: i64,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.wait_until(|| async {
            let rows = ephemeral_count(&self.db, source_file_id).await?;
            Ok(rows == count)
        })
        .await
    }

    async fn wait_until<F, Fut>(&self, mut predicate: F) -> Result<(), Box<dyn std::error::Error>>
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = Result<bool, Box<dyn std::error::Error>>>,
    {
        let deadline = Instant::now() + TEST_TIMEOUT;
        loop {
            if predicate().await? {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err("timed out waiting for acceptance condition".into());
            }
            sleep(POLL_INTERVAL).await;
        }
    }

    async fn jobs_for_source(
        &self,
        source_file_id: Id,
    ) -> Result<Vec<Value>, Box<dyn std::error::Error>> {
        let value = self
            .get_json(&format!(
                "/api/v1/admin/transcodes/jobs?source_file_id={source_file_id}"
            ))
            .await?;
        Ok(value
            .as_array()
            .ok_or("jobs response must be an array")?
            .to_vec())
    }

    async fn master_playlist(
        &self,
        media_item_id: Id,
    ) -> Result<String, Box<dyn std::error::Error>> {
        self.get_text(&format!("/api/v1/stream/items/{media_item_id}/master.m3u8"))
            .await
    }

    async fn get_json(&self, path: &str) -> Result<Value, Box<dyn std::error::Error>> {
        let response = self
            .client
            .get(self.url(path))
            .bearer_auth(ACCEPTANCE_TOKEN)
            .send()
            .await?;
        let status = response.status();
        let body = response.text().await?;
        if !status.is_success() {
            return Err(format!("GET {path} returned {status}: {body}").into());
        }
        Ok(serde_json::from_str(&body)?)
    }

    async fn get_text(&self, path: &str) -> Result<String, Box<dyn std::error::Error>> {
        let response = self
            .client
            .get(self.url(path))
            .bearer_auth(ACCEPTANCE_TOKEN)
            .send()
            .await?;
        let status = response.status();
        let body = response.text().await?;
        if !status.is_success() {
            return Err(format!("GET {path} returned {status}: {body}").into());
        }
        Ok(body)
    }

    async fn post_json(&self, path: &str) -> Result<Value, Box<dyn std::error::Error>> {
        let response = self
            .client
            .post(self.url(path))
            .bearer_auth(ACCEPTANCE_TOKEN)
            .send()
            .await?;
        let status = response.status();
        let body = response.text().await?;
        if !status.is_success() {
            return Err(format!("POST {path} returned {status}: {body}").into());
        }
        if body.trim().is_empty() {
            return Ok(Value::Null);
        }
        Ok(serde_json::from_str(&body)?)
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{}", self.listen, path)
    }
}

impl Drop for AcceptanceEnv {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

fn acceptance_lock() -> &'static Mutex<()> {
    ACCEPTANCE_LOCK.get_or_init(|| Mutex::new(()))
}

fn temp_under_kino_data(name: &str) -> Result<TempDir, Box<dyn std::error::Error>> {
    let home = env::var_os("HOME").ok_or("HOME is required for acceptance temp data")?;
    let base = PathBuf::from(home).join("kino-data");
    std::fs::create_dir_all(&base)?;
    Ok(tempfile::Builder::new()
        .prefix(&format!("phase-3-{name}-"))
        .tempdir_in(base)?)
}

async fn write_config(
    path: &Path,
    db_path: &Path,
    library: &Path,
    watch: &Path,
    cache: &Path,
    listen: SocketAddr,
    scheduler: SchedulerMode,
) -> Result<(), Box<dyn std::error::Error>> {
    let tick_millis = match scheduler {
        SchedulerMode::Fast => 250,
        SchedulerMode::Slow => 5_000,
    };
    let config = format!(
        r#"
database_path = "{}"
library_root = "{}"
log_level = "info"
log_format = "pretty"

[library]
canonical_transfer = "hard_link"

[server]
listen = "{}"
public_base_url = "http://{}"

[providers.watch_folder]
path = "{}"
preference = 100
stability_seconds = 1

[transcode.scheduler]
tick_millis = {}
max_attempts = 1
backoff_seconds = 1
reserve_live_lane = "cpu"
recovery_on_boot = true

[transcode.ephemeral]
enabled = true
cache_root = "{}"
max_size_bytes = 1000000000
max_age_seconds = 3600
eviction_tick_seconds = 60
"#,
        toml_path(db_path),
        toml_path(library),
        listen,
        listen,
        toml_path(watch),
        tick_millis,
        toml_path(cache)
    );
    tokio::fs::write(path, config).await?;
    Ok(())
}

fn toml_path(path: &Path) -> String {
    path.display()
        .to_string()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
}

async fn spawn_kino(config_path: &Path) -> Result<Child, Box<dyn std::error::Error>> {
    let binary = kino_binary()?;
    let child = tokio::process::Command::new(binary)
        .env("KINO_CONFIG", config_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(child)
}

fn kino_binary() -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Some(path) = option_env!("CARGO_BIN_EXE_kino") {
        return Ok(PathBuf::from(path));
    }

    let exe = env::current_exe()?;
    let debug_dir = exe
        .parent()
        .and_then(Path::parent)
        .ok_or("could not derive target/debug from current test binary")?;
    let binary = debug_dir.join(if cfg!(windows) { "kino.exe" } else { "kino" });
    if binary.is_file() {
        return Ok(binary);
    }
    Err(format!(
        "kino binary not found at {}; run `cargo build -p kino` before acceptance tests",
        binary.display()
    )
    .into())
}

async fn require_ffmpeg() -> Result<(), Box<dyn std::error::Error>> {
    let status = tokio::process::Command::new("ffmpeg")
        .arg("-version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("ffmpeg -version failed with {status}").into())
    }
}

fn unused_loopback_addr() -> Result<SocketAddr, Box<dyn std::error::Error>> {
    let listener = TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))?;
    Ok(listener.local_addr()?)
}

async fn seed_token(db: &kino_db::Db, token: &str) -> Result<(), Box<dyn std::error::Error>> {
    let now = Timestamp::now();
    let hash = format!("{:x}", Sha256::digest(token.as_bytes()));
    sqlx::query(
        r#"
        INSERT INTO device_tokens (
            id,
            user_id,
            label,
            hash,
            last_seen_at,
            revoked_at,
            created_at
        )
        VALUES (?1, ?2, 'phase 3 acceptance', ?3, ?4, NULL, ?5)
        "#,
    )
    .bind(Id::new())
    .bind(kino_core::user::SEEDED_USER_ID)
    .bind(hash)
    .bind(now)
    .bind(now)
    .execute(db.write_pool())
    .await?;
    Ok(())
}

async fn insert_acceptance_job(
    db: &kino_db::Db,
    source_file_id: Id,
    seed: u8,
    state: &str,
    lane: &str,
    kind: &str,
) -> Result<Id, Box<dyn std::error::Error>> {
    let id = Id::new();
    let now = Timestamp::now();
    let codec = if kind == "high" { "hevc" } else { "h264" };
    let color = if kind == "high" { "hdr10" } else { "sdr" };
    let bit_depth = if kind == "high" { 10 } else { 8 };
    let profile = json!({
        "source_file_id": source_file_id,
        "kind": kind,
        "codec": codec,
        "container": "fmp4_cmaf",
        "width": 640,
        "bit_depth": bit_depth,
        "color": color,
        "audio": "stereo_aac",
        "vmaf_target": 90.0,
        "source_color_transfer": "smpte2084"
    });
    let profile_json = serde_json::to_string(&profile)?;
    let hash: [u8; 32] = Sha256::digest(format!("{profile_json}:{seed}").as_bytes()).into();
    let started_at = (state == "running").then_some(now);
    sqlx::query(
        r#"
        INSERT INTO transcode_jobs (
            id,
            source_file_id,
            profile_json,
            profile_hash,
            state,
            lane,
            attempt,
            progress_pct,
            last_error,
            next_attempt_at,
            created_at,
            updated_at,
            started_at,
            completed_at
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, NULL, NULL, ?8, ?9, ?10, NULL)
        "#,
    )
    .bind(id)
    .bind(source_file_id)
    .bind(profile_json)
    .bind(hash.as_slice())
    .bind(state)
    .bind(lane)
    .bind(if state == "running" { 1_i64 } else { 0_i64 })
    .bind(now)
    .bind(now)
    .bind(started_at)
    .execute(db.write_pool())
    .await?;
    Ok(id)
}

fn live_profile(source_file_id: Id, width: u32) -> String {
    let profile = json!({
        "source_file_id": source_file_id,
        "kind": "compatibility",
        "codec": "h264",
        "container": "fmp4_cmaf",
        "width": width,
        "bit_depth": 8,
        "color": "sdr",
        "audio": "stereo_aac",
        "vmaf_target": 90.0
    });
    URL_SAFE_NO_PAD.encode(profile.to_string())
}

fn assert_master_variants(
    playlist_text: &str,
    expected: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let variants = parse_master_variants(playlist_text)?;
    assert_eq!(variants.len(), expected);
    for required in ["BANDWIDTH=", "CODECS=", "RESOLUTION=", "VIDEO-RANGE="] {
        assert!(playlist_text.contains(required), "missing {required}");
    }
    Ok(())
}

fn parse_master_variants(playlist_text: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    match m3u8_rs::parse_master_playlist_res(playlist_text.as_bytes()) {
        Ok(playlist) => Ok(playlist
            .variants
            .into_iter()
            .map(|variant| variant.uri)
            .collect()),
        Err(error) => {
            Err(format!("master playlist parse failed: {error:?}\n{playlist_text}").into())
        }
    }
}

fn assert_media_playlist(playlist_text: &str) -> Result<(), Box<dyn std::error::Error>> {
    match m3u8_rs::parse_playlist_res(playlist_text.as_bytes()) {
        Ok(Playlist::MediaPlaylist(playlist)) => {
            assert!(playlist.end_list);
            assert!(!playlist.segments.is_empty());
            Ok(())
        }
        Ok(Playlist::MasterPlaylist(_)) => Err("expected media playlist, got master".into()),
        Err(error) => {
            Err(format!("media playlist parse failed: {error:?}\n{playlist_text}").into())
        }
    }
}

async fn transcode_job_count(
    db: &kino_db::Db,
    source_file_id: Id,
) -> Result<i64, Box<dyn std::error::Error>> {
    Ok(
        sqlx::query_scalar("SELECT COUNT(*) FROM transcode_jobs WHERE source_file_id = ?1")
            .bind(source_file_id)
            .fetch_one(db.read_pool())
            .await?,
    )
}

async fn transcode_output_count(
    db: &kino_db::Db,
    source_file_id: Id,
) -> Result<i64, Box<dyn std::error::Error>> {
    Ok(
        sqlx::query_scalar("SELECT COUNT(*) FROM transcode_outputs WHERE source_file_id = ?1")
            .bind(source_file_id)
            .fetch_one(db.read_pool())
            .await?,
    )
}

async fn downgrade_kinds(
    db: &kino_db::Db,
    source_file_id: Id,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    Ok(sqlx::query_scalar(
        r#"
        SELECT transcode_color_downgrades.kind
        FROM transcode_color_downgrades
        JOIN transcode_outputs
            ON transcode_outputs.id = transcode_color_downgrades.transcode_output_id
        WHERE transcode_outputs.source_file_id = ?1
        ORDER BY transcode_color_downgrades.kind
        "#,
    )
    .bind(source_file_id)
    .fetch_all(db.read_pool())
    .await?)
}

async fn ephemeral_count(
    db: &kino_db::Db,
    source_file_id: Id,
) -> Result<i64, Box<dyn std::error::Error>> {
    Ok(
        sqlx::query_scalar("SELECT COUNT(*) FROM ephemeral_transcodes WHERE source_file_id = ?1")
            .bind(source_file_id)
            .fetch_one(db.read_pool())
            .await?,
    )
}

async fn ephemeral_last_access(
    db: &kino_db::Db,
    source_file_id: Id,
) -> Result<Timestamp, Box<dyn std::error::Error>> {
    Ok(sqlx::query_scalar(
        "SELECT last_access_at FROM ephemeral_transcodes WHERE source_file_id = ?1",
    )
    .bind(source_file_id)
    .fetch_one(db.read_pool())
    .await?)
}
