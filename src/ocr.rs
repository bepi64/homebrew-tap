//! Optional Tesseract OCR for images found by `fetch_local_docs`.
//!
//! The engine is compiled only on macOS Apple Silicon with `--features ocr`, signalled by the
//! `fast_docs_ocr` cfg that `build.rs` emits. All `unsafe` code of the crate lives in this module.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

/// Language model loaded by default (`eng.traineddata`).
pub const DEFAULT_OCR_LANG: &str = "eng";

/// Homebrew's `tesseract` language data, used for local development builds.
pub const DEV_TESSDATA_DIR: &str = "/opt/homebrew/opt/tesseract/share/tessdata";

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

    use super::resolve_tessdata_dir;

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
}
