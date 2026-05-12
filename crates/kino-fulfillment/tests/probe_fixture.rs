use std::{io, path::PathBuf, time::Duration};

use kino_core::{FfprobeFileProbe, ProbeError, ProbeSubtitleKind};
use kino_fulfillment::ProbedFile;

#[tokio::test]
async fn real_ffprobe_reads_mkv_fixture() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("probe_sample.mkv");
    let result = match FfprobeFileProbe::new().probe(&fixture).await {
        Ok(result) => result,
        Err(ProbeError::Spawn { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
            eprintln!("skipping real ffprobe fixture test because ffprobe is unavailable");
            return Ok(());
        }
        Err(error) => return Err(Box::<dyn std::error::Error>::from(error)),
    };

    let Some(container) = &result.container else {
        panic!("expected fixture container metadata");
    };
    assert!(
        container
            .format_names
            .iter()
            .any(|format| format == "matroska"),
        "got: {:?}",
        container.format_names
    );
    assert_eq!(
        container.format_long_name.as_deref(),
        Some("Matroska / WebM")
    );
    assert_eq!(result.title.as_deref(), Some("Kino Probe Fixture"));
    assert_eq!(result.duration, Some(Duration::from_secs(1)));

    assert_eq!(result.video_streams.len(), 1);
    assert_eq!(result.video_streams[0].index, 0);
    assert_eq!(result.video_streams[0].codec_name.as_deref(), Some("ffv1"));
    assert_eq!(result.video_streams[0].width, Some(16));
    assert_eq!(result.video_streams[0].height, Some(16));

    assert_eq!(result.audio_streams.len(), 1);
    assert_eq!(result.audio_streams[0].index, 1);
    assert_eq!(result.audio_streams[0].codec_name.as_deref(), Some("flac"));
    assert_eq!(result.audio_streams[0].channels, Some(1));
    assert_eq!(result.audio_streams[0].language.as_deref(), Some("eng"));

    assert_eq!(result.subtitle_streams.len(), 1);
    assert_eq!(result.subtitle_streams[0].index, 2);
    assert_eq!(
        result.subtitle_streams[0].codec_name.as_deref(),
        Some("subrip")
    );
    assert_eq!(result.subtitle_streams[0].kind, ProbeSubtitleKind::Srt);
    assert!(result.subtitle_streams[0].kind.is_text());
    assert_eq!(result.subtitle_streams[0].language.as_deref(), Some("eng"));

    assert_eq!(
        ProbedFile::from_probe_result(&result),
        ProbedFile::new()
            .with_title("Kino Probe Fixture")
            .with_duration_seconds(1)
            .with_audio_languages(["eng"])
            .with_subtitle_languages(["eng"])
    );

    Ok(())
}
