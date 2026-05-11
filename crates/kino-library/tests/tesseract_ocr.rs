use std::{path::Path, process::Command};

use image::{Rgb, RgbImage};
use kino_library::{
    subtitle_image_extraction::{
        ImageSubtitleExtraction, ImageSubtitleExtractionFuture, ImageSubtitleExtractionInput,
        ImageSubtitleFrame, ProbeSubtitleKind,
    },
    subtitle_ocr::{TesseractOcrEngine, ocr_subtitle_track},
};

/// Runs the real Tesseract binary against a generated PNG; skipped unless run
/// explicitly because local and CI environments may not have Tesseract on PATH.
#[tokio::test]
#[ignore = "requires tesseract on PATH"]
async fn tesseract_reads_generated_png_text() -> Result<(), Box<dyn std::error::Error>> {
    if Command::new("tesseract").arg("--version").output().is_err() {
        return Ok(());
    }

    let dir = tempfile::tempdir()?;
    let image_path = dir.path().join("hel.png");
    write_text_png(&image_path, "HEL")?;

    let extraction = GeneratedPngExtraction {
        image_path: image_path.clone(),
    };
    let frames = extraction
        .extract_image_subtitle_track(ImageSubtitleExtractionInput::new(
            "movie.mkv",
            "source-sha256",
            0,
            ProbeSubtitleKind::ImagePgs,
        ))
        .await?;
    let engine = TesseractOcrEngine::new("eng", "tesseract");
    let cues = ocr_subtitle_track(&engine, &frames).await?;

    assert!(!cues[0].text.trim().is_empty());
    assert!(
        cues[0].text.to_ascii_uppercase().contains("HEL"),
        "recognized text: {:?}",
        cues[0].text
    );
    Ok(())
}

struct GeneratedPngExtraction {
    image_path: std::path::PathBuf,
}

impl ImageSubtitleExtraction for GeneratedPngExtraction {
    fn extract_image_subtitle_track<'a>(
        &'a self,
        _input: ImageSubtitleExtractionInput,
    ) -> ImageSubtitleExtractionFuture<'a> {
        Box::pin(async move {
            Ok(vec![ImageSubtitleFrame::new(
                std::time::Duration::from_secs(1),
                std::time::Duration::from_secs(2),
                self.image_path.clone(),
            )])
        })
    }
}

fn write_text_png(path: &Path, text: &str) -> Result<(), image::ImageError> {
    let scale = 12_u32;
    let glyph_width = 5_u32;
    let glyph_height = 7_u32;
    let gap = 2_u32;
    let margin = 24_u32;
    let width = margin * 2 + text.chars().count() as u32 * (glyph_width + gap) * scale;
    let height = margin * 2 + glyph_height * scale;
    let mut image = RgbImage::from_pixel(width, height, Rgb([255, 255, 255]));

    let mut x = margin;
    for ch in text.chars() {
        if let Some(pattern) = glyph(ch) {
            draw_glyph(&mut image, x, margin, scale, pattern);
        }
        x += (glyph_width + gap) * scale;
    }

    image.save(path)
}

fn draw_glyph(image: &mut RgbImage, x: u32, y: u32, scale: u32, pattern: [&str; 7]) {
    for (row, bits) in pattern.iter().enumerate() {
        for (col, bit) in bits.chars().enumerate() {
            if bit != '1' {
                continue;
            }

            let base_x = x + col as u32 * scale;
            let base_y = y + row as u32 * scale;
            for dy in 0..scale {
                for dx in 0..scale {
                    image.put_pixel(base_x + dx, base_y + dy, Rgb([0, 0, 0]));
                }
            }
        }
    }
}

fn glyph(ch: char) -> Option<[&'static str; 7]> {
    match ch {
        'E' => Some([
            "11111", "10000", "10000", "11110", "10000", "10000", "11111",
        ]),
        'H' => Some([
            "10001", "10001", "10001", "11111", "10001", "10001", "10001",
        ]),
        'L' => Some([
            "10000", "10000", "10000", "10000", "10000", "10000", "11111",
        ]),
        'O' => Some([
            "01110", "10001", "10001", "10001", "10001", "10001", "01110",
        ]),
        _ => None,
    }
}
