use fast_docs::tools::{
    clamp_max_chars, detect_lang, html_to_context7, html_to_context7_with_options,
    markdown_to_context7, markdown_to_context7_with_options, resolve_go_module,
    slice_with_pagination, strip_html_tags, truncate_smart, urlencoding, Ecosystem,
    MAX_ALLOWED_OUTPUT_CHARS, MAX_OUTPUT_CHARS, MIN_OUTPUT_CHARS,
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
fn topic_filtering_falls_back_gracefully_when_no_section_matches() {
    let html = r#"
    <html><body>
      <div class="docblock">
        <h1>Overview</h1>
        <p>Core documentation for the demo crate with basic usage examples.</p>
        <pre>let x = demo::start();</pre>
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
    assert!(rendered.contains("TITLE: Overview"));
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
