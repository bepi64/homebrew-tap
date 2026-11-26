---
paths:
  - "**/*.rs"
  - "**/Cargo.toml"
  - "**/Cargo.lock"
---

# Rust

**Runtime:** Latest stable toolchain via `rustup` or Homebrew (`rustc`/`cargo`).

| Purpose | Tool |
| --- | --- |
| Build & deps | `cargo` |
| Lint | `cargo clippy --all-targets --all-features -- -D warnings` |
| Format | `cargo fmt` (`cargo fmt -- --check` in CI and hooks) |
| Test | `cargo test` |
| Supply chain | `cargo deny check` (advisories, licenses, bans) |
| Safety check | `cargo careful test` (stdlib debug assertions and UB checks) |

## Style

- Prefer `for` loops with mutable accumulators over long iterator chains.
- Shadow variables through transformations instead of adding `raw_x` / `parsed_x` prefixes.
- Prefer patterns that fail to compile on type changes: avoid wildcard `_` arms, use exhaustive `match` over `matches!`, and list struct fields explicitly instead of `..`.
- Use `let...else` for early returns to keep the happy path unindented.

## Type Design

- Use newtypes over bare primitives (`PackageName(String)`, not `String`).
- Use enums for state machines instead of boolean flags.
- Use `thiserror` for library error types and `anyhow` for application binaries.
- Use `tracing` (`error!`, `warn!`, `info!`, `debug!`) for logging, never `println!` or `eprintln!` (especially critical on stdio MCP servers where stdout carries JSON-RPC frames).

## Safety

- Keep `unsafe` blocks as small as possible and avoid them whenever safe Rust suffices.
- Precede every `unsafe` block and `extern` FFI call with a `// SAFETY:` comment stating the exact invariants upheld by the caller.

## Performance

Write efficient code by default: choose the right algorithm and data structures, and avoid unnecessary allocations. Profile before micro-optimizing, and measure again afterward.

## Cargo.toml Lints

Enforce panic prevention, hygiene, and safety in `Cargo.toml` rather than in prose:

```toml
[lints.clippy]
pedantic = { level = "warn", priority = -1 }
# Panic prevention
unwrap_used = "deny"
expect_used = "warn"
panic = "deny"
panic_in_result_fn = "deny"
unimplemented = "deny"
# No silent suppressions
allow_attributes = "deny"
# Code hygiene
dbg_macro = "deny"
todo = "deny"
print_stdout = "deny"
print_stderr = "deny"
# Safety
await_holding_lock = "deny"
large_futures = "deny"
exit = "deny"
mem_forget = "deny"
# Pedantic relaxations (too noisy)
module_name_repetitions = "allow"
similar_names = "allow"
```

Because `allow_attributes = "deny"` blocks `#[allow(...)]`, any genuinely unavoidable lint suppression must use `#[expect(clippy::..., reason = "...")]` with a concrete justification.

## Tests

Place unit tests in `#[cfg(test)]` modules beside the code they test, and integration or security tests under `tests/`. Use `cargo-mutants` to verify that tests catch failures, and `proptest` for parsers, URL builders, serialization, and algorithms.
