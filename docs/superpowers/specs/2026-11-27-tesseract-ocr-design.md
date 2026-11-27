# Tesseract OCR for `fetch_local_docs` (macOS Apple Silicon)

- **Date:** 2026-11-27
- **Status:** Approved design, not yet implemented
- **Scope:** `aarch64-apple-darwin` only. Linux and Intel macOS builds keep the current pure-Rust behavior.

## Goal

Let `fetch_local_docs` extract text from images stored in a repository (screenshots, diagrams, scanned figures) using Tesseract, shipped as a prebuilt standalone `libtesseract.dylib` hosted on a GitHub Release of this repository and installed unconditionally by the Homebrew formula on Apple Silicon.

This is also the first concrete case of a reusable pattern: distributing a native library through a GitHub Release and linking it the standard way, without runtime downloads.

## Non-goals

- OCR of scanned PDFs (requires page rasterization).
- `.svg` files (XML, not pixels).
- Linux, Intel macOS, and the prebuilt binaries produced by `.github/workflows/release.yml`.
- A dedicated `ocr_image` MCP tool or an `include_images` opt-in flag: OCR is integrated into `fetch_local_docs` and always on when compiled in.
- Building the standalone dylib. The maintainer builds it manually.

## Feasibility (verified 2026-11-27)

`brew install tesseract` (5.5.5) pulls 25 formulae, but `libtesseract.5.dylib` itself only depends on `libleptonica` and `libarchive`. Homebrew ships static archives for the whole chain. Linking them with:

```bash
clang++ -dynamiclib -o libtesseract.dylib \
  -install_name @rpath/libtesseract.dylib \
  -Wl,-force_load,/opt/homebrew/opt/tesseract/lib/libtesseract.a \
  .../libleptonica.a .../libarchive.a .../libpng16.a .../libjpeg.a .../libtiff.a \
  .../libwebp.a .../libsharpyuv.a .../libwebpmux.a .../libgif.a .../libopenjp2.a \
  .../libzstd.a .../liblzma.a .../liblz4.a .../libb2.a \
  -framework Accelerate -lcurl -lz -lbz2 -liconv -lexpat -lxml2
```

produced a 7.3 MiB dylib whose `otool -L` lists only `/usr/lib/*` and `/System/Library/Frameworks/Accelerate.framework`. Loading it and running `TessVersion` (`5.5.5`), `TessBaseAPICreate`, `TessBaseAPIInit3` with `eng.traineddata` (3.9 MiB, `rc = 0`), `TessBaseAPISetImage`, `TessBaseAPIGetUTF8Text`, `TessBaseAPIEnd`, and `TessBaseAPIDelete` succeeded.

## Why native linking instead of `dlopen` or runtime download

Security tools (Gatekeeper, AMFI, EDR agents) flag binaries that download a library at runtime into a user-writable directory and then load it, because that pattern matches droppers and dylib hijacking. Installing the dylib through Homebrew (checksum verified, quarantine handled, ad-hoc signed on Apple Silicon) into the immutable Cellar and recording it as a regular `LC_LOAD_DYLIB` dependency makes `fast-docs` look like any other linked binary. `dyld` resolves it at launch through `@loader_path`.

## 1. Distribution and linking

### Release asset

- Published under a dedicated tag `tesseract-5.5.5`, independent of `fast-docs` versions. `release.yml` only triggers on `v*.*.*`, so this tag does not start a `fast-docs` release. Upgrading Tesseract only requires a formula change.
- Archive: `fast-docs-tesseract-5.5.5-aarch64-apple-darwin.tar.gz`, plus its `.sha256`:

  ```text
  lib/libtesseract.dylib
  tessdata/eng.traineddata
  ```

- Requirements on `libtesseract.dylib`:
  - `LC_ID_DYLIB` is `@rpath/libtesseract.dylib`.
  - `otool -L` lists nothing outside `/usr/lib/` and `/System/Library/Frameworks/`.
  - Exports the C API symbols listed in section 2.
  - Signed at least ad-hoc (`codesign -s -`).

### `Formula/fast-docs.rb`

```ruby
on_macos do
  on_arm do
    resource "tesseract" do
      url "https://github.com/bepi64/homebrew-tap/releases/download/tesseract-5.5.5/fast-docs-tesseract-5.5.5-aarch64-apple-darwin.tar.gz"
      sha256 "<sha256 of the published archive>"
    end
  end
end
```

`install`:

- On `OS.mac? && Hardware::CPU.arm?`:
  - Stage the resource: `lib/libtesseract.dylib` into `libexec/"lib"`, `tessdata/*.traineddata` into `libexec/"tessdata"`.
  - Set `ENV["FAST_DOCS_TESSERACT_LIB_DIR"] = libexec/"lib"` and `ENV["FAST_DOCS_PORTABLE_RPATH"] = "1"`.
  - `system "cargo", "install", "--features", "ocr", *std_cargo_args`.
- Otherwise: `system "cargo", "install", *std_cargo_args` (unchanged).

### `Cargo.toml` and `build.rs`

- New opt-in feature: `ocr = ["dep:image"]`, off by default.
- `image` is an optional dependency with default features disabled and only the `png`, `jpeg`, `webp`, `tiff`, and `gif` codecs enabled. Its licenses and those of its codec crates are checked with `cargo deny check`; `deny.toml` is updated only if a new license must be allowed.
- New `build.rs`. When `CARGO_FEATURE_OCR` is set, `CARGO_CFG_TARGET_OS == "macos"`, and `CARGO_CFG_TARGET_ARCH == "aarch64"`, it:
  1. Emits `cargo:rustc-cfg=fast_docs_ocr`. It always emits `cargo:rustc-check-cfg=cfg(fast_docs_ocr)`.
  2. Resolves the library directory: `FAST_DOCS_TESSERACT_LIB_DIR` first, else `/opt/homebrew/opt/tesseract/lib` if it exists (local development), else fails with a message naming both options.
  3. Emits `cargo:rustc-link-search=native=<dir>` and `cargo:rustc-link-lib=dylib=tesseract`.
  4. Always emits `cargo:rustc-link-arg=-Wl,-rpath,@loader_path/../libexec/lib`. `@loader_path` is the real Cellar path of `bin/fast-docs`, even when launched through the `/opt/homebrew/bin` symlink.
  5. Unless `FAST_DOCS_PORTABLE_RPATH == "1"`, also emits `-Wl,-rpath,<dir>` so `cargo test` finds the library without `DYLD_LIBRARY_PATH`. Homebrew builds set the variable, so no build-machine path leaks into `LC_RPATH`.
  6. Emits `cargo:rerun-if-env-changed` for both variables.

All OCR code is gated on `#[cfg(fast_docs_ocr)]`. CI on Ubuntu runs `cargo clippy --all-targets --all-features`, which enables the `ocr` feature, but `fast_docs_ocr` is not emitted on Linux, so no OCR code is compiled or linked there.

## 2. FFI boundary (`src/ocr.rs`)

All `unsafe` code in the crate lives in this module. This is an explicit, documented exception to D-05 (zero `unsafe`).

### Declared C symbols (10)

`TessVersion`, `TessBaseAPICreate`, `TessBaseAPIInit3`, `TessBaseAPISetImage`, `TessBaseAPISetSourceResolution`, `TessBaseAPIGetUTF8Text`, `TessBaseAPIMeanTextConf`, `TessDeleteText`, `TessBaseAPIEnd`, `TessBaseAPIDelete`.

They are declared by hand in an `extern "C"` block: no `-sys` crate, no `bindgen`, no `libclang`. No Leptonica symbol is used.

### RAII wrappers

- `TessHandle(NonNull<TessBaseAPI>)`: created by `TessBaseAPICreate` + `TessBaseAPIInit3`; `Drop` calls `TessBaseAPIEnd` then `TessBaseAPIDelete`. It is neither `Send` nor `Sync`.
- `TessText(NonNull<c_char>)`: owns the string returned by `TessBaseAPIGetUTF8Text`; `Drop` calls `TessDeleteText`. It is converted with `CStr::to_string_lossy`.
- Every `unsafe` block carries a `// SAFETY:` comment covering non-null pointers, the pixel buffer outliving `SetImage` through `GetUTF8Text`, and NUL-terminated `CString` arguments.

### Image decoding and bounds

Images are decoded in safe Rust with `image`, never by the C decoders bundled in the dylib. Checks run in this order:

1. File size ≤ `MAX_RICH_DOC_FILE_BYTES` (5 MiB, existing constant).
2. Header dimensions read before full decoding: width ≤ 8192 px, height ≤ 8192 px, total ≤ 16 Mpx. A header announcing 100,000 × 100,000 px is rejected without allocating the pixel buffer.
3. Images with a side under 32 px (icons) are skipped.
4. Decoding with `image::Limits` matching the bounds above, then conversion to 8-bit grayscale (`into_luma8`).
5. `TessBaseAPISetImage(handle, buf, w, h, 1, w)` and `TessBaseAPISetSourceResolution(handle, 70)`.
6. Results with `TessBaseAPIMeanTextConf` below 60 are dropped.
7. Output truncated to `MAX_EXTRACTED_BYTES` (2 MiB) on a UTF-8 boundary.

### `tessdata` resolution

No runtime download. First match wins:

1. `FAST_DOCS_TESSDATA_DIR`.
2. `libexec/tessdata` next to the executable's `bin/` directory, computed from the canonicalized `std::env::current_exe()`. Canonicalization matters: Homebrew launches the binary through the `/opt/homebrew/bin/fast-docs` symlink, and only the resolved Cellar path has a sibling `libexec/`.
3. `/opt/homebrew/opt/tesseract/share/tessdata` (local development).

The language defaults to `eng`. It must match `^[A-Za-z0-9_]+(\+[A-Za-z0-9_]+)*$` before reaching `TessBaseAPIInit3`.

## 3. Integration

### Collection (`src/local_docs.rs`, `src/extractors.rs`)

- Image extensions `png`, `jpg`, `jpeg`, `webp`, `tif`, `tiff`, `gif` are recognized only under `fast_docs_ocr`. Elsewhere they are not collected, so output is unchanged.
- `MAX_OCR_IMAGES = 16` per call, separate from `MAX_LOCAL_FILES = 200`. Images follow the existing priority order (`README` > `docs/` > the rest). Images beyond the cap are skipped with `tracing::debug!`.
- Each image yields one `DocSection` titled `"<rel_path> (OCR)"`, then goes through the existing topic ranking and pagination.

### Execution

- `fetch_local_docs` moves its call to `search_local_docs` into `tokio::task::spawn_blocking`. Today the scan runs synchronously inside the async handler (`src/tools.rs`), which would block a runtime thread of the shared daemon for the full OCR duration. A `JoinError` is reported as a tool error message, consistent with existing handlers.
- One `TessHandle` per call, created lazily on the first image and reused for the remaining images of that call. `TessBaseAPIInit3` therefore runs at most once per scan. There is no global handle, so concurrent scans from different agents do not contend on a lock.

### Cache

- OCR text is stored in the existing `DiskCache` under the key `ocr:<SHA-256 of file bytes>:<lang>:<TessVersion>` with the pinned TTL (7 days).
- Because the key is derived from content, a modified image produces a new key and no invalidation logic is needed. Repeated calls with another `topic` or `offset` never re-run OCR.
- Images dropped by the confidence filter are cached as empty results so they are not re-processed.

### Failure handling

- Missing `tessdata` or a failing `TessBaseAPIInit3`: one `tracing::warn!` per process, images are skipped, and the rest of the scan proceeds.
- A corrupt, oversized, or undecodable image: `tracing::debug!`, image skipped, consistent with existing extractors.

### Observability

- `GET /health` gains an `ocr` field:
  - `{"engine":"tesseract","version":"5.5.5","lang":"eng","tessdata":"ok"}` under `fast_docs_ocr`, where `tessdata` is `"ok"` when `eng.traineddata` exists (a single `stat`, no initialization) and `"missing"` otherwise.
  - `null` otherwise.
- The `fetch_local_docs` tool description mentions images "when built with OCR support".

## 4. Testing

Tests are written first and observed failing (TDD, per `AGENTS.md`).

### Unit tests (`src/ocr.rs`, under `fast_docs_ocr`)

- Language validation: accepts `eng`, `chi_sim`, `eng+fra`; rejects `""`, `../x`, `a/b`, `eng+`, and strings containing `\0`.
- `tessdata` resolution order.
- Dimension bounds (8192 px per side, 16 Mpx total, 32 px minimum), including exact boundary values.
- Confidence threshold (59 dropped, 60 kept).
- UTF-8-safe truncation at 2 MiB.
- Cache key composition.

### Integration tests (`tests/ocr.rs`, under `fast_docs_ocr`)

Fixtures in `tests/fixtures/ocr/`, generated once by a documented script and committed (a few KiB each):

- A PNG with known text is found by `fetch_local_docs` with a matching `topic`, in a section titled `(OCR)`.
- A corrupt PNG and a PNG whose header announces 100,000 × 100,000 px are skipped without failing the scan.
- A blank image and a 16 × 16 icon produce no section.
- A directory of 20 images yields at most 16 OCR sections.
- A second identical call is served from the cache.
- `/health` reports the `ocr` object.

### Non-regression (all platforms)

Without `fast_docs_ocr`, a repository containing `.png` files renders exactly as before, and `/health` reports `"ocr": null`.

### Mutation testing

`cargo mutants -f src/ocr.rs` with no surviving viable mutants on the bounds, validation, and filtering logic.

## 5. CI, formula test, and documentation

- **CI (`.github/workflows/ci.yml`):** the Ubuntu job is unchanged. A new `macos-15` job runs `brew install tesseract`, then `cargo clippy --all-targets --features ocr -- -D warnings` and `cargo test --locked --features ocr`, using the local-development fallback path. Actions are SHA-pinned and checked with `actionlint` and `zizmor`. The standalone dylib itself is validated by `brew install --build-from-source` and `brew test` (Q-01 in `MEMORY.md`), which avoids duplicating its checksum in the workflow.
- **Formula `test do`:** the existing MCP `initialize` test already proves the dylib links, because `dyld` refuses to launch the binary if `@rpath/libtesseract.dylib` cannot be resolved. On ARM, the test additionally starts the daemon on a free loopback port, requests `/health`, and asserts `"engine":"tesseract"` and `"tessdata":"ok"`. Formula changes are checked with `brew style` and `brew audit --strict`.
- **`MEMORY.md`:** new ADR D-21 recording this decision, the measurements above, and the scoped exception to D-05.
- **`README.md`:** the "unsafe 0 blocks" badge and the "zero `unsafe` blocks" statement are reworded to "zero `unsafe` outside the `src/ocr.rs` FFI boundary"; add an OCR section listing supported image formats and the macOS Apple Silicon restriction.
- **`ROADMAP.md`:** new task under Milestone 3.

## Open questions

- **Q-OCR-1:** the archive `sha256` is unknown until the maintainer publishes the `tesseract-5.5.5` release asset. The formula change cannot be finalized or validated with `brew install` before then; Rust-side work can proceed against the local Homebrew `tesseract` install.
