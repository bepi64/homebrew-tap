//! Optional Tesseract OCR for images found by `fetch_local_docs`.
//!
//! The engine is compiled only on macOS Apple Silicon with `--features ocr`, signalled by the
//! `fast_docs_ocr` cfg that `build.rs` emits. All `unsafe` code of the crate lives in this module.

use std::env;
use std::ffi::OsString;
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

/// Interpret a raw `FAST_DOCS_TESSDATA_DIR` value: unset or empty means no override.
#[must_use]
pub fn tessdata_override(raw: Option<OsString>) -> Option<PathBuf> {
    raw.filter(|value| !value.is_empty()).map(PathBuf::from)
}

/// Resolve the language data directory from `FAST_DOCS_TESSDATA_DIR` and the running executable.
///
/// The executable path is canonicalized because Homebrew launches `fast-docs` through the
/// `/opt/homebrew/bin` symlink, and only the resolved Cellar path has a sibling `libexec/`.
#[must_use]
pub fn default_tessdata_dir() -> Option<PathBuf> {
    let env_override = tessdata_override(env::var_os("FAST_DOCS_TESSDATA_DIR"));
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
    use std::ffi::{c_char, c_int, CStr, CString};
    use std::fs;
    use std::io::Cursor;
    use std::os::unix::ffi::OsStrExt as _;
    use std::path::{Path, PathBuf};
    use std::ptr::NonNull;
    use std::sync::Once;

    use image::{GrayImage, ImageReader, Limits};

    use super::{
        bound_ocr_text, check_dimensions, default_tessdata_dir, ocr_cache_key, passes_confidence,
        validate_lang, DimensionCheck, DEFAULT_OCR_LANG, MAX_OCR_DECODE_ALLOC_BYTES,
        MAX_OCR_SIDE_PX, OCR_SOURCE_DPI,
    };
    use crate::cache::DiskCache;
    use crate::extractors::{lines_to_sections, MAX_RICH_DOC_FILE_BYTES};
    use crate::tools::DocSection;

    mod ffi {
        use std::ffi::{c_char, c_int};

        /// Opaque `TessBaseAPI` handle from Tesseract's C API (`tesseract/capi.h`).
        #[repr(C)]
        pub struct TessBaseAPI {
            _private: [u8; 0],
        }

        unsafe extern "C" {
            pub fn TessVersion() -> *const c_char;
            pub fn TessBaseAPICreate() -> *mut TessBaseAPI;
            pub fn TessBaseAPIInit3(
                handle: *mut TessBaseAPI,
                datapath: *const c_char,
                language: *const c_char,
            ) -> c_int;
            pub fn TessBaseAPISetImage(
                handle: *mut TessBaseAPI,
                imagedata: *const u8,
                width: c_int,
                height: c_int,
                bytes_per_pixel: c_int,
                bytes_per_line: c_int,
            );
            pub fn TessBaseAPISetSourceResolution(handle: *mut TessBaseAPI, ppi: c_int);
            pub fn TessBaseAPIGetUTF8Text(handle: *mut TessBaseAPI) -> *mut c_char;
            pub fn TessBaseAPIMeanTextConf(handle: *mut TessBaseAPI) -> c_int;
            pub fn TessDeleteText(text: *const c_char);
            pub fn TessBaseAPIEnd(handle: *mut TessBaseAPI);
            pub fn TessBaseAPIDelete(handle: *mut TessBaseAPI);
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

    /// Owned, initialized Tesseract handle. Not `Send`/`Sync` (`NonNull`), so each scan owns one.
    pub(super) struct TessHandle(NonNull<ffi::TessBaseAPI>);

    impl TessHandle {
        /// Create a handle and load `<tessdata>/<lang>.traineddata`.
        pub(super) fn new(tessdata: &Path, lang: &str) -> Result<Self, String> {
            validate_lang(lang)?;
            let datapath = CString::new(tessdata.as_os_str().as_bytes())
                .map_err(|_| "Tesseract data path contains a NUL byte".to_string())?;
            let language = CString::new(lang)
                .map_err(|_| "Tesseract language contains a NUL byte".to_string())?;

            // SAFETY: `TessBaseAPICreate` has no preconditions and returns an owned handle or null.
            let raw = unsafe { ffi::TessBaseAPICreate() };
            let handle = NonNull::new(raw)
                .map(Self)
                .ok_or_else(|| "TessBaseAPICreate returned null".to_string())?;

            // SAFETY: `handle.0` is the live handle created above. `datapath` and `language` are
            // NUL-terminated and outlive the call; Tesseract copies them during initialization.
            let rc = unsafe {
                ffi::TessBaseAPIInit3(handle.0.as_ptr(), datapath.as_ptr(), language.as_ptr())
            };
            if rc != 0 {
                return Err(format!(
                    "Tesseract could not load language '{lang}' from {} (code {rc})",
                    tessdata.display()
                ));
            }
            Ok(handle)
        }

        /// Recognize an 8-bit grayscale image, returning the text and its mean word confidence.
        pub(super) fn recognize(&mut self, image: &GrayImage) -> Result<(String, i32), String> {
            let width = c_int::try_from(image.width())
                .map_err(|_| "image width exceeds the C int range".to_string())?;
            let height = c_int::try_from(image.height())
                .map_err(|_| "image height exceeds the C int range".to_string())?;
            let pixels = image.as_raw();

            // SAFETY: `self.0` is a live, initialized handle (invariant of `TessHandle`). `pixels`
            // holds exactly `width * height` bytes (1 byte per pixel, rows of `width` bytes) and
            // stays borrowed for this whole function. Tesseract copies the buffer into its own
            // image (`baseapi.h`: "Copies the image buffer").
            unsafe {
                ffi::TessBaseAPISetImage(self.0.as_ptr(), pixels.as_ptr(), width, height, 1, width);
            }
            // SAFETY: same live handle; setting the resolution has no other precondition.
            unsafe { ffi::TessBaseAPISetSourceResolution(self.0.as_ptr(), OCR_SOURCE_DPI) };
            // SAFETY: live handle with an image set. Returns null or a NUL-terminated string owned
            // by the caller, released by `TessText::drop` through `TessDeleteText`.
            let raw = unsafe { ffi::TessBaseAPIGetUTF8Text(self.0.as_ptr()) };
            let text = match NonNull::new(raw) {
                Some(ptr) => TessText(ptr).to_string_lossy(),
                None => String::new(),
            };
            // SAFETY: live handle; recognition already ran during `TessBaseAPIGetUTF8Text`.
            let confidence = unsafe { ffi::TessBaseAPIMeanTextConf(self.0.as_ptr()) };
            Ok((text, confidence))
        }
    }

    impl Drop for TessHandle {
        fn drop(&mut self) {
            // SAFETY: `self.0` was returned by `TessBaseAPICreate` and is released exactly once
            // here. `TessBaseAPIEnd` is valid even when `TessBaseAPIInit3` failed.
            unsafe {
                ffi::TessBaseAPIEnd(self.0.as_ptr());
                ffi::TessBaseAPIDelete(self.0.as_ptr());
            }
        }
    }

    /// Text returned by `TessBaseAPIGetUTF8Text`, released with `TessDeleteText` on drop.
    struct TessText(NonNull<c_char>);

    impl TessText {
        fn to_string_lossy(&self) -> String {
            // SAFETY: `self.0` is a non-null, NUL-terminated string allocated by Tesseract and
            // valid until `TessDeleteText` runs in `drop`.
            let text = unsafe { CStr::from_ptr(self.0.as_ptr()) };
            text.to_string_lossy().into_owned()
        }
    }

    impl Drop for TessText {
        fn drop(&mut self) {
            // SAFETY: `self.0` came from `TessBaseAPIGetUTF8Text` and is released exactly once.
            unsafe { ffi::TessDeleteText(self.0.as_ptr()) };
        }
    }

    /// Lazily created engine for one scan: created on the first image that misses the cache.
    enum EngineState {
        Uninitialized,
        Ready(TessHandle),
        Unavailable,
    }

    impl EngineState {
        fn ready(
            &mut self,
            tessdata: Option<&Path>,
            lang: &str,
        ) -> Result<&mut TessHandle, String> {
            if let Self::Uninitialized = self {
                *self = match open_engine(tessdata, lang) {
                    Ok(handle) => Self::Ready(handle),
                    Err(err) => {
                        warn_engine_unavailable(&err);
                        Self::Unavailable
                    }
                };
            }
            match self {
                Self::Ready(handle) => Ok(handle),
                Self::Unavailable | Self::Uninitialized => {
                    Err("Tesseract OCR engine is unavailable".to_string())
                }
            }
        }
    }

    fn open_engine(tessdata: Option<&Path>, lang: &str) -> Result<TessHandle, String> {
        let dir = tessdata.ok_or_else(|| {
            "no Tesseract language data directory found (set FAST_DOCS_TESSDATA_DIR)".to_string()
        })?;
        TessHandle::new(dir, lang)
    }

    static ENGINE_WARNING: Once = Once::new();

    fn warn_engine_unavailable(err: &str) {
        ENGINE_WARNING.call_once(|| {
            tracing::warn!("OCR disabled for images in this process: {err}");
        });
    }

    /// Decode an image in safe Rust after checking its header dimensions.
    ///
    /// Returns `Ok(None)` for images too small to hold text.
    pub(super) fn decode_gray(bytes: &[u8]) -> Result<Option<GrayImage>, String> {
        let header = ImageReader::new(Cursor::new(bytes))
            .with_guessed_format()
            .map_err(|err| format!("failed to read image header: {err}"))?;
        let (width, height) = header
            .into_dimensions()
            .map_err(|err| format!("unsupported or corrupt image: {err}"))?;
        match check_dimensions(width, height) {
            DimensionCheck::Accept => {}
            DimensionCheck::TooSmall => return Ok(None),
            DimensionCheck::TooLarge => {
                return Err(format!("image {width}x{height} exceeds OCR size bounds"));
            }
        }

        let mut reader = ImageReader::new(Cursor::new(bytes))
            .with_guessed_format()
            .map_err(|err| format!("failed to read image header: {err}"))?;
        let mut limits = Limits::default();
        limits.max_image_width = Some(MAX_OCR_SIDE_PX);
        limits.max_image_height = Some(MAX_OCR_SIDE_PX);
        limits.max_alloc = Some(MAX_OCR_DECODE_ALLOC_BYTES);
        reader.limits(limits);
        let image = reader
            .decode()
            .map_err(|err| format!("failed to decode image: {err}"))?;
        Ok(Some(image.into_luma8()))
    }

    fn non_empty(text: String) -> Option<String> {
        (!text.is_empty()).then_some(text)
    }

    fn ocr_image_text(
        path: &Path,
        cache: Option<&DiskCache>,
        version: &str,
        lang: &str,
        tessdata: Option<&Path>,
        engine: &mut EngineState,
    ) -> Result<Option<String>, String> {
        let bytes = fs::read(path).map_err(|err| format!("failed to read image: {err}"))?;
        if bytes.len() as u64 > MAX_RICH_DOC_FILE_BYTES {
            return Err(format!("image exceeds {MAX_RICH_DOC_FILE_BYTES} bytes"));
        }

        let key = ocr_cache_key(&bytes, lang, version);
        if let Some(cached) = cache.and_then(|cache| cache.get(&key, true)) {
            return Ok(non_empty(cached));
        }

        let handle = engine.ready(tessdata, lang)?;
        let Some(image) = decode_gray(&bytes)? else {
            return Ok(None);
        };
        let (raw_text, confidence) = handle.recognize(&image)?;
        let text = if passes_confidence(confidence) {
            bound_ocr_text(raw_text.trim())
        } else {
            String::new()
        };

        // Empty results are cached too, so filtered images are not re-processed.
        if let Some(cache) = cache {
            if let Err(err) = cache.put(&key, &text, true) {
                tracing::debug!("failed to cache OCR result: {err}");
            }
        }
        Ok(non_empty(text))
    }

    /// OCR every image (already capped and priority-sorted by the caller) into doc sections.
    pub(crate) fn ocr_image_sections(
        images: &[PathBuf],
        root: &Path,
        cache: Option<&DiskCache>,
    ) -> Vec<DocSection> {
        ocr_image_sections_with(
            images,
            root,
            cache,
            default_tessdata_dir().as_deref(),
            DEFAULT_OCR_LANG,
        )
    }

    pub(super) fn ocr_image_sections_with(
        images: &[PathBuf],
        root: &Path,
        cache: Option<&DiskCache>,
        tessdata: Option<&Path>,
        lang: &str,
    ) -> Vec<DocSection> {
        let mut sections = Vec::new();
        if images.is_empty() {
            return sections;
        }
        let version = tesseract_version().unwrap_or_default();
        let mut engine = EngineState::Uninitialized;
        for path in images {
            let rel = path
                .strip_prefix(root)
                .unwrap_or(path)
                .display()
                .to_string();
            match ocr_image_text(path, cache, &version, lang, tessdata, &mut engine) {
                Ok(Some(text)) => {
                    sections.extend(lines_to_sections(&text, &format!("{rel} (OCR)"), &rel));
                }
                Ok(None) => {}
                Err(err) => tracing::debug!("skipping image '{rel}' for OCR: {err}"),
            }
        }
        sections
    }
}

#[cfg(fast_docs_ocr)]
pub(crate) use engine::ocr_image_sections;

/// Warm up the LSTM engine in a background thread when the native library is available.
#[cfg(fast_docs_ocr)]
pub fn warm_up_engine() {
    unsafe extern "C" {
        #[link_name = "TessBaseAPIWarmup"]
        fn warm_up();
    }
    if (warm_up as usize) == 0 {
        return;
    }
    std::thread::spawn(|| unsafe { warm_up() });
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use std::ffi::OsString;

    use super::{
        bound_ocr_text, check_dimensions, is_ocr_image_file, ocr_cache_key, passes_confidence,
        resolve_tessdata_dir, tessdata_override, validate_lang, DimensionCheck, OCR_ENABLED,
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
    fn tessdata_override_ignores_unset_and_empty_values() {
        assert_eq!(tessdata_override(None), None);
        assert_eq!(tessdata_override(Some(OsString::new())), None);
        assert_eq!(
            tessdata_override(Some(OsString::from("/custom/tessdata"))),
            Some(PathBuf::from("/custom/tessdata"))
        );
    }

    #[cfg(not(fast_docs_ocr))]
    #[test]
    fn tesseract_version_is_none_without_ocr() {
        assert_eq!(super::tesseract_version(), None);
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
        assert_eq!(check_dimensions(2048, 8192), DimensionCheck::Accept);
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

#[cfg(all(test, fast_docs_ocr))]
mod engine_tests {
    use std::fs;
    use std::io::Cursor;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use image::{GrayImage, ImageFormat, Luma, Rgba, RgbaImage};

    use super::engine::{decode_gray, ocr_image_sections_with, TessHandle};
    use super::{default_tessdata_dir, tesseract_version, DEFAULT_OCR_LANG, DEV_TESSDATA_DIR};
    use crate::cache::DiskCache;
    use crate::extractors::MAX_RICH_DOC_FILE_BYTES;

    fn fixture_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ocr")
    }

    fn unique_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        std::env::temp_dir().join(format!(
            "fast-docs-ocr-engine-{label}-{}-{nanos}",
            std::process::id()
        ))
    }

    fn encoded_gray_png(width: u32, height: u32, value: u8) -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        let written = GrayImage::from_pixel(width, height, Luma([value]))
            .write_to(&mut out, ImageFormat::Png);
        assert!(written.is_ok());
        out.into_inner()
    }

    #[test]
    fn tesseract_version_reports_major_five() {
        let version = tesseract_version();
        assert!(
            version.as_deref().is_some_and(|v| v.starts_with("5.")),
            "{version:?}"
        );
    }

    #[test]
    fn tess_handle_rejects_missing_language_data() {
        let empty = unique_dir("empty-tessdata");
        assert!(fs::create_dir_all(&empty).is_ok());
        assert!(TessHandle::new(&empty, DEFAULT_OCR_LANG).is_err());
        let _ = fs::remove_dir_all(&empty);
    }

    #[test]
    fn tess_handle_rejects_invalid_language() {
        assert!(TessHandle::new(Path::new(DEV_TESSDATA_DIR), "../eng").is_err());
    }

    #[test]
    fn decode_gray_accepts_text_fixture_and_skips_icons() {
        let fixture = fs::read(fixture_dir().join("ledger.png"));
        assert!(fixture.is_ok());
        if let Ok(bytes) = fixture {
            let decoded = decode_gray(&bytes);
            assert!(
                matches!(decoded, Ok(Some(ref image)) if image.width() == 1200 && image.height() == 120)
            );
        }
        assert!(matches!(
            decode_gray(&encoded_gray_png(16, 16, 0)),
            Ok(None)
        ));
    }

    #[test]
    fn decode_gray_accepts_rgba_at_the_maximum_pixel_count() {
        // 4096 x 4096 RGBA8 needs exactly MAX_OCR_DECODE_ALLOC_BYTES (64 MiB) to decode.
        let mut out = Cursor::new(Vec::new());
        let written = RgbaImage::from_pixel(4096, 4096, Rgba([255, 255, 255, 255]))
            .write_to(&mut out, ImageFormat::Png);
        assert!(written.is_ok());
        let decoded = decode_gray(&out.into_inner());
        assert!(
            matches!(decoded, Ok(Some(ref image)) if image.width() == 4096 && image.height() == 4096),
            "{:?}",
            decoded.map(|image| image.map(|i| i.dimensions()))
        );
    }

    #[test]
    fn default_tessdata_dir_finds_english_language_data() {
        let dir = default_tessdata_dir();
        assert!(
            dir.as_deref()
                .is_some_and(|d| d.join("eng.traineddata").is_file()),
            "{dir:?}"
        );
    }

    #[test]
    fn ocr_image_sections_enforce_the_file_size_cap() {
        let fixture = fs::read(fixture_dir().join("ledger.png"));
        assert!(fixture.is_ok());
        let Ok(png) = fixture else {
            return;
        };
        let dir = unique_dir("size-cap");
        assert!(fs::create_dir_all(&dir).is_ok());
        let Ok(cap) = usize::try_from(MAX_RICH_DOC_FILE_BYTES) else {
            return;
        };
        // PNG decoders ignore bytes after IEND, so padding keeps the image readable.
        for (name, size) in [("at-cap.png", cap), ("over-cap.png", cap + 1)] {
            let mut padded = png.clone();
            padded.resize(size, 0);
            assert!(fs::write(dir.join(name), &padded).is_ok());
        }
        let tessdata = default_tessdata_dir();
        let at_cap = ocr_image_sections_with(
            &[dir.join("at-cap.png")],
            &dir,
            None,
            tessdata.as_deref(),
            DEFAULT_OCR_LANG,
        );
        let over_cap = ocr_image_sections_with(
            &[dir.join("over-cap.png")],
            &dir,
            None,
            tessdata.as_deref(),
            DEFAULT_OCR_LANG,
        );
        assert_eq!(at_cap.len(), 1);
        assert!(over_cap.is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn decode_gray_rejects_oversized_and_garbage_input() {
        assert!(decode_gray(&encoded_gray_png(8193, 32, 255)).is_err());
        assert!(decode_gray(b"definitely not an image").is_err());
    }

    #[test]
    fn ocr_image_sections_reads_fixture_text() {
        let root = fixture_dir();
        let sections = ocr_image_sections_with(
            &[root.join("ledger.png")],
            &root,
            None,
            default_tessdata_dir().as_deref(),
            DEFAULT_OCR_LANG,
        );
        assert_eq!(sections.len(), 1);
        if let Some(section) = sections.first() {
            assert_eq!(section.title, "ledger.png (OCR)");
            assert_eq!(section.source.as_deref(), Some("ledger.png"));
            assert!(section
                .descriptions
                .join(" ")
                .contains("Quantum Ledger Reconciliation"));
        }
    }

    #[test]
    fn ocr_image_sections_skip_everything_without_language_data() {
        let empty = unique_dir("no-tessdata");
        assert!(fs::create_dir_all(&empty).is_ok());
        let root = fixture_dir();
        let sections = ocr_image_sections_with(
            &[root.join("ledger.png")],
            &root,
            None,
            Some(&empty),
            DEFAULT_OCR_LANG,
        );
        assert!(sections.is_empty());
        let _ = fs::remove_dir_all(&empty);
    }

    #[test]
    fn ocr_image_sections_skip_missing_files() {
        let root = fixture_dir();
        let sections = ocr_image_sections_with(
            &[root.join("does-not-exist.png")],
            &root,
            None,
            default_tessdata_dir().as_deref(),
            DEFAULT_OCR_LANG,
        );
        assert!(sections.is_empty());
    }

    #[test]
    fn ocr_image_sections_keep_results_when_cache_write_fails() {
        let root = fixture_dir();
        // A cache rooted under a regular file can neither be read nor written.
        let cache = DiskCache::new(
            root.join("ledger.png").join("cache"),
            Duration::from_secs(60),
            Duration::from_secs(60),
        );
        let sections = ocr_image_sections_with(
            &[root.join("ledger.png")],
            &root,
            Some(&cache),
            default_tessdata_dir().as_deref(),
            DEFAULT_OCR_LANG,
        );
        assert_eq!(sections.len(), 1);
    }
}
