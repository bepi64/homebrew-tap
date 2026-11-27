//! Links the optional Tesseract OCR library on macOS Apple Silicon (`--features ocr`).

use std::env;
use std::path::PathBuf;

/// Homebrew's `tesseract` keg, used for local development builds.
const DEV_TESSERACT_LIB_DIR: &str = "/opt/homebrew/opt/tesseract/lib";

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
    println!("cargo:rustc-link-arg=-Wl,-weak_library,{}/libtesseract.2.dylib", lib_dir.display());
    println!("cargo:rustc-link-arg=-Wl,-undefined,dynamic_lookup");
    // Homebrew layout: bin/fast-docs loads libexec/lib/libtesseract.dylib.
    println!("cargo:rustc-link-arg=-Wl,-rpath,@loader_path/../libexec/lib");
    // Development builds also need the build-machine directory; Homebrew builds must not leak it.
    if env::var("FAST_DOCS_PORTABLE_RPATH").as_deref() != Ok("1") {
        println!("cargo:rustc-link-arg=-Wl,-rpath,{}", lib_dir.display());
    }
    Ok(())
}
