use std::fmt::Write as _;

use fast_docs::tools::{
    clamp_max_chars, detect_lang, html_to_context7, html_to_context7_with_options,
    markdown_to_context7, markdown_to_context7_with_options, parse_pypi_json_to_context7,
    resolve_go_module, slice_with_pagination, strip_html_tags, truncate_smart, urlencoding,
    Ecosystem, MAX_ALLOWED_OUTPUT_CHARS, MAX_OUTPUT_CHARS, MIN_OUTPUT_CHARS,
};
use proptest::prelude::*;

#[test]
fn detect_lang_identifies_rust_python_go_and_npm() {
    assert_eq!(detect_lang("tokio"), Ecosystem::Rust);
    assert_eq!(detect_lang("serde"), Ecosystem::Rust);
    assert_eq!(detect_lang("Flask"), Ecosystem::Python);
    assert_eq!(detect_lang("pydantic"), Ecosystem::Python);
    assert_eq!(detect_lang("gin"), Ecosystem::Go);
    assert_eq!(detect_lang("github.com/gin-gonic/gin"), Ecosystem::Go);
    assert_eq!(detect_lang("golang.org/x/sync"), Ecosystem::Go);
    assert_eq!(detect_lang("react"), Ecosystem::Npm);
    assert_eq!(detect_lang("zod"), Ecosystem::Npm);
    assert_eq!(detect_lang("hono"), Ecosystem::Npm);
    assert_eq!(detect_lang("@types/node"), Ecosystem::Npm);
    assert_eq!(detect_lang("@modelcontextprotocol/sdk"), Ecosystem::Npm);
}

#[test]
fn ecosystem_parse_accepts_npm_js_and_ts_aliases() {
    for alias in ["npm", "js", "javascript", "ts", "typescript", "node"] {
        assert_eq!(Ecosystem::parse(alias), Some(Ecosystem::Npm));
    }
    assert_eq!(Ecosystem::Npm.as_str(), "npm");
}

#[test]
fn resolve_go_module_maps_known_aliases_and_preserves_paths() {
    assert_eq!(resolve_go_module("gin"), "github.com/gin-gonic/gin");
    assert_eq!(resolve_go_module("ECHO"), "github.com/labstack/echo/v4");
    assert_eq!(resolve_go_module("gorilla"), "github.com/gorilla/mux");
    assert_eq!(
        resolve_go_module("github.com/custom/pkg"),
        "github.com/custom/pkg"
    );
}

#[test]
fn html_to_context7_extracts_sections_and_prioritizes_code() {
    let html = r#"
    <html><body>
      <div class="docblock">
        <h1>Overview</h1>
        <p>This is a detailed library overview paragraph with enough length.</p>
        <h2>Notes</h2>
        <p>This section has descriptive prose without any code blocks at all.</p>
        <h2>Quickstart</h2>
        <p>Run the following code example to get started quickly.</p>
        <pre>fn main() { println!("hi"); }</pre>
      </div>
    </body></html>
    "#;

    let rendered = html_to_context7(html, "https://docs.rs/demo/latest", Ecosystem::Rust, "demo");
    assert!(rendered.contains("TITLE: Overview"));
    assert!(rendered.contains("TITLE: Quickstart"));
    assert!(rendered.contains("```rust\nfn main()"));
    assert!(rendered.len() <= MAX_OUTPUT_CHARS);

    let quickstart_pos = rendered.find("TITLE: Quickstart");
    let notes_pos = rendered.find("TITLE: Notes");
    assert!(quickstart_pos < notes_pos);
}

#[test]
fn html_to_context7_handles_empty_and_malformed_html() {
    let empty = html_to_context7("", "https://docs.rs/x/latest", Ecosystem::Rust, "x");
    assert_eq!(empty, "");

    let malformed = "<div><p>unclosed <b>tags <<< &amp; raw text";
    let out = html_to_context7(malformed, "https://docs.rs/x/latest", Ecosystem::Rust, "x");
    assert!(out.contains("unclosed"));
    assert!(out.len() <= MAX_OUTPUT_CHARS);
}

#[test]
fn markdown_to_context7_extracts_headings_code_and_strips_badges() {
    let md = "\
[![Build](https://img.shields.io/badge/build-passing-green)]
<img src=\"logo.png\" />
# Installation
Install via pip with <b>Python 3</b> support.
```python
import flask
```
";
    let rendered = markdown_to_context7(
        md,
        "A lightweight web framework.",
        "https://pypi.org/project/flask/",
        Ecosystem::Python,
        "flask",
    );
    assert!(rendered.contains("A lightweight web framework."));
    assert!(rendered.contains("TITLE: Installation"));
    assert!(rendered.contains("Install via pip with Python 3 support."));
    assert!(rendered.contains("```python\nimport flask\n```"));
    assert!(!rendered.contains("img.shields.io"));
    assert!(!rendered.contains("<img"));
}

#[test]
fn clamp_max_chars_enforces_default_min_and_max_bounds() {
    assert_eq!(clamp_max_chars(None), MAX_OUTPUT_CHARS);
    assert_eq!(clamp_max_chars(Some(0)), MIN_OUTPUT_CHARS);
    assert_eq!(clamp_max_chars(Some(100)), MIN_OUTPUT_CHARS);
    assert_eq!(clamp_max_chars(Some(2500)), 2500);
    assert_eq!(clamp_max_chars(Some(999_999)), MAX_ALLOWED_OUTPUT_CHARS);
}

#[test]
fn topic_filtering_ranks_matching_sections_first_and_filters_unrelated() {
    let md = "\
# Overview
General introduction to the asynchronous runtime library.
```rust
fn intro() {}
```
# File I/O
Async file readers and writers for local disk streams.
```rust
tokio::fs::read(\"a.txt\").await
```
# Synchronization with RwLock
Use RwLock and Mutex primitives for shared mutable state across tasks.
```rust
let lock = tokio::sync::RwLock::new(5);
let r = lock.read().await;
```
";

    let rendered = markdown_to_context7_with_options(
        md,
        "Async runtime for Rust.",
        "https://docs.rs/tokio/latest",
        Ecosystem::Rust,
        "tokio",
        Some("rwlock mutex"),
        None,
    );

    assert!(rendered.contains("TITLE: Synchronization with RwLock"));
    assert!(rendered.contains("tokio::sync::RwLock::new(5)"));
    assert!(!rendered.contains("TITLE: File I/O"));
}

#[test]
fn topic_filtering_returns_table_of_contents_when_no_section_matches() {
    let html = r#"
    <html><body>
      <div class="docblock">
        <h1>Overview</h1>
        <p>Core documentation for the demo crate with basic usage examples.</p>
        <pre>let x = demo::start();</pre>
        <h2>Authentication</h2>
        <p>Token configuration details.</p>
      </div>
    </body></html>
    "#;

    let rendered = html_to_context7_with_options(
        html,
        "https://docs.rs/demo/latest",
        Ecosystem::Rust,
        "demo",
        Some("nonexistent_xyz_topic"),
        Some(1200),
    );

    assert!(rendered.contains("No sections matched topic"));
    assert!(rendered.contains("Available sections:"));
    assert!(rendered.contains("- Overview"));
    assert!(rendered.contains("- Authentication"));
    assert!(
        !rendered.contains("demo::start()"),
        "unmatched topic must not dump unrelated body content: {rendered}"
    );
}

#[test]
fn slice_with_pagination_supports_multi_page_reading_and_bounds() {
    let doc = "alpha beta gamma delta epsilon zeta eta theta iota kappa";
    assert_eq!(slice_with_pagination(doc, 0, 100), doc);

    let page1 = slice_with_pagination(doc, 0, 20);
    assert!(page1.starts_with("alpha beta"));
    assert!(page1.contains("pass offset="));

    let page2 = slice_with_pagination(doc, 16, 20);
    assert!(page2.contains("delta") || page2.contains("gamma"));
    assert!(page2.contains("of 56"));

    let past_end = slice_with_pagination(doc, 999, 20);
    assert!(past_end.contains("exceeds document length"));
}

#[test]
fn topic_filtering_surfaces_deep_matching_lines_without_450_char_truncation() {
    let mut md = String::from("## Optional\n");
    for i in 1..=15 {
        let _ = writeln!(
            md,
            "- [Unrelated Helper {i}](https://hono.dev/docs/helpers/h{i}): helper description number {i} with extra padding text."
        );
    }
    md.push_str(
        "- [Routing API](https://hono.dev/docs/api/routing): Fast RegExpRouter for HTTP routing.\n",
    );
    md.push_str("- [Middleware Guide](https://hono.dev/docs/guides/middleware): Composable async middleware pipeline.\n");

    let rendered = markdown_to_context7_with_options(
        &md,
        "",
        "https://hono.dev/llms.txt",
        Ecosystem::Npm,
        "hono",
        Some("routing middleware"),
        Some(6500),
    );

    assert!(
        rendered.contains("https://hono.dev/docs/api/routing"),
        "deep matching routing line must not be dropped by .take(4): {rendered}"
    );
    assert!(
        rendered.contains("https://hono.dev/docs/guides/middleware"),
        "deep matching middleware line must appear: {rendered}"
    );
    assert!(
        !rendered.contains("... (truncated)"),
        "short matching output within 6500 budget must not be truncated at 450 chars: {rendered}"
    );
}

#[test]
fn topic_ranking_prefers_multi_term_word_boundary_matches_over_single_substring() {
    let md = "\
# CPU-bound tasks and blocking code
Offload CPU-bound computations using spawn_blocking so worker threads stay responsive.
# Bounded mpsc channel
Create a bounded mpsc channel for backpressure between async producer and consumer tasks.
```rust
let (tx, mut rx) = tokio::sync::mpsc::channel(32);
```
";

    let rendered = markdown_to_context7_with_options(
        md,
        "",
        "https://docs.rs/tokio/latest",
        Ecosystem::Rust,
        "tokio",
        Some("mpsc channel bounded"),
        Some(6500),
    );

    let channel_pos = rendered.find("TITLE: Bounded mpsc channel");
    assert!(
        channel_pos.is_some(),
        "expected Bounded mpsc channel section: {rendered}"
    );
    if let Some(cpu_pos) = rendered.find("TITLE: CPU-bound tasks") {
        assert!(channel_pos < Some(cpu_pos));
    }
}

#[test]
fn html_to_context7_extracts_rustdoc_module_tables() {
    let rustdoc_html = r##"
    <html><body><main><section id="main-content">
      <div class="docblock">
        <h1>Tokio</h1>
        <p>An event-driven, non-blocking I/O platform for writing asynchronous applications.</p>
      </div>
      <h2 id="modules" class="section-header">Modules<a href="#modules" class="anchor">§</a></h2>
      <dl class="item-table">
        <dt><a class="mod" href="sync/index.html">sync</a></dt>
        <dd>Synchronization primitives for use in asynchronous contexts.</dd>
        <dt><a class="mod" href="net/index.html">net</a></dt>
        <dd>TCP/UDP/Unix bindings for tokio.</dd>
      </dl>
    </section></main></body></html>
    "##;

    let rendered = html_to_context7_with_options(
        rustdoc_html,
        "https://docs.rs/tokio/1.54.1/tokio/",
        Ecosystem::Rust,
        "tokio",
        Some("sync synchronization"),
        Some(6500),
    );

    assert!(
        rendered.contains("TITLE: Modules"),
        "rustdoc Modules table must be extracted: {rendered}"
    );
    assert!(
        rendered.contains("sync: Synchronization primitives"),
        "module entry and description must appear: {rendered}"
    );
}

#[test]
fn pypi_json_includes_official_documentation_project_urls_even_with_topic() {
    let pypi_json = r##"{
      "info": {
        "name": "requests",
        "version": "2.32.3",
        "summary": "Python HTTP for Humans.",
        "package_url": "https://pypi.org/project/requests/",
        "project_urls": {
          "Documentation": "https://requests.readthedocs.io",
          "Source": "https://github.com/psf/requests"
        },
        "description": "# Requests\nSimple HTTP library for Python.\n# Features\n- Basic & Digest Authentication\n- Connection Timeouts"
      }
    }"##;

    let rendered = parse_pypi_json_to_context7(
        pypi_json,
        "https://pypi.org/pypi/requests/json",
        "requests",
        Some("authentication"),
        None,
    );
    assert!(rendered.is_some());
    if let Some(text) = rendered {
        assert!(
            text.contains("https://requests.readthedocs.io"),
            "Documentation project URL must remain visible even when topic filters sections: {text}"
        );
        assert!(text.contains("2.32.3"));
        assert!(text.contains("Basic & Digest Authentication"));
    }
}

#[test]
fn markdown_preserves_h1_breadcrumb_for_h2_subsections() {
    let md = "\
# CORS Middleware
Configure Cross-Origin Resource Sharing headers on Hono routes.
## Usage
Apply the middleware to specific route paths.
```ts
app.use('/api/*', cors())
```
# Logger Middleware
Log incoming HTTP requests to stdout.
## Usage
Enable request logging globally.
```ts
app.use('*', logger())
```
";

    let rendered = markdown_to_context7_with_options(
        md,
        "",
        "https://hono.dev/llms-full.txt",
        Ecosystem::Npm,
        "hono",
        Some("cors"),
        Some(3000),
    );

    assert!(
        rendered.contains("TITLE: CORS Middleware > Usage"),
        "H2 subsection should inherit H1 parent breadcrumb: {rendered}"
    );
    assert!(rendered.contains("app.use('/api/*', cors())"));
    assert!(!rendered.contains("Logger Middleware"));
}

proptest! {
    #[test]
    fn urlencoding_emits_only_safe_ascii_and_preserves_unreserved(s in ".*") {
        let encoded = urlencoding(&s);
        for b in encoded.bytes() {
            prop_assert!(
                b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~' | b'+' | b'%'),
                "unexpected byte {b:#x} in {encoded:?}"
            );
        }
        // Unreserved ASCII characters roundtrip unchanged
        if s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~')) {
            prop_assert_eq!(encoded, s);
        }
    }

    #[test]
    fn truncate_smart_never_panics_and_preserves_short_strings(
        s in ".{0,300}",
        max in 0_usize..250
    ) {
        let out = truncate_smart(&s, max);
        if s.len() <= max {
            prop_assert_eq!(out, s);
        } else {
            prop_assert!(out.ends_with("\n... (truncated)"));
            let prefix_len = out.len().saturating_sub("\n... (truncated)".len());
            prop_assert!(prefix_len <= max);
        }
    }

    #[test]
    fn strip_html_tags_is_idempotent_on_tag_free_output(s in ".{0,200}") {
        let once = strip_html_tags(&s);
        let twice = strip_html_tags(&once);
        prop_assert_eq!(&once, &twice);
        prop_assert!(!once.contains('<'));
    }

    #[test]
    fn markdown_to_context7_respects_output_limit(
        md in ".{0,1000}",
        summary in ".{0,100}"
    ) {
        let out = markdown_to_context7(
            &md,
            &summary,
            "https://pypi.org/project/demo/",
            Ecosystem::Python,
            "demo",
        );
        // Either fits within MAX_OUTPUT_CHARS or ends with truncation marker
        let suffix = "\n... (truncated)";
        let effective = out.strip_suffix(suffix).unwrap_or(&out);
        prop_assert!(effective.len() <= MAX_OUTPUT_CHARS);
    }

    #[test]
    fn markdown_with_topic_and_max_chars_never_panics_and_respects_budget(
        md in ".{0,1000}",
        topic in ".{0,60}",
        budget in 0_usize..25_000
    ) {
        let clamped = clamp_max_chars(Some(budget));
        let out = markdown_to_context7_with_options(
            &md,
            "summary",
            "https://pypi.org/project/demo/",
            Ecosystem::Python,
            "demo",
            Some(&topic),
            Some(budget),
        );
        let suffix = "\n... (truncated)";
        let effective = out.strip_suffix(suffix).unwrap_or(&out);
        prop_assert!(effective.len() <= clamped);
    }
}
