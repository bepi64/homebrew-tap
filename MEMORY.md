# MEMORY.md

Architecture Decision Records (ADRs) and open questions for `fast-docs` (`homebrew-tap`). Record both confirmed decisions and lessons learned here, focusing on the *why* that cannot be inferred from the code alone.

## Architecture Decision Records

### D-01: Client-Server Rust MCP Server Over Stdio (`rmcp` + `rustls-tls`)
- **Status:** Accepted
- **Decision:** Implement `fast-docs` as a standalone Rust MCP server (`FastDocsServer`) speaking JSON-RPC over `stdio` via `rmcp`, `tokio`, `reqwest` (`rustls-tls`, default features disabled), and `scraper`. AI assistants such as Claude act as the MCP client invoking `fetch_docs` and `search_docs`.
- **Rationale:** Native compilation yields fast startup and low memory overhead when spawned by an MCP client. Using `rustls-tls` avoids OpenSSL system library mismatches across macOS and Linux builds. Logging goes strictly to `stderr` via `tracing-subscriber` so `stdout` remains reserved for MCP JSON-RPC frames.

### D-02: SHA-256 Hashed Keys for Local Disk Cache
- **Status:** Accepted (updated 2026-11-26)
- **Decision:** Store cached documentation entries under `~/.cache/fast-docs/indexes/` using the first 16 bytes (32 lowercase hex characters) of `SHA-256("c7v2:<url>")` as the filename, wrapped in a versioned JSON envelope (`{ version, fetched_at_secs, pinned, content }`) written atomically via temp-file rename.
- **Rationale:** Hashing the canonical URL eliminates directory traversal from untrusted library names while keeping filenames portable across filesystems. Increasing from 8 bytes to 16 bytes reduces collision probability to negligible levels, and atomic rename prevents partial reads across concurrent clients.

### D-03: Standardized AI Assistance Conventions in Technical English
- **Status:** Accepted (2026-11-26)
- **Decision:** Maintain all AI development instructions (`AGENTS.md`, `CLAUDE.md`, `.agents/rules/*.md`) and core project documentation (`README.md`, `MEMORY.md`, `ROADMAP.md`) in concise technical English. Scope language- and domain-specific rules via `paths:` frontmatter in `.agents/rules/` (`rust.md`, `ruby.md`, `bash.md`, `github-actions.md`, `security.md`).
- **Rationale:** Keeps conventions aligned with the actual stack (Rust MCP server + Homebrew tap on GitHub) and eliminates contradictions introduced by an earlier Python/GitLab template.

### D-04: Local Quality Gates via `prek`
- **Status:** Accepted (2026-11-26)
- **Decision:** Use `prek` (`prek.toml`) to enforce file hygiene, secret detection, Rust formatting/linting, Bash linting, and Homebrew formula style checks locally before commit.
- **Rationale:** `prek` runs built-in hygiene checks natively in Rust with zero Python runtime dependency and executes local toolchain hooks (`cargo fmt`, `cargo clippy`, `shellcheck`, `shfmt`, `brew style`) consistently before commits.

### D-05: Pure-Rust Cache System Replacing C FFI (`cache_prefetch_init`)
- **Status:** Accepted (2026-11-26)
- **Decision:** Remove `extern "C" { fn cache_prefetch_init() -> i32; }`, the associated `unsafe` call in `src/main.rs`, and the unused `cc` build dependency in `Cargo.toml`. Implement disk caching, TTL validation, and expired-entry pruning entirely in safe Rust (`src/cache.rs`).
- **Rationale:** The missing C symbol broke `cargo build` linking (`Undefined symbols: _cache_prefetch_init`) and introduced an unnecessary `unsafe` FFI surface. A pure-Rust `DiskCache` eliminates `unsafe` across the entire crate, enables deterministic unit testing with injected timestamps, and simplifies cross-platform release builds and Homebrew packaging.

### D-06: Evidence-Based Hardening Bounds (Response Cap, Timeouts, Cache TTL, SSRF Guard)
- **Status:** Accepted (2026-11-26)
- **Decision:** Enforce the following concrete bounds in `src/tools.rs` and `src/cache.rs`:
  - `MAX_RESPONSE_BYTES = 5 MiB` (5,242,880 bytes), checked both on `Content-Length` and incrementally while streaming chunks.
  - `REQUEST_TIMEOUT = 10 s` and `CONNECT_TIMEOUT = 5 s`.
  - `DEFAULT_CACHE_TTL = 24 hours` for mutable (`latest`) docs and `PINNED_CACHE_TTL = 7 days` for version-pinned docs.
  - Outbound HTTPS host allowlist (`docs.rs`, `pypi.org`, `pkg.go.dev`, `api.github.com`) validated on initial URLs and on every redirect hop (max 5 redirects), with strict character/path validation on `library` and `version`.
- **Evidence & Sources:**
  1. **Response size cap (5 MiB):**
     - *OWASP API4:2023 — Unrestricted Resource Consumption* (`https://api-security.owasp.org/editions/2023/en/0xa4-unrestricted-resource-consumption`) mandates explicit payload size caps to prevent memory exhaustion.
     - *HTTP Archive Web Almanac 2024 — Page Weight* (`https://almanac.httparchive.org/en/2024/page-weight`, Fig. 16.17–16.19 & 16.37) reports a median HTML document transfer size of 18 KB and a 75th-percentile total page weight (all assets) of 4,169 KB (~4.07 MiB).
     - *Direct empirical measurements of `fast-docs` upstream endpoints (2026-11-26):* `docs.rs/serde/latest/serde/` = 25.0 KiB, `docs.rs/tokio/latest/tokio/` = 57.5 KiB, `api.github.com` search (6 items) = 37.3 KiB, `pkg.go.dev/github.com/gin-gonic/gin` = 420.7 KiB, `pypi.org/pypi/django/json` = 615.8 KiB, and `pypi.org/pypi/boto3/json` (one of PyPI's largest release histories) = **3.19 MiB**. Setting the cap at **5 MiB** provides ~1.57× headroom above `boto3` while bounding memory strictly against hostile or runaway responses. (Note: PyPI removed the `releases` map from version-pinned JSON endpoints for stability per `https://docs.pypi.org/api/json/`, so pinned requests are much smaller.)
  2. **Timeouts (10 s total / 5 s connect):**
     - *Jakob Nielsen, Usability Engineering (1993)* — citing Miller (1968) and Card, Robertson & Mackinlay (1991) (`https://www.nngroup.com/articles/response-times-3-important-limits/`) — establishes **10 seconds** as the empirical ceiling for keeping a user's attention within an interactive dialogue.
     - *OWASP API4:2023* requires explicit execution/network timeouts. A 5-second connect timeout fails fast on unreachable hosts without blocking the full 10-second budget.
  3. **Two-tier cache TTL (24 h mutable / 7 d pinned):**
     - *RFC 9111 §4.2 (HTTP Caching — Freshness)* (`https://www.rfc-editor.org/rfc/rfc9111.html`) separates explicit origin freshness from client/offline heuristic freshness.
     - *GitHub REST API Rate Limits* (`https://docs.github.com/en/rest/using-the-rest-api/rate-limits-for-the-rest-api`) restrict unauthenticated clients to **60 requests/hour per IP** (and Search endpoints even further).
     - Live upstream headers show `pypi.org` serves `Cache-Control: max-age=900, public` (15 min CDN freshness) and `docs.rs` serves `Cache-Control: max-age=0` with `ETag`. For local coding-assistant lookups, a 24-hour TTL for `latest` balances daily release freshness against repeated network latency and rate limits, while a 7-day TTL for explicitly pinned versions leverages release immutability.
  4. **SSRF & redirect validation:**
     - *OWASP SSRF Prevention Cheat Sheet* (`https://cheatsheetseries.owasp.org/cheatsheets/Server_Side_Request_Forgery_Prevention_Cheat_Sheet.html`) prescribes strict domain/scheme allowlists, rejecting parser-confusion characters (`@`, `\`, `..`), and preventing unvalidated redirects. Validating every redirect hop against the HTTPS allowlist satisfies OWASP while preserving legitimate same-host `docs.rs` `/latest` redirects.

### D-07: Clean Release Builds Without Build Cache & Native `gh release`
- **Status:** Accepted (2026-11-26)
- **Decision:** Omit `Swatinem/rust-cache` from `.github/workflows/release.yml` and publish GitHub Releases using the runner's built-in `gh release create` inside a dedicated `publish` job (the only job granted `contents: write`).
- **Rationale:** Surfaced directly by `zizmor` (`cache-poisoning` and `superfluous-actions` audits): building tagged releases from a clean workspace eliminates cache-poisoning risk on distributed binaries, and using built-in `gh` removes a third-party write-scoped action from the release supply chain.

### D-08: Upgrade `rmcp` (`0.2.1` → `3.12.0`), `schemars` (`0.8` → `1.2.3`), and `url` (`2.5.8` → `2.5.9`)
- **Status:** Accepted (2026-11-26)
- **Decision:** Upgrade `rmcp` to `3.11` (locked at `3.12.0`), `schemars` to `1` (`1.2.3`), and `url` to `2.5.9`, migrating `FastDocsServer` to `rmcp::handler::server::wrapper::Parameters`, `ContentBlock`, `ErrorData`, `ServerConfig`, and `#[tool_handler(router = self.tool_router)]`.
- **Rationale:** Resolved three RustSec advisories surfaced by `cargo deny check` (`RUSTSEC-2026-0189` DNS rebinding in `rmcp` HTTP transport, `RUSTSEC-2026-0462` unbounded response buffering in `rmcp` HTTP clients, and `RUSTSEC-2026-0592` stack overflow in `url::Url::origin()` on nested `blob:` URLs) without adding advisory ignore entries, and enabled advertising `serverInfo.name = "fast-docs"` explicitly.

### D-09: Structured Section Caching for Sub-Millisecond Multi-Topic Filtering
- **Status:** Accepted (2026-11-26)
- **Decision:** Cache parsed documentation sections (`ParsedDoc`) rather than pre-truncated Context7 text in `DiskCache`, and expose `module`, `topic`, and `max_chars` (`500..=20,000`, default `6,500`) on `fetch_docs`.
- **Rationale:** Storing parsed sections allows a single upstream fetch per page/submodule to serve unlimited subsequent `topic` and `max_chars` queries locally in sub-millisecond time without extra network calls. On `docs.rs`, `module` normalizes Rust `::` paths to `/` and crate hyphens to `_` while strictly rejecting traversal and SSRF characters. On `pypi.org` (single JSON document per package), `module` is folded into `topic` to target matching README sections.

### D-10: Parallel `search_docs`, Transparent `gzip`, and RFC 9111 `ETag` Revalidation
- **Status:** Accepted (2026-11-26)
- **Decision:** Enable `reqwest`'s `gzip` feature, parallelize `search_docs` (and `search_python`'s PyPI/GitHub lookups) with `tokio::join!` plus a 15-minute `SEARCH_CACHE_TTL`, and store HTTP `ETag` validators in `DiskCache` to issue conditional `If-None-Match` requests (`304 Not Modified`) when cached entries expire.
- **Rationale:** Benchmarks showed sequential `search_docs` taking ~1,000–1,200 ms across registries and `pypi.org`/`pkg.go.dev` returning multi-hundred-KiB to multi-MiB uncompressed payloads. Parallelizing searches reduces cold latency from the sum of all upstream round-trips to the slowest single upstream, `SEARCH_CACHE_TTL` (15 min, matching PyPI's `Cache-Control: max-age=900`) shields GitHub's 60 req/h unauthenticated quota, `gzip` cuts wire transfer while `MAX_RESPONSE_BYTES` caps the decompressed stream against gzip bombs, and RFC 9111 §4.3 `ETag` revalidation avoids re-downloading unchanged `docs.rs`/`pypi.org` pages after TTL expiry.

### D-11: npm Ecosystem via `unpkg.com` READMEs + `registry.npmjs.org` Search/Fallback
- **Status:** Accepted (2026-11-26)
- **Decision:** Add `Ecosystem::Npm` (aliases `npm`, `js`, `javascript`, `ts`, `typescript`, `node`, plus automatic detection for scoped `@scope/pkg` and common JS/TS libraries). Fetch package READMEs primarily from `https://unpkg.com/{library}@{version}/README.md` (with fallback to `readme.md` and `https://registry.npmjs.org/{pkg}`) and search via `https://registry.npmjs.org/-/v1/search`.
- **Rationale:** Live measurements showed `registry.npmjs.org/{pkg}/{version}` omits the `readme` field while full packuments (`registry.npmjs.org/react` = 6.84 MiB, `zod` = 4.18 MiB) can exceed our 5 MiB response cap. `unpkg.com/{pkg}@{ver}/README.md` delivers version-accurate Markdown in ~1–16 KiB (600×–6,000× smaller), supports scoped packages, and redirects cleanly within `unpkg.com`.

### D-12: DNS-Pinned `llms.txt` Support (`fetch_llms_txt`)
- **Status:** Accepted (2026-11-26)
- **Decision:** Expose a dedicated `fetch_llms_txt` MCP tool (`src/llms_txt.rs`) supporting `/llms.txt` and `/llms-full.txt` (with 404 fallback) from public HTTPS documentation sites, reusing `ParsedDoc`, `topic` ranking, `max_chars`, and `DiskCache` (`ETag` + 24 h TTL).
- **Rationale:** Follows the OWASP SSRF Prevention Cheat Sheet for non-allowlisted documentation domains: restricts paths strictly to `/llms.txt` or `/llms-full.txt`, blocks IP literals and internal suffixes (`.localhost`, `.local`, `.internal`, `.consul`, `.home.arpa`, `.lan`), resolves DNS upfront to reject any private/loopback/link-local/CGNAT/IMDS/ULA/NAT64-private IP, pins the HTTP client to the validated `SocketAddr` list via `resolve_to_addrs` to defeat DNS rebinding (TOCTOU), and restricts redirects to HTTPS on the exact same host.

### D-13: Local Project Manifest Version Resolution & Cache Management Tool
- **Status:** Accepted (2026-11-26)
- **Decision:** Add `src/manifest.rs` (`resolve_project_version` for `Cargo.lock` → `Cargo.toml`, `package.json`, `pyproject.toml`, and `go.mod`, bounded at 2 MiB per manifest with zero new dependencies), wire it into `fetch_docs` via optional `project_dir`, and expose `manage_cache` (`stats`, `prune`, `clear`) backed by `DiskCache::stats_at` and `DiskCache::clear`.
- **Rationale:** Automatically resolving the exact installed dependency version prevents documentation drift between `latest` and the version pinned in the user's repository (especially across major releases like `rmcp` or `react`), while `manage_cache` makes the local pure-Rust cache directly observable and controllable by MCP clients without shell commands.

### D-14: Byte-Offset Pagination, Local Private Docs (`fetch_local_docs`), and Loopback Streamable HTTP (`--http`)
- **Status:** Accepted (2026-11-26)
- **Decision:** Add UTF-8-safe `offset` pagination (`slice_with_pagination`) to `fetch_docs` and `fetch_llms_txt`; add `fetch_local_docs` (`src/local_docs.rs`, capped at 200 files / 512 KiB per file / depth 6 with canonicalized anti-symlink-escape and secret-file exclusions); and add optional `--http [127.0.0.1:3000]` Streamable HTTP MCP transport (`src/http_server.rs`) using `rmcp`'s built-in `StreamableHttpService` over `hyper` (`1.12`) with strict loopback bind validation and `rmcp`'s default DNS rebinding `Host` guard.
- **Rationale:** Closes the remaining parity gaps against Official MCP Fetch (pagination, Streamable HTTP) and Docs MCP Server (local/private repo documentation, Streamable HTTP) while preserving `fast-docs`'s zero-overhead `stdio` default, local-only privacy, and zero `unsafe` posture.

### D-15: Pure-Rust Rich Document Extraction (`.ipynb`, Office, OpenDocument, `.epub`, `.pdf`)
- **Status:** Accepted (2026-11-26)
- **Decision:** Add `src/extractors.rs` backed by `zip 9.8.2` (`deflate`-only, MIT) and `pdf-extract 0.12.1` (MIT) to extract `.ipynb`, `.docx`, `.pptx`, `.xlsx`, `.odt`, `.odp`, `.ods`, `.epub`, and `.pdf` inside `fetch_local_docs`, and allow the `Zlib` license in `deny.toml` for `zlib-rs`.
- **Rationale:** Achieves full document-format parity with `docs-mcp-server` in a single pure-Rust binary with zero external runtimes. Security is enforced via a 5 MiB raw file cap, 200-entry archive cap, `Read::take`-enforced 2 MiB decompressed cap (anti-zip-bomb), entity-safe XML text extraction without DTD expansion (anti-XXE), and `std::panic::catch_unwind` isolation around third-party PDF parsing so corrupted PDFs never terminate the MCP server.

### D-16: Word-Boundary Multi-Term Topic Ranking, Intra-Section Filtering, Rustdoc Tables, and Resolved `final_url`
- **Status:** Accepted (2026-11-26)
- **Decision:** Replace binary substring topic matching and hard per-section `.take(4)` / 450-char truncation with word-boundary multi-term scoring (`count_word_hits` + quadratic distinct-term coverage bonus), intra-section line/code filtering when `topic` is set, rustdoc `h2.section-header` + `dl.item-table` extraction, PyPI `project_urls` surfacing (`parse_pypi_json_doc`), and resolved `final_url` capture in `FetchOutcome::Fetched`.
- **Rationale:** Real-world testing against `tokio`, `hono.dev/llms.txt`, `requests`, and `zod` exposed that `.take(4)` + 450-char truncation discarded deep matching lines (such as `routing` in `hono.dev/llms.txt`) and produced pervasive `... (truncated)` markers well below the 6,500-char budget, while `@latest` redirects hid the actual served version (`zod@4.6.7`, `tokio/1.54.1`).

### D-17: Lockfile Priority (`uv.lock`, `package-lock.json`), Persistent PyPI Metadata Banner, and Unmatched-Topic Table of Contents
- **Status:** Accepted (2026-11-26)
- **Decision:** Prioritize `uv.lock` before `pyproject.toml` (PEP 503 matching) and `package-lock.json` (v1/v2/v3) before `package.json` in `src/manifest.rs` (capped at 5 MiB); store PyPI `version` and `project_urls` in a persistent `ParsedDoc.metadata` header rendered even when `topic` filters sections; and return a compact `Available sections:` table of contents instead of dumping unrelated overview prose when a `topic` matches zero sections.
- **Rationale:** Ensures exact locked versions across Rust, Python, and npm projects; keeps official external documentation links (e.g. ReadTheDocs) visible to MCP clients during topic-filtered Python lookups so they can follow up with `fetch_llms_txt`; and eliminates context noise when a lexical `topic` misses.

### D-18: Per-Hop DNS-Pinned `llms.txt` Redirects, Project-Aware Ecosystem Detection, and H1>H2 Breadcrumbs
- **Status:** Accepted (2026-11-26)
- **Decision:** Handle `llms.txt` redirects hop by hop (`fetch_llms_with_safe_redirects`) re-validating HTTPS, public hostname, DNS resolution, and public-IP pinning on each hop; infer ecosystem from `project_dir` manifests (`detect_project_ecosystem`) before falling back to `detect_lang`; preserve `H1 > H2` heading breadcrumbs in `parse_markdown_sections`; and append a `Modules` hint on 404 submodule lookups.
- **Rationale:** Fixes legitimate cross-host redirects like `docs.pydantic.dev/llms.txt` → `pydantic.dev/docs/validation/latest/llms.txt` without weakening SSRF or DNS-rebinding defenses, prevents homonym collisions (e.g., Python `ruff` vs Rust crate `ruff`) when `project_dir` is provided, and ensures `llms-full.txt` subsections like `# CORS Middleware` / `## Usage` retain parent topic context.

### D-19: Multi-Ecosystem QA Hardening (Lockfiles, Strict `lang`, RST/Go/Rustdoc, PyPI RSS, Anti-Changelog)
- **Status:** Accepted (2026-11-26)
- **Decision:** Following parallel E2E QA across four real repositories (`sharkdp/fd`, `encode/httpx`, `spf13/cobra`, `honojs/hono`):
  - Added `pnpm-lock.yaml`, `requirements.txt`, unique Go short-name resolution from `go.mod`, and an exact-pin policy for un-locked manifests (`DepMatch`).
  - Made `lang` validation strict (rejecting typos instead of silent fallback) and expanded Go auto-detection to vanity imports and stdlib packages.
  - Combined `div.UnitReadme-content` + `div.Documentation-content` on `pkg.go.dev`, extracted rustdoc method signatures cleanly (stripping line numbers and preserving `->`), added RST heading/`.. code-block::` parsing, preserved original fence languages, demoted changelog/migration sections, indexed local file paths, excluded `.github/`, and emitted pagination footers from `offset=0`.
  - Added a ~12 KiB `pypi.org/rss/project/<pkg>/releases.xml` fallback when unpinned `/pypi/<pkg>/json` exceeds 5 MiB (e.g. `ruff`), parallel `struct.`/`trait.`/`enum.` fallback for Rust `::` item paths (`builder::Command`, `ser::Serialize`), and cleaner `search_docs` formatting.

### D-20: Daemon-First Shared Background Service Architecture (`127.0.0.1:3000/mcp` + `brew services`)
- **Status:** Accepted (2026-11-27)
- **Decision:** Make the persistent loopback Streamable HTTP daemon (`127.0.0.1:3000/mcp`, configurable via `--http` or `FAST_DOCS_BIND`) the default execution mode of `fast-docs`, sharing a single `FastDocsServer` (`reqwest::Client` TLS/HTTP2 pool) across all concurrent MCP clients, adding `GET /health` + hourly `DiskCache::prune_expired` maintenance + Homebrew `service do` in `Formula/fast-docs.rb`, and keeping `stdio` available via `--stdio`.
- **Rationale:** Running one shared background daemon avoids spawning redundant processes when multiple Claude agents or IDEs run simultaneously, reuses a single HTTP/2 connection pool and disk cache, integrates natively with `brew services start fast-docs` / `launchd`, and preserves strict loopback-only + DNS-rebinding (`HTTP 403`) security.

### D-21: Native-Linked Tesseract OCR for Local Images (macOS Apple Silicon)
- **Status:** Accepted (2026-11-27)
- **Decision:** Add an opt-in `ocr` Cargo feature that, on `aarch64-apple-darwin` only, links a standalone `libtesseract.dylib` (Tesseract 5.5.5) through `build.rs` (`-ltesseract`, `@loader_path/../libexec/lib` rpath, `cfg(fast_docs_ocr)`). The Homebrew formula installs the dylib and `eng.traineddata` from the `tesseract-5.5.5` GitHub Release asset into `libexec/`. `fetch_local_docs` OCRs up to 16 images per call (`.png`, `.jpg`, `.jpeg`, `.webp`, `.tif`, `.tiff`, `.gif`), decoded in safe Rust with `image` 0.25.10 (8192 px per side, 16 Mi px, 32 px minimum), keeps results with mean confidence ≥ 60, caches them in `DiskCache` keyed by content SHA-256 + language + Tesseract version (7-day TTL), and runs the scan in `spawn_blocking`. `image` is declared as a target-specific dependency (`cfg(all(target_os = "macos", target_arch = "aarch64"))`) so Linux builds with `--all-features` neither compile it nor report it as unused.
- **Rationale:** Native `LC_LOAD_DYLIB` linking with a Homebrew-verified, Cellar-installed dylib avoids the runtime-download-then-`dlopen` pattern that Gatekeeper, AMFI, and EDR agents flag. Measured on 2026-11-27: `brew install tesseract` pulls 25 formulae, but `libtesseract.5.dylib` only depends on `libleptonica` and `libarchive`. Linking Homebrew's static archives yields a 7.3 MiB dylib whose `otool -L` lists only `/usr/lib/*` and `Accelerate.framework`. On a 1200×120 test image, OCR took 90 ms with confidence 95, and a blank image returned confidence 0. A simulated Cellar layout (`bin/fast-docs` launched through a symlink, `libexec/lib/libtesseract.dylib`, `libexec/tessdata/eng.traineddata`) confirmed that `dyld` loads the `libexec` dylib through the only rpath, that `tessdata` resolves to `libexec` after canonicalization, that `/health` reports the engine, and that stdio mode keeps stdout pure JSON-RPC (Tesseract writes diagnostics to stderr). Decoding in Rust instead of Leptonica's C decoders keeps malformed image parsing out of C code.
- **Exception to D-05:** `unsafe` now exists in the crate, confined to the FFI boundary in `src/ocr.rs` (RAII `TessHandle` and `TessText`, `// SAFETY:` comments on every block). Builds without `cfg(fast_docs_ocr)` still compile zero `unsafe` blocks.

## Open Questions

- **Q-01 (v1.0.0 release publication):** `Cargo.toml` and `USER_AGENT` are now at `1.0.0`. Once pushed to GitHub (`https://github.com/bepi64/homebrew-tap`) and tagged `v1.0.0`, replace the placeholder `sha256` in `Formula/fast-docs.rb` with the real release archive checksum, then run `brew audit --strict --new Formula/fast-docs.rb`, `brew install --build-from-source Formula/fast-docs.rb`, and `brew test fast-docs`.
- **Q-OCR-1 (`tesseract-5.5.5` release asset):** Publish `fast-docs-tesseract-5.5.5-aarch64-apple-darwin.tar.gz` (`lib/libtesseract.dylib` with `LC_ID_DYLIB` `@rpath/libtesseract.dylib`, ad-hoc signed, `otool -L` limited to `/usr/lib` and `/System/Library/Frameworks`; `tessdata/eng.traineddata`) under tag `tesseract-5.5.5`, replace the placeholder `sha256` of the `tesseract` resource in `Formula/fast-docs.rb`, then validate with `brew install --build-from-source` and `brew test fast-docs` (which checks `/health` reports `"engine":"tesseract"` and `"tessdata":"ok"`).
