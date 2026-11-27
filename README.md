# fast-docs

**A local-first, sub-millisecond MCP documentation server for AI coding assistants — written in pure safe Rust.**

[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![Rust: 100% Safe](https://img.shields.io/badge/unsafe-0%20blocks-brightgreen.svg)](#security-by-design--verified-by-tests)
[![Tests: 81 Passing](https://img.shields.io/badge/tests-81%20passing-brightgreen.svg)](#security-by-design--verified-by-tests)
[![Zero Telemetry](https://img.shields.io/badge/telemetry-none-success.svg)](#why-fast-docs)

Stop feeding your AI assistant outdated APIs, cloud-proxied snippets, or spawning a dozen heavy Node.js processes across agents. `fast-docs` compiles into a **single 9.6 MiB native Rust background daemon** with **zero runtime dependencies**: run **one shared service** on `127.0.0.1:3000/mcp` for every Claude Code session and IDE on your machine (sharing one HTTP/2 connection pool and one disk cache), resolve the **exact dependency versions locked in your repository**, and get cached answers in **under 0.5 ms**.

---

## Why `fast-docs`?

| ❌ Without `fast-docs` | ✅ With `fast-docs` |
| --- | --- |
| Hallucinated methods or docs from the wrong major version | Reads **`Cargo.lock`, `uv.lock`, `pnpm-lock.yaml`, `package-lock.json`, `go.mod`**, `Cargo.toml`, `pyproject.toml`, `requirements.txt`, and `package.json` to fetch docs for your **exact locked version** |
| Slow cloud round-trips, API keys, and rate-limited third-party SaaS | **100% local**, zero API keys, zero accounts, zero telemetry — talks directly to official registries and your local repository |
| Heavy Node.js / Python / Docker MCP runtimes eating RAM | **5.65 ms** cold start, **0.29 ms** cache hits, **5.2 MiB** peak memory footprint, near-zero CPU |
| Unbounded HTTP fetches and unguarded local file reads | **Defense-in-depth security**: HTTPS allowlists, DNS-pinned public-IP SSRF guards, anti-DNS-rebinding HTTP transport, 5 MiB streaming caps, anti-zip-bomb & anti-XXE extractors |
| Private company docs, PDFs, spreadsheets, and notebooks left out | Searches local Markdown, RST, **Jupyter `.ipynb`**, **Word `.docx`**, **PowerPoint `.pptx`**, **Excel `.xlsx`**, **OpenDocument**, **EPUB**, and **PDF** on disk without uploading a single byte |

---

## Benchmarks

Measured on Apple Silicon (`macOS ARM64`, `target/release/fast-docs` v1.0.0 with LTO, 10 runs each, isolated temp `HOME`):

| Metric | `fast-docs` (Rust) | Comparison / Context |
| --- | ---: | --- |
| **Cold start + full MCP handshake** (`initialize` + `tools/list`) | **5.65 ms** *(min 5.05 ms)* | **~12× faster** than bare Python (`69.8 ms`) · **~29× faster** than bare Node.js (`163.8 ms`) before loading any SDK |
| **Warm cache hit** (`fetch_docs`, multi-topic local re-ranking) | **0.29 ms** *(min 0.17 ms)* | Zero network round-trips; re-scores cached sections for any new `topic` or `offset` locally |
| **Streamable HTTP RPC** (`--http`, `POST /mcp` `initialize`) | **0.44 ms** *(min 0.40 ms)* | Sub-millisecond persistent loopback server |
| **Parallel 4-registry search** (`search_docs`, Rust + Python + Go + npm) | **~800 ms** cold / **0.31 ms** warm | Runs all upstream registries concurrently via `tokio::join!` with 15-minute cache |
| **Local private repo search** (`fetch_local_docs`, topic ranked) | **1.66 ms** median | On-the-fly scan — zero pre-indexing step, zero vector DB |
| **Memory footprint** (`peak memory footprint` / max RSS) | **5.2 MiB** / **17.7 MiB** | Includes TLS (`rustls`), `gzip` decompression, and full HTML parsing |
| **CPU usage** (`/usr/bin/time -l` user + sys on live fetch) | **0.00 s user · 0.00 s sys** | Event-driven async I/O (`tokio`), idles cleanly between MCP tool calls |
| **Binary size** | **9.6 MiB** | Single self-contained executable (no Node, Python, OpenSSL, or C FFI) |

> **Transparency note:** Cold network fetches are bounded by upstream registry latency (~40–350 ms across `docs.rs`, `pypi.org`, `pkg.go.dev`, `unpkg.com`, and `llms.txt` sites). `fast-docs` wins on startup, memory, CPU, parallel search, `gzip` transfer, `ETag` (`304 Not Modified`) revalidation, and sub-millisecond repeat reads.

---

## Quickstart (30 Seconds)

### 1. Install & Start the Background Service

**Homebrew (macOS / Linux — recommended):**

```bash
brew tap bepi64/tap
brew install fast-docs
brew services start fast-docs
```

Verify the daemon is running:

```bash
curl -s http://127.0.0.1:3000/health
# {"status":"ok","service":"fast-docs","version":"1.0.0"}
```

**From source (Rust stable):**

```bash
git clone https://github.com/bepi64/homebrew-tap.git
cd homebrew-tap
cargo build --release
# Starts the shared background HTTP daemon on http://127.0.0.1:3000/mcp by default
./target/release/fast-docs
```

*(Override the loopback bind address via `--http 127.0.0.1:8080` or `FAST_DOCS_BIND=127.0.0.1:8080`.)*

### 2. Connect Your AI Assistants to the Shared Daemon

One running `fast-docs` process serves **all** your concurrent Claude Code terminals, Cursor windows, and IDE agents with a shared HTTP/2 connection pool and hourly cache pruning:

#### Claude Code (recommended — connects to the shared daemon)

```bash
claude mcp add -s user --transport http fast-docs http://127.0.0.1:3000/mcp
```

#### Cursor / Windsurf / VS Code (`mcp.json`)

```json
{
  "mcpServers": {
    "fast-docs": {
      "url": "http://127.0.0.1:3000/mcp"
    }
  }
}
```

#### Alternative: Per-client `stdio` mode (`--stdio`)

If you prefer clients to spawn an isolated `stdio` process instead of using the background daemon:

```bash
claude mcp add -s user fast-docs -- fast-docs --stdio
```

*(Behind a corporate HTTPS proxy without custom `rustls` roots, start the daemon with `HTTP_PROXY= HTTPS_PROXY= http_proxy= https_proxy= fast-docs`.)*

#### Optional: Make Your Agent Use `fast-docs` Automatically

Add one line to your project's `CLAUDE.md` or `AGENTS.md`:

```markdown
When looking up library APIs, call `fast-docs` (`fetch_docs` with `project_dir` set to this repository) instead of guessing from memory.
```

---

## Five Focused MCP Tools

```text
+------------------------------+   stdio / Streamable HTTP   +----------------------------------------+
|  AI Coding Assistant (Client)|  <=======================>  |  fast-docs (Safe Rust MCP Server)      |
|  Claude Code · Cursor · IDE  |   JSON-RPC 2.0 tool calls   |  - Lockfile & manifest version lookup  |
|  Calls tools on demand       |                             |  - Pure-Rust TTL DiskCache + ETag 304  |
+------------------------------+                             |  - Word-boundary topic ranker + paging |
                                                             |  - Local Markdown/Office/EPUB/PDF scan |
                                                             +-------------------+--------------------+
                                                                                 | HTTPS (allowlisted / DNS-pinned)
                                                                                 v
                           docs.rs · pypi.org · pkg.go.dev · unpkg.com · registry.npmjs.org · api.github.com · */llms.txt
```

| Tool | What it does | Key parameters |
| --- | --- | --- |
| **`fetch_docs`** | Fetches version-accurate documentation across **Rust** (`docs.rs`), **Python** (`pypi.org` + RST parser + RSS fallback for >5 MiB packages), **Go** (`pkg.go.dev` README + API docs, vanity imports & stdlib), and **npm/JS/TS** (`unpkg.com` + `registry.npmjs.org` fallback, scoped `@scope/pkg`). | `library`, `version`, `lang`, `module` (supports `::` item paths like `builder::Command` and `sync::mpsc`), `topic`, `project_dir`, `max_chars` (`500..=20000`), `offset` |
| **`search_docs`** | Searches Rust, Python, Go, and npm registries **in parallel** (`tokio::join!`) with 15-minute caching and exact PyPI package matching even on multi-word queries (`"pytest fixtures"`). | `query`, `lang` (`rust`, `python`, `go`, `npm`, `all`) |
| **`fetch_llms_txt`** | Fetches and filters official `/llms.txt` or `/llms-full.txt` (auto-fallback on 404, safe per-hop DNS-pinned redirects like `docs.pydantic.dev` → `pydantic.dev`). | `target`, `full`, `topic`, `max_chars`, `offset` |
| **`fetch_local_docs`** | Searches private repository documentation locally across `.md`, `.mdx`, `.txt`, `.rst`, **`.ipynb`**, **`.docx`**, **`.pptx`**, **`.xlsx`**, **`.odt`**, **`.odp`**, **`.ods`**, **`.epub`**, and **`.pdf`** — prioritizing `README.md` and `docs/` over changelogs. | `project_dir`, `topic`, `max_chars`, `offset` |
| **`manage_cache`** | Inspects or maintains the local disk cache (`~/.cache/fast-docs/indexes/`). | `action` (`stats`, `prune`, `clear`) |

### Example Prompts to Try Right Away

- *"Use `fast-docs` to look up `tokio` submodule `sync` for `mpsc channel bounded` using the exact version in this repository."*
- *"Use `fast-docs` to check `pydantic` validators from `docs.pydantic.dev` (`llms-full.txt`)."*
- *"Use `fast-docs` to search our local `docs/` and PDFs for authentication and rate limits."*
- *"Use `fast-docs` to inspect `pflag` `Parse` in this Go repo without guessing the version."*

---

## Security by Design — Verified by Tests

Security is enforced in code and CI, not just in prose. `fast-docs` contains **zero `unsafe` blocks**, **zero `.unwrap()` / `panic!()` calls in production paths**, and enforces strict `clippy::pedantic` + panic-prevention lints in `Cargo.toml`:

- **Outbound SSRF & DNS-rebinding defense:**
  - `fetch_docs` and `search_docs` allow HTTPS strictly to 6 pinned registry hosts (`docs.rs`, `pypi.org`, `pkg.go.dev`, `api.github.com`, `registry.npmjs.org`, `unpkg.com`), rejecting non-HTTPS schemes, explicit ports, embedded credentials, path traversal (`..`), and cross-allowlist redirects.
  - `fetch_llms_txt` blocks IP literals and internal suffixes (`.localhost`, `.local`, `.internal`, `.consul`, `.home.arpa`, `.lan`), resolves DNS upfront on **every redirect hop**, rejects all non-public IPv4/IPv6 ranges (loopback, RFC 1918, CGNAT, cloud IMDS `169.254.169.254`, ULA, link-local, IPv4-mapped `::ffff:`, NAT64 `64:ff9b::`), and pins each connection to the validated IP via `resolve_to_addrs`.
- **Streamable HTTP hardening (`--http`):**
  - Refuses non-loopback bind addresses (`0.0.0.0`, LAN IPs) by default and enforces `rmcp`'s built-in DNS-rebinding `Host` header validation (`HTTP 403` on hostile `Host` headers).
- **Resource & DoS caps (OWASP API4:2023 + RFC 9111):**
  - 5 s connect timeout, 10 s total timeout, and a **5 MiB streaming byte cap** enforced on the **decompressed** stream (defeating gzip bombs).
  - Rich local extractors enforce a 5 MiB raw file cap, 200-entry ZIP limit, **`Read::take` 2 MiB decompressed cap** (anti-zip-bomb), entity-safe XML text extraction without DTD loading (anti-XXE), and `std::panic::catch_unwind` isolation around PDF parsing.
- **Local secrets & symlink isolation:**
  - `fetch_local_docs` canonicalizes every file and drops any symlink escaping `project_dir`, while excluding `.env*`, `.git/`, `.github/`, `.ssh/`, `.gnupg/`, `target/`, `node_modules/`, `*.pem`, `*.key`, `id_rsa*`, and `id_ed25519*`.
- **Automated verification:**
  - **81 automated tests** (unit, `proptest` property-based tests, local mock-HTTP integration tests, concurrent multi-agent HTTP daemon & `/health` tests, SSRF, zip-bomb, XXE, corrupted PDF, symlink escape, and DNS-rebinding tests).
  - **32/32 viable mutants caught** by `cargo-mutants` on `src/cache.rs`.
  - **0 warnings** on `cargo clippy --all-targets --all-features -- -D warnings`, **0 RustSec advisories** on `cargo deny check`, **0 findings** on `actionlint` & `zizmor`, and **0 offenses** on `brew style`.

See [`MEMORY.md`](MEMORY.md) (`D-01`–`D-20`) for the empirical measurements and standards (OWASP API4:2023, OWASP SSRF Cheat Sheet, RFC 9111, HTTP Archive Web Almanac, Nielsen's response-time limits) backing every constant.

---

## Honest Comparison

| Capability | `fast-docs` | Context7 | Official MCP Fetch | Docs MCP Server |
| --- | :---: | :---: | :---: | :---: |
| **Runtime** | Single Rust binary (9.6 MiB) | Node.js / Cloud SaaS | Python (+ Node opt.) | Node.js / Docker |
| **Cold start** | **5.65 ms** | ≥ 164 ms (Node) / remote | ≥ 70 ms (Python) | ≥ 164 ms (Node) |
| **Warm cache hit** | **0.29 ms** (local disk + ETag) | Remote HTTPS call | No cache | Local SQLite |
| **Auto lockfile version lookup** | **Yes** (`Cargo.lock`, `uv.lock`, `pnpm-lock.yaml`, `package-lock.json`, `go.mod`, etc.) | No | No | No |
| **Public registries + `llms.txt`** | Rust, Python, Go, npm + `llms.txt` | Broad community GitHub index | Arbitrary URLs | Web, GitHub, npm, PyPI |
| **Local private docs (MD/Office/EPUB/PDF)** | **Yes** (instant, no index step) | No | No | Yes (pre-indexed) |
| **Search ranking** | Word-boundary lexical + anti-changelog | Cloud semantic | None | Hybrid FTS + vector |
| **Zero API keys & zero telemetry** | **Yes** | No (cloud API) | Yes | Opt-out telemetry |
| **Transports** | `stdio` + Streamable HTTP | `stdio` + Streamable HTTP | `stdio` + Streamable HTTP | `stdio` + Streamable HTTP |

**When should you pick another tool instead?**
- Pick **Context7** if you primarily need semantic search across a broad catalog of web frameworks that do not publish `llms.txt` or package-registry docs and you are comfortable sending queries to a hosted cloud service.
- Pick **Docs MCP Server** if you need persistent vector-embedding search across multi-thousand-page crawled websites and are willing to run Node.js/Docker plus an embedding model.
- Pick **`fast-docs`** if you want **instant startup, lockfile-exact versions, zero cloud/API keys, strict security boundaries, and local private docs** in a single safe Rust binary.

---

## Repository Layout & Development

| Path | Responsibility |
| --- | --- |
| `src/main.rs` | Entrypoint, stderr `tracing` setup, startup/daemon cache pruning, `stdio` & `--http` modes |
| `src/cache.rs` | Pure-Rust `DiskCache` (SHA-256 keys, atomic writes, two-tier TTL `24h`/`168h` + `15m` search, `ETag` `304`, `stats`/`prune`/`clear`) |
| `src/manifest.rs` | Zero-dependency lockfile & manifest version resolver + project ecosystem detector |
| `src/tools.rs` | `FastDocsServer` MCP router (5 tools), SSRF allowlist, bounded HTTP fetcher, HTML/Markdown/RST/PyPI/rustdoc parsers, word-boundary topic ranker, pagination |
| `src/llms_txt.rs` | `llms.txt` / `llms-full.txt` URL validator, public-IP SSRF guard, and per-hop DNS-pinned redirect fetcher |
| `src/local_docs.rs` | Priority-ordered, symlink-safe, secret-excluding local documentation scanner |
| `src/extractors.rs` | Zip-bomb-safe, XXE-safe, panic-isolated extractors for `.ipynb`, Office, OpenDocument, `.epub`, and `.pdf` |
| `src/http_server.rs` | Loopback-only MCP Streamable HTTP server (`/mcp`) over `rmcp` + `hyper` |
| `tests/` | 81 tests across 8 integration, property-based (`proptest`), and security suites |
| `Formula/fast-docs.rb` | Homebrew tap formula with a two-step MCP `initialize` test |
| `.github/workflows/`, `deny.toml`, `prek.toml` | SHA-pinned CI & clean-build release workflows, supply-chain audit, and local Git hooks |

```bash
# Install local Git hooks
prek install

# Format, lint, test, and build
cargo fmt -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
cargo build --release

# Supply-chain, mutation, workflow, and Homebrew audits
cargo deny check
cargo mutants -f src/cache.rs
actionlint
zizmor .github/workflows/
brew style Formula/fast-docs.rb
prek run
```

See [`AGENTS.md`](AGENTS.md), [`MEMORY.md`](MEMORY.md), and [`ROADMAP.md`](ROADMAP.md) for engineering conventions, architecture decisions, and release milestones. Licensed under [Apache-2.0](LICENSE).
