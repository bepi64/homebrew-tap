# ROADMAP.md

Development milestones and tasks for `fast-docs` (`homebrew-tap`).

## Milestone 1: Project Conventions & Local Tooling
- [x] Standardize AI instructions (`AGENTS.md`, `CLAUDE.md`, `.agents/rules/`) in technical English for Rust, Ruby (Homebrew tap), Bash, GitHub Actions, and security.
- [x] Initialize core project documents (`README.md`, `MEMORY.md`, `ROADMAP.md`).
- [x] Configure `prek.toml`, install Git hook shims (`prek install`), and update `.gitignore`.

## Milestone 2: Test Suite, Pure-Rust Cache & Code Hardening
- [x] Replace the broken C FFI hook (`cache_prefetch_init`) and unused `cc` dependency with a pure-Rust `DiskCache` (`src/cache.rs`) featuring SHA-256 keys, atomic writes, two-tier TTL (`24h` mutable / `7d` pinned), and expired-entry pruning.
- [x] Add unit and property-based tests (`proptest`) for `detect_lang`, `resolve_go_module`, `build_url`, `urlencoding`, `truncate_smart`, `strip_html_tags`, `html_to_context7`, `markdown_to_context7`, and `DiskCache`.
- [x] Add security and mock-HTTP integration tests covering SSRF/path-traversal rejection, outbound HTTPS allowlisting, streaming response size limits (`5 MiB`), timeouts (`10s` / `5s`), non-2xx statuses, and invalid UTF-8 payloads.
- [x] Enable the `[lints.clippy]` configuration from `.agents/rules/rust.md` in `Cargo.toml` and resolve all Clippy warnings across all targets.
- [x] Add `deny.toml` and verify license, ban, and source policies with `cargo deny check`.
- [x] Document empirical and standards-based justifications for all hardening and caching bounds in `MEMORY.md` (`D-06`).

## Milestone 3: CI/CD & Homebrew Tap Distribution
- [x] Configure GitHub Actions CI (`.github/workflows/ci.yml`) and release automation (`.github/workflows/release.yml`) with SHA-pinned actions, verified locally with `actionlint` and `zizmor`.
- [x] Add `.github/dependabot.yml` with 7-day cooldowns and grouped updates for `cargo` and `github-actions`.
- [x] Author `Formula/fast-docs.rb` with a real MCP `initialize` JSON-RPC `test do` block and bump crate version to `1.0.0`.
- [ ] Push to GitHub, tag `v1.0.0`, update the `sha256` checksum in `Formula/fast-docs.rb`, and validate with `brew audit --strict --new`, `brew install --build-from-source`, and `brew test`.
