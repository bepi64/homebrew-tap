//! Optional Tesseract OCR for images found by `fetch_local_docs`.
//!
//! The engine is compiled only on macOS Apple Silicon with `--features ocr`, signalled by the
//! `fast_docs_ocr` cfg that `build.rs` emits. All `unsafe` code of the crate lives in this module.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::extractors::MAX_EXTRACTED_BYTES;

/// Language model loaded by default (`eng.traineddata`).
pub const DEFAULT_OCR_LANG: &str = "eng";

/// Homebrew's `tesseract` language data, used for local development builds.
pub const DEV_TESSDATA_DIR: &str = "/opt/homebrew/opt/tesseract/share/tessdata";

/// Maximum number of images sent to OCR per `fetch_local_docs` call.
pub const MAX_OCR_IMAGES: usize = 16;

/// Images with a side shorter than this (icons, bullets) are skipped.
pub const MIN_OCR_SIDE_PX: u32 = 32;

/// Maximum accepted width or height, checked from the header before decoding.
pub const MAX_OCR_SIDE_PX: u32 = 8192;

/// Maximum accepted pixel count (16 Mi px), checked from the header before decoding.
pub const MAX_OCR_PIXELS: u64 = 16 * 1024 * 1024;

/// Decoder allocation cap: `MAX_OCR_PIXELS` at 4 bytes per pixel (8-bit RGBA).
pub const MAX_OCR_DECODE_ALLOC_BYTES: u64 = MAX_OCR_PIXELS * 4;

/// Results below this mean word confidence are noise (logos, diagrams without text).
pub const MIN_OCR_CONFIDENCE: i32 = 60;

/// Source resolution passed to Tesseract; avoids its "0 dpi" estimation warning.
pub const OCR_SOURCE_DPI: i32 = 70;

/// Image extensions handed to OCR when the binary is built with OCR support.
pub const OCR_IMAGE_EXTENSIONS: [&str; 7] = ["png", "jpg", "jpeg", "webp", "tif", "tiff", "gif"];

/// Whether this binary was built with the Tesseract OCR engine.
pub const OCR_ENABLED: bool = cfg!(fast_docs_ocr);

/// Outcome of the header-level size check performed before decoding an image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DimensionCheck {
    Accept,
    TooSmall,
    TooLarge,
}

/// Whether `path` is an image that `fetch_local_docs` should OCR in this build.
#[must_use]
pub fn is_ocr_image_file(path: &Path) -> bool {
    OCR_ENABLED
        && path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| {
                OCR_IMAGE_EXTENSIONS
                    .iter()
                    .any(|allowed| ext.eq_ignore_ascii_case(allowed))
            })
}

/// Validate a Tesseract language spec such as `eng` or `eng+fra` before it reaches the C API.
///
/// # Errors
/// Returns an error when `lang` is empty or contains anything other than ASCII letters, digits,
/// and underscores, optionally joined by single `+` separators.
pub fn validate_lang(lang: &str) -> Result<(), String> {
    let valid = lang.split('+').all(|part| {
        !part.is_empty()
            && part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    });
    if valid {
        Ok(())
    } else {
        Err(format!("invalid Tesseract language '{lang}'"))
    }
}

/// Classify image dimensions read from the file header.
#[must_use]
pub fn check_dimensions(width: u32, height: u32) -> DimensionCheck {
    if width > MAX_OCR_SIDE_PX
        || height > MAX_OCR_SIDE_PX
        || u64::from(width) * u64::from(height) > MAX_OCR_PIXELS
    {
        DimensionCheck::TooLarge
    } else if width < MIN_OCR_SIDE_PX || height < MIN_OCR_SIDE_PX {
        DimensionCheck::TooSmall
    } else {
        DimensionCheck::Accept
    }
}

/// Whether a Tesseract mean word confidence is high enough to keep the text.
#[must_use]
pub const fn passes_confidence(confidence: i32) -> bool {
    confidence >= MIN_OCR_CONFIDENCE
}

/// Truncate OCR output to `MAX_EXTRACTED_BYTES` on a UTF-8 character boundary.
#[must_use]
pub fn bound_ocr_text(text: &str) -> String {
    text[..text.floor_char_boundary(MAX_EXTRACTED_BYTES)].to_string()
}

/// Cache key for OCR text: image content hash, language, and Tesseract version.
#[must_use]
pub fn ocr_cache_key(content: &[u8], lang: &str, version: &str) -> String {
    format!(
        "ocr:{}:{lang}:{version}",
        hex::encode(Sha256::digest(content))
    )
}

/// Pick the Tesseract language data directory: explicit override, then the Homebrew
/// `libexec/tessdata` next to the executable's `bin/`, then the development fallback.
#[must_use]
pub fn resolve_tessdata_dir(
    env_override: Option<PathBuf>,
    exe_path: Option<&Path>,
    dev_fallback: &Path,
) -> Option<PathBuf> {
    if let Some(dir) = env_override {
        return Some(dir);
    }
    if let Some(prefix) = exe_path.and_then(Path::parent).and_then(Path::parent) {
        let candidate = prefix.join("libexec").join("tessdata");
        if candidate.is_dir() {
            return Some(candidate);
        }
    }
    dev_fallback.is_dir().then(|| dev_fallback.to_path_buf())
}

/// Resolve the language data directory from `FAST_DOCS_TESSDATA_DIR` and the running executable.
///
/// The executable path is canonicalized because Homebrew launches `fast-docs` through the
/// `/opt/homebrew/bin` symlink, and only the resolved Cellar path has a sibling `libexec/`.
#[must_use]
pub fn default_tessdata_dir() -> Option<PathBuf> {
    let env_override = env::var_os("FAST_DOCS_TESSDATA_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    let exe_path = env::current_exe()
        .ok()
        .and_then(|exe| fs::canonicalize(exe).ok());
    resolve_tessdata_dir(
        env_override,
        exe_path.as_deref(),
        Path::new(DEV_TESSDATA_DIR),
    )
}

/// Tesseract library version, or `None` when the binary is built without OCR.
#[cfg(not(fast_docs_ocr))]
#[must_use]
pub fn tesseract_version() -> Option<String> {
    None
}

#[cfg(fast_docs_ocr)]
pub use engine::tesseract_version;

/// JSON value for the `ocr` field of `GET /health`.
#[cfg(not(fast_docs_ocr))]
#[must_use]
pub fn health_json() -> String {
    "null".to_string()
}

/// JSON value for the `ocr` field of `GET /health`.
#[cfg(fast_docs_ocr)]
#[must_use]
pub fn health_json() -> String {
    let lang_file = format!("{DEFAULT_OCR_LANG}.traineddata");
    let tessdata = if default_tessdata_dir().is_some_and(|dir| dir.join(&lang_file).is_file()) {
        "ok"
    } else {
        "missing"
    };
    serde_json::json!({
        "engine": "tesseract",
        "version": tesseract_version(),
        "lang": DEFAULT_OCR_LANG,
        "tessdata": tessdata,
    })
    .to_string()
}

#[cfg(fast_docs_ocr)]
mod engine {
    use std::ffi::CStr;

    mod ffi {
        use std::ffi::c_char;

        unsafe extern "C" {
            pub fn TessVersion() -> *const c_char;
        }
    }

    /// Version string reported by the linked Tesseract library (for example `5.5.5`).
    #[must_use]
    pub fn tesseract_version() -> Option<String> {
        // SAFETY: `TessVersion` takes no arguments and returns null or a pointer to a static,
        // NUL-terminated string owned by the library for the whole process lifetime.
        let raw = unsafe { ffi::TessVersion() };
        if raw.is_null() {
            return None;
        }
        // SAFETY: `raw` is non-null and points to the static NUL-terminated string described above.
        let version = unsafe { CStr::from_ptr(raw) };
        Some(version.to_string_lossy().into_owned())
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{
        bound_ocr_text, check_dimensions, is_ocr_image_file, ocr_cache_key, passes_confidence,
        resolve_tessdata_dir, validate_lang, DimensionCheck, OCR_ENABLED,
    };
    use crate::extractors::MAX_EXTRACTED_BYTES;

    fn unique_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        std::env::temp_dir().join(format!(
            "fast-docs-ocr-{label}-{}-{nanos}",
            std::process::id()
        ))
    }

    #[test]
    fn resolve_tessdata_prefers_env_override() {
        let env_dir = PathBuf::from("/nonexistent/override");
        let exe = PathBuf::from("/nonexistent/bin/fast-docs");
        assert_eq!(
            resolve_tessdata_dir(
                Some(env_dir.clone()),
                Some(&exe),
                Path::new("/nonexistent/dev")
            ),
            Some(env_dir)
        );
    }

    #[test]
    fn resolve_tessdata_uses_libexec_next_to_bin() {
        let prefix = unique_dir("prefix");
        let tessdata = prefix.join("libexec").join("tessdata");
        assert!(fs::create_dir_all(&tessdata).is_ok());
        let exe = prefix.join("bin").join("fast-docs");
        assert_eq!(
            resolve_tessdata_dir(None, Some(&exe), Path::new("/nonexistent/dev")),
            Some(tessdata)
        );
        let _ = fs::remove_dir_all(&prefix);
    }

    #[test]
    fn resolve_tessdata_falls_back_to_dev_dir_then_none() {
        let dev = unique_dir("dev");
        assert!(fs::create_dir_all(&dev).is_ok());
        let exe = PathBuf::from("/nonexistent/bin/fast-docs");
        assert_eq!(
            resolve_tessdata_dir(None, Some(&exe), &dev),
            Some(dev.clone())
        );
        assert_eq!(
            resolve_tessdata_dir(None, Some(&exe), Path::new("/nonexistent/dev")),
            None
        );
        assert_eq!(
            resolve_tessdata_dir(None, None, Path::new("/nonexistent/dev")),
            None
        );
        let _ = fs::remove_dir_all(&dev);
    }

    #[test]
    fn validate_lang_accepts_tesseract_codes_and_combinations() {
        for lang in ["eng", "chi_sim", "eng+fra", "deu+eng+chi_tra"] {
            assert!(validate_lang(lang).is_ok(), "{lang} should be accepted");
        }
    }

    #[test]
    fn validate_lang_rejects_traversal_separators_and_empty_parts() {
        for lang in [
            "",
            "../eng",
            "a/b",
            "eng+",
            "+eng",
            "eng++fra",
            "eng\0",
            "en g",
            "eng.traineddata",
        ] {
            assert!(validate_lang(lang).is_err(), "{lang:?} should be rejected");
        }
    }

    #[test]
    fn check_dimensions_enforces_exact_bounds() {
        assert_eq!(check_dimensions(32, 32), DimensionCheck::Accept);
        assert_eq!(check_dimensions(8192, 2048), DimensionCheck::Accept);
        assert_eq!(check_dimensions(4096, 4096), DimensionCheck::Accept);
        assert_eq!(check_dimensions(31, 500), DimensionCheck::TooSmall);
        assert_eq!(check_dimensions(500, 31), DimensionCheck::TooSmall);
        assert_eq!(check_dimensions(8193, 32), DimensionCheck::TooLarge);
        assert_eq!(check_dimensions(32, 8193), DimensionCheck::TooLarge);
        assert_eq!(check_dimensions(8192, 2049), DimensionCheck::TooLarge);
        assert_eq!(check_dimensions(4097, 4096), DimensionCheck::TooLarge);
        assert_eq!(check_dimensions(100_000, 10), DimensionCheck::TooLarge);
    }

    #[test]
    fn passes_confidence_keeps_sixty_and_above() {
        assert!(!passes_confidence(-1));
        assert!(!passes_confidence(0));
        assert!(!passes_confidence(59));
        assert!(passes_confidence(60));
        assert!(passes_confidence(95));
    }

    #[test]
    fn bound_ocr_text_truncates_on_utf8_boundary() {
        assert_eq!(bound_ocr_text("short text"), "short text");
        let mut long = "a".repeat(MAX_EXTRACTED_BYTES - 1);
        long.push('é');
        let bounded = bound_ocr_text(&long);
        assert_eq!(bounded.len(), MAX_EXTRACTED_BYTES - 1);
        assert!(bounded.chars().all(|c| c == 'a'));
    }

    #[test]
    fn ocr_cache_key_hashes_content_and_includes_lang_and_version() {
        assert_eq!(
            ocr_cache_key(b"", "eng", "5.5.5"),
            "ocr:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855:eng:5.5.5"
        );
        let base = ocr_cache_key(b"png-bytes", "eng", "5.5.5");
        assert_eq!(base, ocr_cache_key(b"png-bytes", "eng", "5.5.5"));
        assert_ne!(base, ocr_cache_key(b"other-bytes", "eng", "5.5.5"));
        assert_ne!(base, ocr_cache_key(b"png-bytes", "fra", "5.5.5"));
        assert_ne!(base, ocr_cache_key(b"png-bytes", "eng", "5.5.6"));
    }

    #[test]
    fn is_ocr_image_file_matches_extensions_only_when_enabled() {
        assert_eq!(is_ocr_image_file(Path::new("docs/shot.PNG")), OCR_ENABLED);
        assert_eq!(is_ocr_image_file(Path::new("scan.jpeg")), OCR_ENABLED);
        assert_eq!(is_ocr_image_file(Path::new("figure.tif")), OCR_ENABLED);
        assert!(!is_ocr_image_file(Path::new("diagram.svg")));
        assert!(!is_ocr_image_file(Path::new("README.md")));
        assert!(!is_ocr_image_file(Path::new("png")));
    }
}
