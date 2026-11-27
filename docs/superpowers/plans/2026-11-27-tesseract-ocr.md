# Tesseract OCR for `fetch_local_docs` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** On macOS Apple Silicon Homebrew builds, `fetch_local_docs` extracts text from repository images with Tesseract, linked natively against a standalone `libtesseract.dylib` shipped as a GitHub Release asset.

**Architecture:** An opt-in Cargo feature `ocr` makes `build.rs` emit `cfg(fast_docs_ocr)` and link `-ltesseract` with an `@loader_path/../libexec/lib` rpath, only on `aarch64-apple-darwin`. All FFI and `unsafe` code lives in `src/ocr.rs`. Pure guard functions in that module are compiled everywhere, while the engine sits in a `cfg(fast_docs_ocr)` inner module. `src/local_docs.rs` collects images separately from text documents, and `fetch_local_docs` runs the scan in `spawn_blocking`. OCR text is cached in `DiskCache` by content hash.

**Tech Stack:** Rust 1.100 (edition 2021), Tesseract 5.5.5 C API, `image` 0.25.10 (optional), `tokio`, Homebrew Ruby formula, GitHub Actions.

**Spec:** `docs/superpowers/specs/2026-11-27-tesseract-ocr-design.md`

## Global Constraints

- OCR only exists when the `ocr` feature is enabled **and** the target is `macos` + `aarch64`. Every other build behaves exactly as today.
- `cargo clippy --all-targets --all-features -- -D warnings` must pass on Ubuntu (feature on, `fast_docs_ocr` off) and on macOS ARM (feature on, `fast_docs_ocr` on). The `prek` hook runs this command, so macOS ARM contributors need `brew install tesseract`.
- Zero warnings. Suppress only with `#[expect(..., reason = "...")]`, never `#[allow(...)]`.
- No `.unwrap()`, `.expect()`, or `panic!` in production code or tests. Tests follow the existing pattern: `assert!(x.is_ok()); if let Ok(v) = x { ... }` or `let Ok(v) = x else { return; };`.
- Every `unsafe` block is preceded by a `// SAFETY:` comment. No `unsafe` outside `src/ocr.rs`.
- Bounds (exact values): 5 MiB max image file (`MAX_RICH_DOC_FILE_BYTES`), 8192 px max per side, 16,777,216 px max total, 32 px min per side, confidence ≥ 60 kept, 2 MiB max text (`MAX_EXTRACTED_BYTES`), 16 images max per call, source resolution 70 DPI, default language `eng`.
- `image` dependency: `version = "0.25.10"`, `optional = true`, `default-features = false`, `features = ["png", "jpeg", "webp", "tiff", "gif"]`.
- Release asset: tag `tesseract-5.5.5`, archive `fast-docs-tesseract-5.5.5-aarch64-apple-darwin.tar.gz` containing `lib/libtesseract.dylib` and `tessdata/eng.traineddata`.
- Commits: imperative mood, subject ≤ 72 characters, one logical change, message ends with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`. Work on branch `feat/tesseract-ocr`; never push to `main`. Run `prek run` before each commit.
- Delete with `trash`, never `rm -rf`.

## Commands Used Throughout

| Purpose | Command |
| --- | --- |
| Pure-Rust test suite | `cargo test` |
| OCR test suite (macOS ARM, needs `brew install tesseract`) | `cargo test --features ocr` |
| Lint, OCR on | `cargo clippy --all-targets --all-features -- -D warnings` |
| Lint, OCR off | `cargo clippy --all-targets -- -D warnings` |
| Format | `cargo fmt -- --check` |

## File Structure

| File | Responsibility |
| --- | --- |
| `build.rs` (new) | Emits `cfg(fast_docs_ocr)`, link search path, `-ltesseract`, and rpath directives |
| `Cargo.toml` | `ocr` feature and optional `image` dependency |
| `src/ocr.rs` (new) | Pure guards (always compiled) + `engine` inner module (FFI, decoding, cache, sections) under `cfg(fast_docs_ocr)` + `/health` JSON |
| `src/lib.rs` | `pub mod ocr;` |
| `src/http_server.rs` | Adds the `ocr` field to `GET /health` |
| `src/extractors.rs` | `lines_to_sections` becomes `pub(crate)` |
| `src/local_docs.rs` | Separate image collection, priority sort helper, 16-image cap, OCR sections, new `cache` parameter |
| `src/tools.rs` | `fetch_local_docs` runs in `spawn_blocking`, passes the cache, updated description |
| `scripts/generate-ocr-fixture.swift` (new) | Regenerates the committed text fixture |
| `tests/fixtures/ocr/ledger.png` (new) | 1200×120 PNG reading `fast-docs OCR fixture: Quantum Ledger Reconciliation` |
| `tests/ocr_tests.rs` (new) | End-to-end OCR tests (`#![cfg(fast_docs_ocr)]`) |
| `tests/local_docs_tests.rs`, `tests/extractors_tests.rs` | Call sites updated; non-OCR regression test |
| `tests/http_transport_tests.rs` | `/health` `ocr` assertions |
| `.github/workflows/ci.yml` | New `macos-15` OCR job |
| `Formula/fast-docs.rb` | `tesseract` resource, feature-enabled install, `/health` test |
| `MEMORY.md`, `README.md`, `ROADMAP.md` | ADR D-21, Q-OCR-1, reworded `unsafe` claims, OCR docs |

---

### Task 1: `ocr` feature plumbing and `/health` OCR status

**Files:**
- Create: `build.rs`
- Create: `src/ocr.rs`
- Modify: `Cargo.toml` (`[dependencies]`, new `[features]`)
- Modify: `src/lib.rs`
- Modify: `src/http_server.rs:141-147`
- Test: `tests/http_transport_tests.rs:78-89`, unit tests in `src/ocr.rs`

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `pub const DEFAULT_OCR_LANG: &str` (`"eng"`) and `pub const DEV_TESSDATA_DIR: &str`
  - `pub fn resolve_tessdata_dir(env_override: Option<PathBuf>, exe_path: Option<&Path>, dev_fallback: &Path) -> Option<PathBuf>`
  - `pub fn default_tessdata_dir() -> Option<PathBuf>`
  - `pub fn tesseract_version() -> Option<String>` (OCR builds call the FFI; other builds return `None`)
  - `pub fn health_json() -> String`
  - The `cfg(fast_docs_ocr)` flag and the private `engine` module (with an inner `ffi` module), both extended in Task 3

- [ ] **Step 1: Write the failing health assertions**

In `tests/http_transport_tests.rs`, inside `streamable_http_server_handles_initialize_health_404_and_dns_rebinding`, right after `assert!(text.contains("fast-docs"));`, add:

```rust
        #[cfg(not(fast_docs_ocr))]
        assert!(text.contains("\"ocr\":null"), "unexpected health: {text}");
        #[cfg(fast_docs_ocr)]
        assert!(
            text.contains("\"engine\":\"tesseract\""),
            "unexpected health: {text}"
        );
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test --test http_transport_tests streamable_http_server_handles_initialize_health`
Expected: FAIL. Rustc also emits an `unexpected_cfgs` warning for `fast_docs_ocr`, because nothing declares it yet. The assertion fails with `unexpected health: {"status":"ok","service":"fast-docs","version":"1.0.0"}`.

- [ ] **Step 3: Add the feature, the optional dependency, and `build.rs`**

In `Cargo.toml`, add under `[dependencies]` (after `pdf-extract`):

```toml
image = { version = "0.25.10", optional = true, default-features = false, features = ["png", "jpeg", "webp", "tiff", "gif"] }
```

Add a new section after `[dev-dependencies]`:

```toml
[features]
# Tesseract OCR for local images. Only takes effect on macOS Apple Silicon (see build.rs).
ocr = ["dep:image"]
```

Create `build.rs`:

```rust
//! Links the optional Tesseract OCR library on macOS Apple Silicon (`--features ocr`).

use std::env;
use std::path::PathBuf;

/// Homebrew's `tesseract` keg, used for local development builds.
const DEV_TESSERACT_LIB_DIR: &str = "/opt/homebrew/opt/tesseract/lib";

#[expect(
    clippy::print_stdout,
    reason = "Cargo build scripts communicate with Cargo through `cargo:` directives on stdout"
)]
fn main() -> Result<(), String> {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=FAST_DOCS_TESSERACT_LIB_DIR");
    println!("cargo:rerun-if-env-changed=FAST_DOCS_PORTABLE_RPATH");
    println!("cargo:rustc-check-cfg=cfg(fast_docs_ocr)");

    let ocr_requested = env::var_os("CARGO_FEATURE_OCR").is_some();
    let is_macos = env::var("CARGO_CFG_TARGET_OS").is_ok_and(|os| os == "macos");
    let is_arm64 = env::var("CARGO_CFG_TARGET_ARCH").is_ok_and(|arch| arch == "aarch64");
    if !(ocr_requested && is_macos && is_arm64) {
        return Ok(());
    }

    let lib_dir = env::var_os("FAST_DOCS_TESSERACT_LIB_DIR")
        .filter(|dir| !dir.is_empty())
        .map_or_else(|| PathBuf::from(DEV_TESSERACT_LIB_DIR), PathBuf::from);
    if !lib_dir.join("libtesseract.dylib").is_file() {
        return Err(format!(
            "the `ocr` feature needs libtesseract.dylib in {}: set FAST_DOCS_TESSERACT_LIB_DIR or run `brew install tesseract`",
            lib_dir.display()
        ));
    }

    println!("cargo:rustc-cfg=fast_docs_ocr");
    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    println!("cargo:rustc-link-lib=dylib=tesseract");
    // Homebrew layout: bin/fast-docs loads libexec/lib/libtesseract.dylib.
    println!("cargo:rustc-link-arg=-Wl,-rpath,@loader_path/../libexec/lib");
    // Development builds also need the build-machine directory; Homebrew builds must not leak it.
    if env::var("FAST_DOCS_PORTABLE_RPATH").as_deref() != Ok("1") {
        println!("cargo:rustc-link-arg=-Wl,-rpath,{}", lib_dir.display());
    }
    Ok(())
}
```

- [ ] **Step 4: Write the failing `tessdata` resolution unit tests**

Create `src/ocr.rs` with only the module doc and tests for now:

```rust
//! Optional Tesseract OCR for images found by `fetch_local_docs`.
//!
//! The engine is compiled only on macOS Apple Silicon with `--features ocr`, signalled by the
//! `fast_docs_ocr` cfg that `build.rs` emits. All `unsafe` code of the crate lives in this module.

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
            resolve_tessdata_dir(Some(env_dir.clone()), Some(&exe), Path::new("/nonexistent/dev")),
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
        assert_eq!(resolve_tessdata_dir(None, Some(&exe), &dev), Some(dev.clone()));
        assert_eq!(
            resolve_tessdata_dir(None, Some(&exe), Path::new("/nonexistent/dev")),
            None
        );
        assert_eq!(resolve_tessdata_dir(None, None, Path::new("/nonexistent/dev")), None);
        let _ = fs::remove_dir_all(&dev);
    }
}
```

Add `pub mod ocr;` to `src/lib.rs`, between `pub mod manifest;` and `pub mod tools;`.

- [ ] **Step 5: Run them to verify they fail**

Run: `cargo test --lib ocr::tests`
Expected: FAIL to compile with `unresolved import super::resolve_tessdata_dir`.

- [ ] **Step 6: Implement resolution, version, and health JSON**

Insert this between the module doc and `#[cfg(test)]` in `src/ocr.rs`:

```rust
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
    resolve_tessdata_dir(env_override, exe_path.as_deref(), Path::new(DEV_TESSDATA_DIR))
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
```

If rustc reports `non_snake_case` on the `extern` block, put `#[expect(non_snake_case, reason = "names must match Tesseract's C API symbols")]` on the `unsafe extern "C"` block.

In `src/http_server.rs`, replace the `/health` payload:

```rust
                                    let payload = format!(
                                        r#"{{"status":"ok","service":"fast-docs","version":"{}","ocr":{}}}"#,
                                        env!("CARGO_PKG_VERSION"),
                                        crate::ocr::health_json()
                                    );
```

- [ ] **Step 7: Run the tests in both modes**

Run: `cargo test --lib ocr::tests && cargo test --test http_transport_tests`
Expected: PASS. The health assertion takes the `"ocr":null` branch.

Run: `cargo test --features ocr --lib ocr::tests && cargo test --features ocr --test http_transport_tests`
Expected: PASS. The health assertion takes the `"engine":"tesseract"` branch, which proves `-ltesseract` links and `TessVersion` runs.

- [ ] **Step 8: Lint, audit dependencies, verify linkage**

Run: `cargo fmt -- --check && cargo clippy --all-targets -- -D warnings && cargo clippy --all-targets --all-features -- -D warnings`
Expected: no warnings.

Run: `cargo deny check`
Expected: `advisories ok, bans ok, licenses ok, sources ok`. If `licenses` rejects a crate pulled in by `image`, **stop and report the crate and its license to the user**. Adding a license to `deny.toml` is their decision.

Run: `cargo build --features ocr && otool -L target/debug/fast-docs | rg tesseract`
Expected: one line referencing `libtesseract`.

- [ ] **Step 9: Commit**

```bash
git add build.rs Cargo.toml Cargo.lock src/ocr.rs src/lib.rs src/http_server.rs tests/http_transport_tests.rs
prek run
git commit -m "Add ocr feature linking Tesseract and report it in /health" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 2: Pure OCR guards (language, dimensions, confidence, text bound, cache key)

**Files:**
- Modify: `src/ocr.rs` (constants and functions above `#[cfg(not(fast_docs_ocr))] pub fn tesseract_version`, tests in `mod tests`)

**Interfaces:**
- Consumes: `crate::extractors::MAX_EXTRACTED_BYTES` (`usize`, 2 MiB).
- Produces (all `pub` and compiled on every platform):
  - `MAX_OCR_IMAGES: usize = 16`, `MIN_OCR_SIDE_PX: u32 = 32`, `MAX_OCR_SIDE_PX: u32 = 8192`, `MAX_OCR_PIXELS: u64 = 16_777_216`, `MAX_OCR_DECODE_ALLOC_BYTES: u64`, `MIN_OCR_CONFIDENCE: i32 = 60`, `OCR_SOURCE_DPI: i32 = 70`, `OCR_IMAGE_EXTENSIONS: [&str; 7]`, `OCR_ENABLED: bool`
  - `fn is_ocr_image_file(path: &Path) -> bool`
  - `fn validate_lang(lang: &str) -> Result<(), String>`
  - `enum DimensionCheck { Accept, TooSmall, TooLarge }` and `fn check_dimensions(width: u32, height: u32) -> DimensionCheck`
  - `fn passes_confidence(confidence: i32) -> bool`
  - `fn bound_ocr_text(text: &str) -> String`
  - `fn ocr_cache_key(content: &[u8], lang: &str, version: &str) -> String`

- [ ] **Step 1: Write the failing unit tests**

Extend the `use super::...` line in `src/ocr.rs` `mod tests` to:

```rust
    use super::{
        bound_ocr_text, check_dimensions, is_ocr_image_file, ocr_cache_key, passes_confidence,
        resolve_tessdata_dir, validate_lang, DimensionCheck, OCR_ENABLED,
    };
    use crate::extractors::MAX_EXTRACTED_BYTES;
```

Append these tests to `mod tests`:

```rust
    #[test]
    fn validate_lang_accepts_tesseract_codes_and_combinations() {
        for lang in ["eng", "chi_sim", "eng+fra", "deu+eng+chi_tra"] {
            assert!(validate_lang(lang).is_ok(), "{lang} should be accepted");
        }
    }

    #[test]
    fn validate_lang_rejects_traversal_separators_and_empty_parts() {
        for lang in ["", "../eng", "a/b", "eng+", "+eng", "eng++fra", "eng\0", "en g", "eng.traineddata"] {
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
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test --lib ocr::tests`
Expected: FAIL to compile with unresolved imports `bound_ocr_text`, `check_dimensions`, and the rest.

- [ ] **Step 3: Implement the guards**

In `src/ocr.rs`, add `use sha2::{Digest, Sha256};` and `use crate::extractors::MAX_EXTRACTED_BYTES;` to the imports. Then insert after `DEV_TESSDATA_DIR`:

```rust
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
    format!("ocr:{}:{lang}:{version}", hex::encode(Sha256::digest(content)))
}
```

- [ ] **Step 4: Run the tests in both modes**

Run: `cargo test --lib ocr::tests && cargo test --features ocr --lib ocr::tests`
Expected: PASS (10 tests each run).

- [ ] **Step 5: Lint and commit**

Run: `cargo fmt -- --check && cargo clippy --all-targets -- -D warnings && cargo clippy --all-targets --all-features -- -D warnings`
Expected: no warnings.

```bash
git add src/ocr.rs
prek run
git commit -m "Add OCR input guards for language, size, confidence, and cache keys" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 3: Tesseract engine, safe image decoding, and OCR sections

**Files:**
- Create: `scripts/generate-ocr-fixture.swift`
- Create: `tests/fixtures/ocr/ledger.png` (generated)
- Modify: `src/ocr.rs` (`mod engine`, re-export)
- Modify: `src/extractors.rs:186` (`lines_to_sections` visibility)

**Interfaces:**
- Consumes: Task 1 (`engine` module, `default_tessdata_dir`, `DEFAULT_OCR_LANG`) and Task 2 (all guards and constants). Also `crate::cache::DiskCache::{get, put}`, `crate::extractors::{lines_to_sections, MAX_RICH_DOC_FILE_BYTES}`, `crate::tools::DocSection`.
- Produces: `pub(crate) fn ocr_image_sections(images: &[PathBuf], root: &Path, cache: Option<&DiskCache>) -> Vec<DocSection>`, exported as `crate::ocr::ocr_image_sections` under `cfg(fast_docs_ocr)`. Each image yields at most one section titled `"<path relative to root> (OCR)"` with `source = Some(<relative path>)`.

- [ ] **Step 1: Create the fixture generator and the fixture**

Create `scripts/generate-ocr-fixture.swift`:

```swift
// Regenerates tests/fixtures/ocr/ledger.png, the text image used by the OCR tests.
// Usage: swift scripts/generate-ocr-fixture.swift tests/fixtures/ocr/ledger.png
import AppKit

let arguments = CommandLine.arguments
guard arguments.count == 2 else {
    FileHandle.standardError.write(Data("usage: generate-ocr-fixture.swift <output.png>\n".utf8))
    exit(2)
}

let width = 1200
let height = 120
guard let context = CGContext(
    data: nil, width: width, height: height, bitsPerComponent: 8, bytesPerRow: 0,
    space: CGColorSpaceCreateDeviceGray(), bitmapInfo: CGImageAlphaInfo.none.rawValue
), let font = NSFont(name: "Helvetica", size: 40) else {
    exit(1)
}
context.setFillColor(gray: 1, alpha: 1)
context.fill(CGRect(x: 0, y: 0, width: width, height: height))
NSGraphicsContext.current = NSGraphicsContext(cgContext: context, flipped: false)
NSAttributedString(
    string: "fast-docs OCR fixture: Quantum Ledger Reconciliation",
    attributes: [.font: font, .foregroundColor: NSColor.black]
).draw(at: NSPoint(x: 20, y: 40))

let output = URL(fileURLWithPath: arguments[1])
guard let image = context.makeImage(),
      let destination = CGImageDestinationCreateWithURL(output as CFURL, "public.png" as CFString, 1, nil) else {
    exit(1)
}
CGImageDestinationAddImage(destination, image, nil)
guard CGImageDestinationFinalize(destination) else { exit(1) }
```

Run: `mkdir -p tests/fixtures/ocr && swift scripts/generate-ocr-fixture.swift tests/fixtures/ocr/ledger.png && tesseract stdin - < tests/fixtures/ocr/ledger.png`
Expected: the file is about 8 KB, and `tesseract` prints `fast-docs OCR fixture: Quantum Ledger Reconciliation`. Pipe the image through `stdin`, because Leptonica's file opener can fail on some temporary paths.

- [ ] **Step 2: Write the failing engine tests**

Append to `src/ocr.rs`, after `mod engine`'s closing brace:

```rust
#[cfg(all(test, fast_docs_ocr))]
mod engine_tests {
    use std::fs;
    use std::io::Cursor;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use image::{GrayImage, ImageFormat, Luma};

    use super::engine::{decode_gray, ocr_image_sections_with, TessHandle};
    use super::{default_tessdata_dir, tesseract_version, DEFAULT_OCR_LANG, DEV_TESSDATA_DIR};
    use crate::cache::DiskCache;

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
        let written = GrayImage::from_pixel(width, height, Luma([value])).write_to(&mut out, ImageFormat::Png);
        assert!(written.is_ok());
        out.into_inner()
    }

    #[test]
    fn tesseract_version_reports_major_five() {
        let version = tesseract_version();
        assert!(version.as_deref().is_some_and(|v| v.starts_with("5.")), "{version:?}");
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
            assert!(matches!(decoded, Ok(Some(ref image)) if image.width() == 1200 && image.height() == 120));
        }
        assert!(matches!(decode_gray(&encoded_gray_png(16, 16, 0)), Ok(None)));
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
            assert!(section.descriptions.join(" ").contains("Quantum Ledger Reconciliation"));
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
```

- [ ] **Step 3: Run them to verify they fail**

Run: `cargo test --features ocr --lib ocr::engine_tests`
Expected: FAIL to compile with `unresolved imports super::engine::{decode_gray, ocr_image_sections_with, TessHandle}`.

- [ ] **Step 4: Expose `lines_to_sections` to the crate**

In `src/extractors.rs:186`, change `fn lines_to_sections(` to `pub(crate) fn lines_to_sections(`.

- [ ] **Step 5: Implement the engine**

In `src/ocr.rs`, replace the whole `#[cfg(fast_docs_ocr)] mod engine { ... }` block from Task 1 with:

```rust
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
        fn ready(&mut self, tessdata: Option<&Path>, lang: &str) -> Result<&mut TessHandle, String> {
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
            let rel = path.strip_prefix(root).unwrap_or(path).display().to_string();
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
```

`ocr_image_sections` is used by `local_docs` only from Task 4 onward. If clippy reports it as unused at the end of this task, put `#[expect(dead_code, reason = "wired into search_local_docs in the next commit")]` on the `pub(crate) use`, and remove that attribute in Task 4 Step 6.

- [ ] **Step 6: Run the engine tests and the full OCR suite**

Run: `cargo test --features ocr --lib ocr::`
Expected: PASS (19 tests: 10 in `tests`, 9 in `engine_tests`).

Run: `cargo test` (pure-Rust build)
Expected: PASS. None of the engine code is compiled.

- [ ] **Step 7: Lint and commit**

Run: `cargo fmt -- --check && cargo clippy --all-targets -- -D warnings && cargo clippy --all-targets --all-features -- -D warnings`
Expected: no warnings.

```bash
git add scripts/generate-ocr-fixture.swift tests/fixtures/ocr/ledger.png src/ocr.rs src/extractors.rs
prek run
git commit -m "Add Tesseract FFI engine with bounded image decoding" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 4: OCR in `fetch_local_docs` (collection, cap, cache, non-blocking handler)

**Files:**
- Modify: `src/local_docs.rs` (imports, `collect_doc_files`, new `is_image_file` and `sort_by_priority`, `search_local_docs`)
- Modify: `src/tools.rs:2` (imports), `src/tools.rs:2892-2924` (`fetch_local_docs` description and body)
- Modify: `tests/local_docs_tests.rs` (6 call sites + 1 new test), `tests/extractors_tests.rs:180`
- Create: `tests/ocr_tests.rs`

**Interfaces:**
- Consumes: `crate::ocr::{is_ocr_image_file, MAX_OCR_IMAGES, ocr_image_sections}`, `crate::cache::DiskCache`.
- Produces: `pub fn search_local_docs(root: &Path, topic: Option<&str>, max_chars: usize, offset: usize, cache: Option<&DiskCache>) -> Result<String, String>`.

- [ ] **Step 1: Write the failing end-to-end OCR tests**

Create `tests/ocr_tests.rs`:

```rust
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
    assert!(GrayImage::from_pixel(400, 100, Luma([255])).save(dir.join("docs/blank.png")).is_ok());
    assert!(GrayImage::from_pixel(16, 16, Luma([0])).save(dir.join("docs/icon.png")).is_ok());

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
        assert!(fs::copy(fixture_path(), dir.join(format!("shots/shot-{index:02}.png"))).is_ok());
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
        let key = ocr_cache_key(&bytes, DEFAULT_OCR_LANG, &tesseract_version().unwrap_or_default());
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
        let key = ocr_cache_key(&bytes, DEFAULT_OCR_LANG, &tesseract_version().unwrap_or_default());
        let cached = cache.get(&key, true);
        assert!(
            cached.as_deref().is_some_and(|text| text.contains("Quantum Ledger Reconciliation")),
            "{cached:?}"
        );
    }
    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_dir_all(&cache_root);
}
```

- [ ] **Step 2: Write the failing non-OCR regression test and update call sites**

Update the seven existing calls to the new signature:

Run: `sed -i '' -E 's/(search_local_docs\([^;]*), 0\)/\1, 0, None)/' tests/local_docs_tests.rs tests/extractors_tests.rs && rg -n "search_local_docs\(" tests`
Expected: all seven calls end in `, 0, None)`.

Append to `tests/local_docs_tests.rs`:

```rust
#[cfg(not(fast_docs_ocr))]
#[test]
fn search_local_docs_ignores_images_without_ocr_support() {
    let dir = unique_dir("no-ocr");
    assert!(fs::create_dir_all(&dir).is_ok());
    assert!(fs::write(dir.join("README.md"), "# Overview\nPlain text docs.\n").is_ok());
    assert!(fs::write(dir.join("diagram.png"), b"\x89PNG\r\n\x1a\nnot a real image").is_ok());

    let out = search_local_docs(&dir, None, 4000, 0, None);
    assert!(out.is_ok());
    if let Ok(text) = out {
        assert!(text.contains("Overview"));
        assert!(!text.contains("diagram.png"));
    }
    let _ = fs::remove_dir_all(&dir);
}
```

- [ ] **Step 3: Run them to verify they fail**

Run: `cargo test --features ocr --test ocr_tests`
Expected: FAIL to compile with `this function takes 4 arguments but 5 arguments were supplied`.

- [ ] **Step 4: Implement collection, cap, and OCR sections in `src/local_docs.rs`**

Replace the `use` block at the top with:

```rust
use std::fs;
use std::path::{Path, PathBuf};

use crate::cache::DiskCache;
use crate::extractors::{
    extract_document_sections, MAX_RICH_DOC_FILE_BYTES, SUPPORTED_DOC_EXTENSIONS,
};
use crate::ocr::{is_ocr_image_file, MAX_OCR_IMAGES};
use crate::tools::{
    render_doc_sections_paginated, DocSection, MAX_ALLOWED_OUTPUT_CHARS, MIN_OUTPUT_CHARS,
};
```

After `is_doc_file`, add:

```rust
fn is_image_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|name| !is_forbidden_filename(name))
        && is_ocr_image_file(path)
}
```

Replace `collect_doc_files` with this version. Images are collected into their own list, so they never consume the `MAX_LOCAL_FILES` budget of text documents:

```rust
fn collect_doc_files(
    dir: &Path,
    canonical_root: &Path,
    depth: usize,
    docs: &mut Vec<PathBuf>,
    images: &mut Vec<PathBuf>,
) {
    if depth > MAX_DEPTH || (docs.len() >= MAX_LOCAL_FILES && images.len() >= MAX_LOCAL_FILES) {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };

    let mut children: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    children.sort();

    for path in children {
        if docs.len() >= MAX_LOCAL_FILES && images.len() >= MAX_LOCAL_FILES {
            break;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };

        // Check symlink_metadata first to avoid traversing symlinks into ignored or external trees.
        let Ok(sym_meta) = fs::symlink_metadata(&path) else {
            continue;
        };

        if sym_meta.is_dir() {
            if IGNORED_DIRS
                .iter()
                .any(|ignored| name.eq_ignore_ascii_case(ignored))
            {
                continue;
            }
            collect_doc_files(&path, canonical_root, depth + 1, docs, images);
            continue;
        }

        let target = if is_doc_file(&path) && docs.len() < MAX_LOCAL_FILES {
            &mut *docs
        } else if is_image_file(&path) && images.len() < MAX_LOCAL_FILES {
            &mut *images
        } else {
            continue;
        };
        let Ok(canonical) = fs::canonicalize(&path) else {
            continue;
        };
        // Strictly block symlinks pointing outside the canonical project root.
        if !canonical.starts_with(canonical_root) {
            continue;
        }
        let Ok(meta) = fs::metadata(&canonical) else {
            continue;
        };
        if meta.is_file() && meta.len() <= MAX_RICH_DOC_FILE_BYTES {
            target.push(canonical);
        }
    }
}

/// Order files so `README.md` and `docs/` come before CONTRIBUTING, MIGRATION, and CHANGELOG.
fn sort_by_priority(files: &mut [PathBuf], canonical_root: &Path) {
    files.sort_by(|a, b| {
        let rel_a = a
            .strip_prefix(canonical_root)
            .unwrap_or(a)
            .to_string_lossy();
        let rel_b = b
            .strip_prefix(canonical_root)
            .unwrap_or(b)
            .to_string_lossy();
        local_doc_priority(&rel_a)
            .cmp(&local_doc_priority(&rel_b))
            .then_with(|| rel_a.cmp(&rel_b))
    });
}
```

Replace `search_local_docs` (doc comment included):

```rust
/// Scan local documentation files inside `root` (Markdown, text, Jupyter notebooks, Office,
/// `OpenDocument`, EPUB, PDF, and images when built with OCR), rank sections by optional
/// `topic`, and return paginated Context7 blocks. OCR results are cached in `cache`.
///
/// # Errors
/// Returns an error if `root` does not exist, is not a directory, or contains no readable
/// documentation files.
pub fn search_local_docs(
    root: &Path,
    topic: Option<&str>,
    max_chars: usize,
    offset: usize,
    cache: Option<&DiskCache>,
) -> Result<String, String> {
    if !root.is_dir() {
        return Err(format!(
            "project directory '{}' does not exist or is not a directory",
            root.display()
        ));
    }

    let canonical_root = fs::canonicalize(root)
        .map_err(|err| format!("failed to resolve '{}': {err}", root.display()))?;

    let mut files = Vec::new();
    let mut images = Vec::new();
    collect_doc_files(&canonical_root, &canonical_root, 0, &mut files, &mut images);

    if files.is_empty() && images.is_empty() {
        return Err(format!(
            "no supported documentation files found under '{}'",
            root.display()
        ));
    }

    sort_by_priority(&mut files, &canonical_root);
    sort_by_priority(&mut images, &canonical_root);
    if images.len() > MAX_OCR_IMAGES {
        tracing::debug!(
            "OCR limited to the first {MAX_OCR_IMAGES} of {} images",
            images.len()
        );
        images.truncate(MAX_OCR_IMAGES);
    }

    let mut all_sections: Vec<DocSection> = Vec::new();
    for file in &files {
        let rel = file
            .strip_prefix(&canonical_root)
            .unwrap_or(file)
            .display()
            .to_string();
        match extract_document_sections(file, &rel) {
            Ok(sections) => all_sections.extend(sections),
            Err(err) => {
                tracing::debug!("skipping unreadable local doc '{rel}': {err}");
            }
        }
    }

    #[cfg(fast_docs_ocr)]
    all_sections.extend(crate::ocr::ocr_image_sections(&images, &canonical_root, cache));
    // Without OCR support no image is ever collected, so the cache has nothing to store.
    #[cfg(not(fast_docs_ocr))]
    let _ = cache;

    if all_sections.is_empty() {
        return Err(format!(
            "documentation files under '{}' contained no extractable sections",
            root.display()
        ));
    }

    let budget = max_chars.clamp(MIN_OUTPUT_CHARS, MAX_ALLOWED_OUTPUT_CHARS);
    Ok(render_doc_sections_paginated(
        &all_sections,
        "local",
        "markdown",
        topic,
        budget,
        offset,
    ))
}
```

- [ ] **Step 5: Run the scan off the async runtime in `src/tools.rs`**

Change `use std::path::Path;` (line 2) to `use std::path::{Path, PathBuf};`.

Replace the `fetch_local_docs` attribute description with:

```rust
        description = "Search and read local documentation inside a private repository or directory: Markdown/text (.md, .mdx, .txt, .rst), Jupyter notebooks (.ipynb), Office/OpenDocument (.docx, .pptx, .xlsx, .odt, .odp, .ods), EPUB (.epub), PDF (.pdf), and images (.png, .jpg, .jpeg, .webp, .tif, .tiff, .gif) through on-device Tesseract OCR when built with OCR support. Enforces anti-zip-bomb, anti-XXE, image-size, secret-exclusion, and anti-symlink-escape guards with topic ranking and pagination."
```

Replace the `match search_local_docs(...) { ... }` block at the end of `fetch_local_docs` with:

```rust
        // The scan does blocking file I/O and OCR; keep it off the shared daemon's async workers.
        let root = PathBuf::from(trimmed_dir);
        let topic = req.topic.clone();
        let cache = self.cache.clone();
        let result = tokio::task::spawn_blocking(move || {
            search_local_docs(&root, topic.as_deref(), max_chars, offset, cache.as_ref())
        })
        .await
        .unwrap_or_else(|err| Err(format!("local documentation task failed: {err}")));

        match result {
            Ok(rendered) => Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                "# Local Docs ({trimmed_dir})\n\n{rendered}"
            ))])),
            Err(err) => Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                "Local documentation lookup failed: {err}"
            ))])),
        }
```

The `JoinError` branch only fires if the blocking task panics or the runtime shuts down. No test triggers it: production code does not panic by construction, and PDF parsing is already wrapped in `catch_unwind`.

- [ ] **Step 6: Run all suites in both modes**

If Task 3 added `#[expect(dead_code, ...)]` on `pub(crate) use engine::ocr_image_sections;`, remove it now.

Run: `cargo test`
Expected: PASS, including `search_local_docs_ignores_images_without_ocr_support`.

Run: `cargo test --features ocr`
Expected: PASS, including the 5 tests in `ocr_tests`. `search_local_docs_ignores_images_without_ocr_support` is compiled out.

- [ ] **Step 7: Lint and commit**

Run: `cargo fmt -- --check && cargo clippy --all-targets -- -D warnings && cargo clippy --all-targets --all-features -- -D warnings`
Expected: no warnings.

```bash
git add src/local_docs.rs src/tools.rs tests/ocr_tests.rs tests/local_docs_tests.rs tests/extractors_tests.rs
prek run
git commit -m "OCR repository images in fetch_local_docs off the async runtime" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 5: macOS OCR CI job

**Files:**
- Modify: `.github/workflows/ci.yml` (new job after `rust`)

**Interfaces:**
- Consumes: the `ocr` feature and the `/opt/homebrew/opt/tesseract` development fallback (Task 1).
- Produces: a required check that builds, lints, and tests the OCR path on every push and pull request.

- [ ] **Step 1: Add the job**

Insert after the `rust` job, using the same pinned action SHAs:

```yaml
  rust-ocr:
    name: Rust OCR (macOS arm64, clippy, test)
    runs-on: macos-15
    env:
      HOMEBREW_NO_AUTO_UPDATE: "1"
    steps:
      - name: Checkout repository
        uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          persist-credentials: false

      - name: Install Rust stable toolchain
        uses: dtolnay/rust-toolchain@39008827e6b4319a349e9a42dfc90579dd25f1ca # stable
        with:
          components: clippy

      - name: Restore Cargo cache
        uses: Swatinem/rust-cache@d1527c8f67ef9af357290475d4af4b0fab8f12e0 # v2.9.3

      - name: Install Tesseract
        run: brew install tesseract

      - name: Run Clippy with OCR
        run: cargo clippy --all-targets --features ocr -- -D warnings

      - name: Run tests with OCR
        run: cargo test --locked --features ocr
```

- [ ] **Step 2: Lint the workflow**

Run: `actionlint && zizmor .github/workflows/`
Expected: no findings. If `zizmor` flags `Swatinem/rust-cache` for cache poisoning on this job, remove the `Restore Cargo cache` step: release builds already skip caching for the same reason (D-07).

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/ci.yml
prek run
git commit -m "Run OCR clippy and tests on macOS arm64 in CI" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 6: Homebrew formula with the `tesseract` resource

**Files:**
- Modify: `Formula/fast-docs.rb`

**Interfaces:**
- Consumes: the release archive contract (Global Constraints), `FAST_DOCS_TESSERACT_LIB_DIR` and `FAST_DOCS_PORTABLE_RPATH` (Task 1), and the `/health` `ocr` field (Task 1).
- Produces: Apple Silicon installs ship `libexec/lib/libtesseract.dylib` and `libexec/tessdata/eng.traineddata`, and `fast-docs` is built with `--features ocr`.

- [ ] **Step 1: Add the resource, the conditional install, and the health test**

After `depends_on "rust" => :build`, add:

```ruby
  on_macos do
    on_arm do
      resource "tesseract" do
        url "https://github.com/bepi64/homebrew-tap/releases/download/tesseract-5.5.5/fast-docs-tesseract-5.5.5-aarch64-apple-darwin.tar.gz"
        # Replace with the real archive SHA-256 once tag tesseract-5.5.5 is published on GitHub.
        sha256 "0000000000000000000000000000000000000000000000000000000000000000"
      end
    end
  end
```

Replace `def install ... end` with:

```ruby
  def install
    if OS.mac? && Hardware::CPU.arm?
      resource("tesseract").stage do
        (libexec/"lib").install "lib/libtesseract.dylib"
        (libexec/"tessdata").install Dir["tessdata/*.traineddata"]
      end
      ENV["FAST_DOCS_TESSERACT_LIB_DIR"] = (libexec/"lib").to_s
      ENV["FAST_DOCS_PORTABLE_RPATH"] = "1"
      system "cargo", "install", "--features", "ocr", *std_cargo_args
    else
      system "cargo", "install", *std_cargo_args
    end
  end
```

At the end of `test do`, after `assert_match "fast-docs", output`, add:

```ruby

    if OS.mac? && Hardware::CPU.arm?
      port = free_port
      pid = spawn bin/"fast-docs", "--http", "127.0.0.1:#{port}"
      begin
        sleep 2
        health = shell_output("curl -s http://127.0.0.1:#{port}/health")
        assert_match '"engine":"tesseract"', health
        assert_match '"tessdata":"ok"', health
      ensure
        Process.kill("TERM", pid)
        Process.wait(pid)
      end
    end
```

- [ ] **Step 2: Check style**

Run: `brew style Formula/fast-docs.rb`
Expected: `1 file inspected, no offenses detected`. If a cop rejects `if OS.mac? && Hardware::CPU.arm?` inside `install` or `test`, replace each such `if/else` with Homebrew's on-system blocks, for example in `install`:

```ruby
  def install
    on_macos do
      on_arm do
        resource("tesseract").stage do
          (libexec/"lib").install "lib/libtesseract.dylib"
          (libexec/"tessdata").install Dir["tessdata/*.traineddata"]
        end
        ENV["FAST_DOCS_TESSERACT_LIB_DIR"] = (libexec/"lib").to_s
        ENV["FAST_DOCS_PORTABLE_RPATH"] = "1"
      end
    end
    features = (OS.mac? && Hardware::CPU.arm?) ? ["--features", "ocr"] : []
    system "cargo", "install", *features, *std_cargo_args
  end
```

Then re-run `brew style Formula/fast-docs.rb` until it reports no offenses.

- [ ] **Step 3: Commit**

`brew install --build-from-source` and `brew test` cannot run yet: the `tesseract-5.5.5` asset and its `sha256` do not exist (spec Q-OCR-1, `MEMORY.md` Q-01). Do not claim them as verified.

```bash
git add Formula/fast-docs.rb
prek run
git commit -m "Install Tesseract OCR resource on Apple Silicon in the formula" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 7: Documentation, mutation testing, and final verification

**Files:**
- Modify: `MEMORY.md` (new D-21 after D-20, new Q-OCR-1 under Open Questions)
- Modify: `README.md` (lines 6, 7, 10, 22, 39, 138, 152, 161-167, 194-208, 210-227)
- Modify: `ROADMAP.md` (Milestone 3)

**Interfaces:**
- Consumes: everything above.
- Produces: documentation that matches the code, and verification evidence.

- [ ] **Step 1: Mutation testing**

Run: `cargo mutants -f src/ocr.rs --features ocr`
Expected: every viable mutant caught. For each survivor, add a unit test in `src/ocr.rs` that kills it, re-run, and repeat until none survive. Record the final `caught/viable` count for the README.

- [ ] **Step 2: Verify stdout stays pure JSON-RPC in stdio mode**

Tesseract prints diagnostics to stderr. This check proves nothing reaches stdout, which carries MCP frames.

Run:

```bash
cargo build --features ocr
probe_dir="$(mktemp -d /tmp/fast-docs-ocr-stdio.XXXXXX)"
cp tests/fixtures/ocr/ledger.png "$probe_dir/"
{
  printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"probe","version":"1"}}}'
  printf '%s\n' '{"jsonrpc":"2.0","method":"notifications/initialized"}'
  printf '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"fetch_local_docs","arguments":{"project_dir":"%s","topic":"quantum ledger"}}}\n' "$probe_dir"
  sleep 3
} | target/debug/fast-docs --stdio 2>/dev/null | python3 -I -c '
import json, sys
frames = [json.loads(line) for line in sys.stdin if line.strip()]
assert any("Reconciliation" in json.dumps(f) for f in frames), frames
print(f"{len(frames)} frames, all valid JSON-RPC, OCR text present")
'
trash "$probe_dir"
```

Expected: `2 frames, all valid JSON-RPC, OCR text present`. A `JSONDecodeError` means something wrote non-JSON to stdout, and it must be fixed before continuing.

- [ ] **Step 3: Record the decision in `MEMORY.md`**

Append after D-20:

```markdown
### D-21: Native-Linked Tesseract OCR for Local Images (macOS Apple Silicon)
- **Status:** Accepted (2026-11-27)
- **Decision:** Add an opt-in `ocr` Cargo feature that, on `aarch64-apple-darwin` only, links a standalone `libtesseract.dylib` (Tesseract 5.5.5) through `build.rs` (`-ltesseract`, `@loader_path/../libexec/lib` rpath, `cfg(fast_docs_ocr)`). The Homebrew formula installs the dylib and `eng.traineddata` from the `tesseract-5.5.5` GitHub Release asset into `libexec/`. `fetch_local_docs` OCRs up to 16 images per call (`.png`, `.jpg`, `.jpeg`, `.webp`, `.tif`, `.tiff`, `.gif`), decoded in safe Rust with `image` 0.25.10 (8192 px per side, 16 Mi px, 32 px minimum), keeps results with mean confidence ≥ 60, caches them in `DiskCache` keyed by content SHA-256 + language + Tesseract version (7-day TTL), and runs the scan in `spawn_blocking`.
- **Rationale:** Native `LC_LOAD_DYLIB` linking with a Homebrew-verified, Cellar-installed dylib avoids the runtime-download-then-`dlopen` pattern that Gatekeeper, AMFI, and EDR agents flag. Measured on 2026-11-27: `brew install tesseract` pulls 25 formulae, but `libtesseract.5.dylib` only depends on `libleptonica` and `libarchive`. Linking Homebrew's static archives yields a 7.3 MiB dylib whose `otool -L` lists only `/usr/lib/*` and `Accelerate.framework`, and the C API (`TessVersion`, `TessBaseAPIInit3`, `TessBaseAPIGetUTF8Text`, `TessBaseAPIMeanTextConf`) runs from it. On a 1200×120 test image, OCR took 90 ms with confidence 95, and a blank image returned confidence 0. Decoding in Rust instead of Leptonica's C decoders keeps malformed image parsing out of C code.
- **Exception to D-05:** `unsafe` now exists in the crate, confined to the FFI boundary in `src/ocr.rs` (RAII `TessHandle` and `TessText`, `// SAFETY:` comments on every block). Builds without `cfg(fast_docs_ocr)` still compile zero `unsafe` blocks.
```

Under `## Open Questions`, append:

```markdown
- **Q-OCR-1 (`tesseract-5.5.5` release asset):** Publish `fast-docs-tesseract-5.5.5-aarch64-apple-darwin.tar.gz` (`lib/libtesseract.dylib` with `LC_ID_DYLIB` `@rpath/libtesseract.dylib`, ad-hoc signed, `otool -L` limited to `/usr/lib` and `/System/Library/Frameworks`; `tessdata/eng.traineddata`) under tag `tesseract-5.5.5`, replace the placeholder `sha256` of the `tesseract` resource in `Formula/fast-docs.rb`, then validate with `brew install --build-from-source` and `brew test fast-docs` (which checks `/health` reports `"engine":"tesseract"` and `"tessdata":"ok"`).
```

- [ ] **Step 4: Update `README.md`**

Make these exact changes:

1. Line 6: replace the badge with `[![Rust: unsafe confined to FFI](https://img.shields.io/badge/unsafe-FFI%20only%20(src%2Focr.rs)-brightgreen.svg)](#security-by-design--verified-by-tests)`.
2. Line 7: replace `81` (badge text and URL) with the test count that `cargo test --features ocr` reports in Step 6.
3. Line 10: after "with **zero runtime dependencies**", insert "(Apple Silicon Homebrew builds also ship a bundled `libtesseract.dylib` for on-device OCR)".
4. Line 22 (table row): append `, and **images** (`.png`, `.jpg`, `.webp`, `.tiff`, `.gif`) through on-device **Tesseract OCR** on Apple Silicon` before `on disk without uploading a single byte`.
5. Line 39 (binary size row): change `(no Node, Python, OpenSSL, or C FFI)` to `(no Node, Python, or OpenSSL; Apple Silicon Homebrew builds add a 7.3 MiB libtesseract.dylib and 3.9 MiB eng.traineddata in libexec/)`.
6. Line 138 (`fetch_local_docs` row): add `, and images via Tesseract OCR (Apple Silicon Homebrew builds, up to 16 per call, cached by content hash)` after `**`.pdf`**`.
7. Line 152: replace "contains **zero `unsafe` blocks**" with "confines all `unsafe` code to the Tesseract FFI boundary in `src/ocr.rs` (zero `unsafe` blocks in builds without OCR)".
8. In the resource caps bullet (line 161), append: "Images are decoded in safe Rust (`image`), rejected above 8192 px per side or 16 Mi px from the header before decoding, and OCR text below confidence 60 is dropped."
9. Line 165: update the test count and add `OCR (fixture text, corrupt/oversized/blank/icon images, 16-image cap, cache read/write)` to the list. Line 166: replace the `src/cache.rs` mutant sentence with both results (`src/cache.rs` and `src/ocr.rs` from Step 1).
10. Repository layout table: add rows `| `src/ocr.rs` | Tesseract FFI boundary (only `unsafe` in the crate), image bounds, OCR cache keys, `/health` OCR status |` and `| `build.rs` | Links `libtesseract.dylib` and emits `cfg(fast_docs_ocr)` for `--features ocr` on Apple Silicon |`, and update the `tests/` row count.
11. Development commands block: add, before `cargo build --release`:

    ```bash
    # Apple Silicon: OCR build and tests (needs `brew install tesseract`; also required by the
    # `--all-features` clippy hook)
    cargo test --features ocr
    ```

- [ ] **Step 5: Update `ROADMAP.md`**

Under `## Milestone 3`, append:

```markdown
- [x] On-device Tesseract OCR for repository images in `fetch_local_docs` on Apple Silicon (`--features ocr`, native-linked `libtesseract.dylib`, content-hash cache, `spawn_blocking` scan).
```

Under `## Milestone 4`, append:

```markdown
- [ ] Publish the `tesseract-5.5.5` release asset, set the `tesseract` resource `sha256` in `Formula/fast-docs.rb`, and validate with `brew install --build-from-source` and `brew test` (Q-OCR-1).
```

- [ ] **Step 6: Full verification in both modes**

Run each command and keep its output for the summary:

```bash
cargo fmt -- --check
cargo clippy --all-targets -- -D warnings
cargo clippy --all-targets --all-features -- -D warnings
cargo test
cargo test --features ocr
cargo deny check
actionlint
zizmor .github/workflows/
brew style Formula/fast-docs.rb
rg -n '\bunsafe\b' src --glob '!src/ocr.rs'
prek run --all-files
```

Expected: every command succeeds, and `rg` prints nothing (no `unsafe` outside `src/ocr.rs`). Use the test totals from `cargo test --features ocr` for README items 2 and 9. If they differ from what Step 4 wrote, fix the README now.

- [ ] **Step 7: Commit**

```bash
git add MEMORY.md README.md ROADMAP.md src/ocr.rs
prek run
git commit -m "Document Tesseract OCR design, limits, and release follow-up" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

(`src/ocr.rs` is included in case Step 1 added mutant-killing tests.)
