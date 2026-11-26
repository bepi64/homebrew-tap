# fast-docs (`homebrew-tap`)

A fast Model Context Protocol (MCP) documentation server written in Rust for AI coding assistants, paired with its Homebrew tap repository.

## Client-Server Architecture

`fast-docs` follows the **MCP client-server model**:

```text
+------------------------------+     stdio JSON-RPC     +-----------------------------------+
|  AI Coding Assistant (Client)|  <==================>  |  fast-docs (Rust MCP Server)      |
|  e.g. Claude Code / Desktop  |  fetch_docs /          |  - Input validation & SSRF guard  |
|  Requests library docs &     |  search_docs /         |  - Pure-Rust TTL DiskCache + ETag |
|  package searches on demand  |  fetch_llms_txt calls  |  - Bounded HTTPS fetcher (rustls) |
+------------------------------+                        +-----------------+-----------------+
                                                                          | HTTPS (allowlisted / DNS-pinned)
                                                                          v
                    docs.rs · pypi.org · pkg.go.dev · unpkg.com · registry.npmjs.org · api.github.com · */llms.txt
```

- **Client (Claude):** Spawns `fast-docs` over `stdio` and invokes its MCP tools (`fetch_docs`, `search_docs`) whenever up-to-date library documentation or package discovery is needed.
- **Server (`fast-docs`):** Validates inputs, checks the local pure-Rust disk cache (`~/.cache/fast-docs/indexes/`), executes bounded HTTPS requests against allowlisted registries, parses HTML/Markdown into Context7-style blocks, and caches the result with explicit TTL expiration.

### Example Claude Code MCP Configuration

```json
{
  "mcpServers": {
    "fast-docs": {
      "command": "fast-docs"
    }
  }
}
```

## Features

- **`fetch_docs` MCP tool:** Fetches documentation and code examples for Rust (`docs.rs`), Python (`pypi.org`), Go (`pkg.go.dev`), and npm/JS/TS (`unpkg.com` with `registry.npmjs.org` fallback, including scoped `@scope/pkg` packages), supporting optional submodule/item navigation (`module`), keyword-based section ranking/filtering (`topic`), configurable output budgets (`max_chars`, default 6,500, clamped to `500..=20,000`), byte-offset pagination (`offset`), and automatic project version resolution (`project_dir`) from `Cargo.lock`, `Cargo.toml`, `package-lock.json`, `package.json`, `uv.lock`, `pyproject.toml`, or `go.mod`.
- **`search_docs` MCP tool:** Searches package registries across Rust (`docs.rs`), Python (`pypi.org` and GitHub), Go (`pkg.go.dev`), and npm (`registry.npmjs.org`) **in parallel** (`tokio::join!`) with a 15-minute local cache.
- **`fetch_llms_txt` MCP tool:** Fetches and filters official `/llms.txt` or `/llms-full.txt` documentation files (with automatic fallback from `llms-full.txt` to `llms.txt` on 404 and byte-offset `offset` pagination) from public HTTPS documentation sites, protected by pre-request DNS resolution, public-IP validation, IP-pinned TLS connections (blocking DNS rebinding), and same-host redirect restrictions.
- **`fetch_local_docs` MCP tool:** Searches and reads local documentation inside private repositories or project folders across Markdown/text (`*.md`, `*.mdx`, `*.txt`, `*.rst`), Jupyter notebooks (`*.ipynb`), Office & OpenDocument (`*.docx`, `*.pptx`, `*.xlsx`, `*.odt`, `*.odp`, `*.ods`), `*.epub`, and `*.pdf`, with topic ranking, pagination (`offset` / `max_chars`), anti-zip-bomb (`2 MiB` decompressed cap / `200` entries), anti-XXE XML extraction, panic-isolated PDF parsing, strict secret/directory exclusions, and canonicalized anti-symlink-escape checks.
- **`manage_cache` MCP tool:** Exposes local cache observability and maintenance (`stats`, `prune`, `clear`) directly to MCP clients.
- **Dual MCP transports (`stdio` & Streamable HTTP):** Defaults to `stdio` for zero-overhead local spawning; pass `--http [127.0.0.1:3000]` to run the MCP Streamable HTTP server (`/mcp`) bound strictly to loopback with `rmcp`'s built-in DNS rebinding `Host` validation.
- **Pure-Rust TTL disk cache (`src/cache.rs`):** Stores parsed documentation sections and HTTP `ETag` validators atomically under SHA-256-derived filenames with 24-hour freshness for mutable (`latest`) entries, 7-day freshness for version-pinned entries, RFC 9111 `If-None-Match` (`304 Not Modified`) revalidation on expiry, and startup/daemon pruning (zero `unsafe`). Subsequent `topic` or `max_chars` queries against the same page are re-ranked locally in sub-millisecond time without a network round-trip.
- **Hardened outbound HTTP (`src/tools.rs`, `src/llms_txt.rs`):** Strict HTTPS allowlist for package registries (`docs.rs`, `pypi.org`, `pkg.go.dev`, `api.github.com`, `registry.npmjs.org`, `unpkg.com`), DNS-pinned public-IP guard for `llms.txt`, redirect hop validation (max 5 hops), transparent `gzip` decompression, 5 s connect timeout, 10 s request timeout, and a 5 MiB incremental streaming response body cap on the decompressed stream.

## Repository Layout

| Path | Responsibility |
| --- | --- |
| `src/lib.rs` | Library root exposing `cache`, `extractors`, `http_server`, `llms_txt`, `local_docs`, `manifest`, and `tools` modules |
| `src/main.rs` | Binary entrypoint, stderr `tracing` setup, startup/daemon cache pruning, and `stdio` / `--http` MCP server |
| `src/cache.rs` | Pure-Rust `DiskCache` (SHA-256 keys, atomic writes, two-tier TTL, `ETag` revalidation, stats, prune, clear) |
| `src/extractors.rs` | Bounded, XXE-safe & zip-bomb-safe extractors for `.ipynb`, `.docx`, `.pptx`, `.xlsx`, `.odt`, `.odp`, `.ods`, `.epub`, and `.pdf` |
| `src/http_server.rs` | Loopback-only MCP Streamable HTTP server (`/mcp`) built on `rmcp` + `hyper` with DNS rebinding protection |
| `src/llms_txt.rs` | `llms.txt` / `llms-full.txt` URL normalizer, public-IP SSRF guard, and DNS-pinned HTTP client builder |
| `src/local_docs.rs` | Bounded, symlink-safe local Markdown/text documentation indexer for private repositories |
| `src/manifest.rs` | Zero-dependency manifest and lockfile version resolver (`Cargo.lock`, `Cargo.toml`, `package-lock.json`, `package.json`, `uv.lock`, `pyproject.toml`, `go.mod`) |
| `src/tools.rs` | `FastDocsServer` MCP router (`fetch_docs`, `search_docs`, `fetch_llms_txt`, `fetch_local_docs`, `manage_cache`), URL/SSRF validation, bounded HTTP fetcher, pagination, and HTML/Markdown parsers |
| `tests/` | Unit, property-based (`proptest`), cache, security, and mock-HTTP integration tests |
| `Formula/fast-docs.rb` | Homebrew tap formula |
| `.github/workflows/`, `.github/dependabot.yml` | Hardened GitHub Actions CI, release automation, and Dependabot configuration |
| `deny.toml`, `prek.toml` | Supply-chain policy (`cargo-deny`) and local pre-commit hooks (`prek`) |
| `AGENTS.md`, `CLAUDE.md`, `.agents/rules/` | AI-assisted development conventions and path-scoped rules |

## Tech Stack

- **Language:** Rust (2021 edition, zero `unsafe`)
- **MCP runtime:** `rmcp` (`server`, `macros`, `transport-io`, `schemars`) over `tokio`
- **HTTP & parsing:** `reqwest` (`rustls-tls`), `scraper`, `serde`, `serde_json`
- **Hashing & logging:** `sha2`, `hex`, `tracing`, `tracing-subscriber`
- **Testing & quality:** `proptest`, `cargo-mutants`, `cargo-deny`, `actionlint`, `zizmor`, `prek`

## Development

```bash
# Install Git hooks
prek install

# Format, lint, test, and build
cargo fmt -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
cargo build

# Supply-chain, mutation, and workflow audits
cargo deny check
cargo mutants
actionlint
zizmor .github/workflows/

# Run all pre-commit hooks manually
prek run
```

See `AGENTS.md`, `MEMORY.md`, and `ROADMAP.md` for conventions, architecture decisions (including empirical justifications for security/caching limits), and release readiness.
