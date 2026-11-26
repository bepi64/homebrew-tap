---
paths:
  - "src/**/*.rs"
  - "tests/**/*.rs"
  - "Cargo.toml"
  - "Cargo.lock"
  - "Formula/**/*.rb"
  - "Casks/**/*.rb"
---

# Security Rules (`fast-docs` & Homebrew Tap)

`fast-docs` is an MCP server that fetches and parses untrusted external HTTP, HTML, Markdown, and JSON responses (`docs.rs`, `pypi.org`, `pkg.go.dev`, `api.github.com`, `registry.npmjs.org`, `unpkg.com`, and DNS-pinned public `llms.txt` documentation hosts) and persists cached entries on local disk. The following patterns are forbidden in production code without exception.

| Forbidden | Use instead | Risk prevented |
| --- | --- | --- |
| `danger_accept_invalid_certs(true)` or `danger_accept_invalid_hostnames(true)` | Default `rustls-tls` certificate and hostname verification | Traffic interception (MITM) |
| Building outbound URLs from unvalidated user input (path segments containing `..`, `@`, `?`, `#`, or scheme/host overrides) | Strict allowlisted HTTPS registry hosts (`docs.rs`, `pypi.org`, `pkg.go.dev`, `api.github.com`, `registry.npmjs.org`, `unpkg.com`), or DNS-pinned public-IP validation (`src/llms_txt.rs`) for `llms.txt` | SSRF, DNS rebinding, and URL confusion |
| Joining untrusted input directly onto filesystem paths (`dir.join(&req.library)`) | Fixed-format cryptographic hash filenames (e.g., SHA-256 hex) inside a dedicated cache directory | Path traversal and arbitrary file overwrite |
| Shell command execution (`Command::new("sh").arg("-c")`) with external input | Avoid shell invocation; pass explicit argument arrays if subprocesses are ever required | Command injection |
| Unbounded HTTP downloads or missing network timeouts | Explicit request timeouts and strict response body size caps | Memory exhaustion and DoS |
| Unjustified `unsafe` blocks or unvalidated `extern` FFI calls | Safe Rust; when FFI or `unsafe` is strictly required, isolate it and document every invariant in a `// SAFETY:` comment | Memory corruption and undefined behavior |
| Panicking on untrusted network or MCP input (`.unwrap()`, `.expect()`, unchecked indexing) | Propagate structured `Result` errors and handle malformed input gracefully | Crash / denial of service |
| Homebrew formulae with `http://` URLs, floating branch refs, or missing `sha256` | Immutable HTTPS release URLs with verified `sha256` checksums | Supply-chain tampering |

Check for forbidden patterns and dependency advisories before committing:

```bash
rg -n 'danger_accept_invalid_certs|danger_accept_invalid_hostnames|\bunsafe\b|\.unwrap\(|\.expect\(|Command::new' src
ast-grep --pattern 'unsafe { $$$ }' --lang rust src
cargo deny check
```

Legitimate negative test payloads (for example, testing that a path-traversal library name or malformed HTML document is rejected) belong in `tests/` or `#[cfg(test)]` modules, never in production code paths.
