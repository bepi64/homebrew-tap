//! End-to-end OCR tests for `fetch_local_docs` (macOS Apple Silicon with `--features ocr`).
#![cfg(fast_docs_ocr)]

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use fast_docs::cache::{DiskCache, DEFAULT_CACHE_TTL, PINNED_CACHE_TTL};
use fast_docs::local_docs::search_local_docs;
use fast_docs::ocr::{ocr_cache_key, tesseract_version, DEFAULT_OCR_LANG, MAX_OCR_IMAGES};
use image::{GrayImage, Luma};

fn unique_dir(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    std::env::temp_dir().join(format!(
        "fast-docs-ocr-it-{label}-{}-{nanos}",
        std::process::id()
    ))
}

fn fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ocr/ledger.png")
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFF_u32;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

/// A valid PNG signature and IHDR announcing `width` x `height`, with no pixel data.
fn png_header_only(width: u32, height: u32) -> Vec<u8> {
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = b"IHDR".to_vec();
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 0, 0, 0, 0]);
    out.extend_from_slice(&13_u32.to_be_bytes());
    out.extend_from_slice(&ihdr);
    out.extend_from_slice(&crc32(&ihdr).to_be_bytes());
    out.extend_from_slice(&0_u32.to_be_bytes());
    out.extend_from_slice(b"IEND");
    out.extend_from_slice(&crc32(b"IEND").to_be_bytes());
    out
}

fn ocr_titles(text: &str) -> usize {
    text.lines()
        .filter(|line| line.starts_with("TITLE: ") && line.ends_with("(OCR)"))
        .count()
}

#[test]
fn ocr_text_from_png_is_found_by_topic() {
    let dir = unique_dir("topic");
    assert!(fs::create_dir_all(dir.join("docs")).is_ok());
    assert!(fs::write(dir.join("README.md"), "# Overview\nGeneral notes.\n").is_ok());
    assert!(fs::copy(fixture_path(), dir.join("docs/ledger.png")).is_ok());

    let out = search_local_docs(&dir, Some("quantum ledger"), 4000, 0, None);
    assert!(out.is_ok(), "{out:?}");
    if let Ok(text) = out {
        assert!(text.contains("TITLE: docs/ledger.png (OCR)"), "{text}");
        assert!(text.contains("Reconciliation"), "{text}");
    }
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn corrupt_oversized_blank_and_icon_images_are_skipped() {
    let dir = unique_dir("rejects");
    assert!(fs::create_dir_all(dir.join("docs")).is_ok());
    assert!(fs::write(dir.join("README.md"), "# Overview\nGeneral notes.\n").is_ok());

    let fixture = fs::read(fixture_path());
    assert!(fixture.is_ok());
    if let Ok(bytes) = fixture {
        assert!(fs::write(dir.join("docs/corrupt.png"), &bytes[..64]).is_ok());
    }
    assert!(fs::write(dir.join("docs/huge.png"), png_header_only(100_000, 100_000)).is_ok());
    assert!(GrayImage::from_pixel(400, 100, Luma([255]))
        .save(dir.join("docs/blank.png"))
        .is_ok());
    assert!(GrayImage::from_pixel(16, 16, Luma([0]))
        .save(dir.join("docs/icon.png"))
        .is_ok());

    let out = search_local_docs(&dir, None, 4000, 0, None);
    assert!(out.is_ok(), "{out:?}");
    if let Ok(text) = out {
        assert!(text.contains("Overview"), "{text}");
        assert_eq!(ocr_titles(&text), 0, "{text}");
    }
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn at_most_sixteen_images_are_processed_per_call() {
    let dir = unique_dir("cap");
    assert!(fs::create_dir_all(dir.join("shots")).is_ok());
    for index in 0..20 {
        assert!(fs::copy(
            fixture_path(),
            dir.join(format!("shots/shot-{index:02}.png"))
        )
        .is_ok());
    }

    let out = search_local_docs(&dir, None, 20_000, 0, None);
    assert!(out.is_ok(), "{out:?}");
    if let Ok(text) = out {
        assert_eq!(ocr_titles(&text), MAX_OCR_IMAGES, "{text}");
    }
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn cached_ocr_text_is_served_without_running_tesseract() {
    let dir = unique_dir("cache-read");
    let cache_root = unique_dir("cache-read-store");
    assert!(fs::create_dir_all(&dir).is_ok());
    assert!(fs::copy(fixture_path(), dir.join("ledger.png")).is_ok());
    let cache = DiskCache::new(cache_root.clone(), DEFAULT_CACHE_TTL, PINNED_CACHE_TTL);

    let fixture = fs::read(fixture_path());
    assert!(fixture.is_ok());
    if let Ok(bytes) = fixture {
        let key = ocr_cache_key(
            &bytes,
            DEFAULT_OCR_LANG,
            &tesseract_version().unwrap_or_default(),
        );
        assert!(cache.put(&key, "Cached Marker Sentence", true).is_ok());
    }

    let out = search_local_docs(&dir, Some("cached marker"), 4000, 0, Some(&cache));
    assert!(out.is_ok(), "{out:?}");
    if let Ok(text) = out {
        assert!(text.contains("Cached Marker Sentence"), "{text}");
        assert!(!text.contains("Reconciliation"), "{text}");
    }
    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_dir_all(&cache_root);
}

#[test]
fn ocr_text_is_written_to_the_cache() {
    let dir = unique_dir("cache-write");
    let cache_root = unique_dir("cache-write-store");
    assert!(fs::create_dir_all(&dir).is_ok());
    assert!(fs::copy(fixture_path(), dir.join("ledger.png")).is_ok());
    let cache = DiskCache::new(cache_root.clone(), DEFAULT_CACHE_TTL, PINNED_CACHE_TTL);

    assert!(search_local_docs(&dir, None, 4000, 0, Some(&cache)).is_ok());

    let fixture = fs::read(fixture_path());
    assert!(fixture.is_ok());
    if let Ok(bytes) = fixture {
        let key = ocr_cache_key(
            &bytes,
            DEFAULT_OCR_LANG,
            &tesseract_version().unwrap_or_default(),
        );
        let cached = cache.get(&key, true);
        assert!(
            cached
                .as_deref()
                .is_some_and(|text| text.contains("Quantum Ledger Reconciliation")),
            "{cached:?}"
        );
    }
    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_dir_all(&cache_root);
}
