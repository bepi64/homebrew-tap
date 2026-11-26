use std::fs::{self, File};
use std::io::Read as _;
use std::panic::AssertUnwindSafe;
use std::path::Path;

use zip::ZipArchive;

use crate::tools::{parse_html_sections, parse_markdown_sections, DocSection};

/// Maximum raw file size accepted for local rich documents (5 MiB).
pub const MAX_RICH_DOC_FILE_BYTES: u64 = 5 * 1024 * 1024;

/// Maximum total decompressed bytes extracted from a single archive/document (2 MiB)
/// to defeat zip bombs before unbounded allocation.
pub const MAX_EXTRACTED_BYTES: usize = 2 * 1024 * 1024;

/// Maximum number of ZIP archive entries inspected per document.
const MAX_ARCHIVE_ENTRIES: usize = 200;

/// Supported local document extensions (plain text, Markdown, notebooks, Office, EPUB, PDF).
pub const SUPPORTED_DOC_EXTENSIONS: [&str; 13] = [
    "md", "mdx", "txt", "rst", "ipynb", "docx", "pptx", "xlsx", "odt", "odp", "ods", "epub", "pdf",
];

/// Decode basic XML entities without resolving DTD or external entities (XXE-safe).
fn decode_xml_entities(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;

    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp + 1..];
        let Some(semi) = after.find(';') else {
            out.push('&');
            rest = after;
            continue;
        };
        let entity = &after[..semi];
        match entity {
            "amp" => out.push('&'),
            "lt" => out.push('<'),
            "gt" => out.push('>'),
            "quot" => out.push('"'),
            "apos" => out.push('\''),
            num if num.starts_with('#') => {
                let code =
                    if let Some(hex) = num.strip_prefix("#x").or_else(|| num.strip_prefix("#X")) {
                        u32::from_str_radix(hex, 16).ok()
                    } else {
                        num[1..].parse::<u32>().ok()
                    };
                if let Some(ch) = code.and_then(char::from_u32) {
                    out.push(ch);
                }
            }
            // Ignore unknown / custom DTD entities (prevents XXE expansion).
            _ => {}
        }
        rest = &after[semi + 1..];
    }

    out.push_str(rest);
    out
}

fn is_xml_block_closing_tag(tag_name: &str) -> bool {
    matches!(
        tag_name,
        "/w:p"
            | "/a:p"
            | "/text:p"
            | "/text:h"
            | "/si"
            | "/row"
            | "/w:tr"
            | "/table:table-row"
            | "/p"
            | "/div"
            | "/li"
    )
}

/// Extract plain text from Office/OpenDocument XML without loading DTDs or expanding custom
/// entities (strictly XXE-safe).
#[must_use]
pub fn extract_xml_text(xml: &str) -> String {
    let bytes = xml.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut raw_text = String::with_capacity(len / 4);

    while i < len {
        if bytes[i] == b'<' {
            // Skip XML comments <!-- ... -->
            if xml[i..].starts_with("<!--") {
                if let Some(end) = xml[i + 4..].find("-->") {
                    i += 4 + end + 3;
                    continue;
                }
                break;
            }
            // Skip <!DOCTYPE ... [ ... ]> or <!ENTITY ...>
            if xml[i..].starts_with("<!DOCTYPE") || xml[i..].starts_with("<!doctype") {
                let mut depth = 0_usize;
                i += 2;
                while i < len {
                    match bytes[i] {
                        b'[' => depth += 1,
                        b']' => depth = depth.saturating_sub(1),
                        b'>' if depth == 0 => {
                            i += 1;
                            break;
                        }
                        _ => {}
                    }
                    i += 1;
                }
                continue;
            }
            // Skip <?xml ...?> processing instructions
            if xml[i..].starts_with("<?") {
                if let Some(end) = xml[i + 2..].find("?>") {
                    i += 2 + end + 2;
                    continue;
                }
                break;
            }
            // Regular XML tag
            if let Some(rel_end) = xml[i + 1..].find('>') {
                let tag_content = xml[i + 1..i + 1 + rel_end].trim();
                let tag_name = tag_content.split_whitespace().next().unwrap_or("");
                if is_xml_block_closing_tag(tag_name) {
                    raw_text.push('\n');
                }
                i += rel_end + 2;
                continue;
            }
            break;
        }

        let next_tag = xml[i..].find('<').map_or(len, |rel| i + rel);
        raw_text.push_str(&xml[i..next_tag]);
        i = next_tag;
    }

    let decoded = decode_xml_entities(&raw_text);
    let mut lines = Vec::new();
    for line in decoded.lines() {
        let collapsed = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if !collapsed.is_empty() {
            lines.push(collapsed);
        }
    }
    lines.join("\n")
}

fn read_zip_entry_bounded(
    archive: &mut ZipArchive<File>,
    index: usize,
    remaining_budget: &mut usize,
) -> Result<String, String> {
    let entry = archive
        .by_index(index)
        .map_err(|err| format!("failed to open zip entry {index}: {err}"))?;

    if entry.is_dir() {
        return Ok(String::new());
    }

    let limit = (*remaining_budget).saturating_add(1) as u64;
    let mut limited = entry.take(limit);
    let mut buf = Vec::new();
    limited
        .read_to_end(&mut buf)
        .map_err(|err| format!("failed to decompress zip entry: {err}"))?;

    if buf.len() > *remaining_budget {
        return Err(format!(
            "archive decompressed content exceeds maximum allowed {MAX_EXTRACTED_BYTES} bytes"
        ));
    }
    *remaining_budget = remaining_budget.saturating_sub(buf.len());
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

fn lines_to_sections(text: &str, default_title: &str, rel_path: &str) -> Vec<DocSection> {
    let mut descriptions = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            descriptions.push(trimmed.to_string());
        }
    }
    if descriptions.is_empty() {
        return Vec::new();
    }
    vec![DocSection {
        title: default_title.to_string(),
        descriptions,
        codes: Vec::new(),
        code_langs: Vec::new(),
        source: Some(rel_path.to_string()),
    }]
}

fn extract_ipynb_sections(raw: &str, rel_path: &str) -> Result<Vec<DocSection>, String> {
    let json = serde_json::from_str::<serde_json::Value>(raw)
        .map_err(|err| format!("invalid notebook JSON in '{rel_path}': {err}"))?;
    let Some(cells) = json["cells"].as_array() else {
        return Ok(Vec::new());
    };

    let mut sections: Vec<DocSection> = Vec::new();
    let mut cur_title = format!("{rel_path} Notebook");
    let mut cur_desc: Vec<String> = Vec::new();
    let mut cur_code: Vec<String> = Vec::new();
    let mut cur_code_langs: Vec<Option<String>> = Vec::new();

    for cell in cells {
        let cell_type = cell["cell_type"].as_str().unwrap_or("");
        let source_text = match &cell["source"] {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Array(arr) => arr
                .iter()
                .filter_map(serde_json::Value::as_str)
                .collect::<Vec<_>>()
                .join(""),
            _ => continue,
        };

        if cell_type == "markdown" {
            let parsed = parse_markdown_sections(&source_text, "", rel_path, rel_path);
            for sec in parsed {
                if !cur_desc.is_empty() || !cur_code.is_empty() {
                    sections.push(DocSection {
                        title: cur_title.clone(),
                        descriptions: std::mem::take(&mut cur_desc),
                        codes: std::mem::take(&mut cur_code),
                        code_langs: std::mem::take(&mut cur_code_langs),
                        source: Some(rel_path.to_string()),
                    });
                }
                cur_title = sec.title;
                cur_desc = sec.descriptions;
                cur_code = sec.codes;
                cur_code_langs = sec.code_langs;
            }
        } else if cell_type == "code" {
            let trimmed = source_text.trim();
            if !trimmed.is_empty() {
                cur_code.push(trimmed.to_string());
                cur_code_langs.push(Some("python".to_string()));
            }
        }
    }

    if !cur_desc.is_empty() || !cur_code.is_empty() {
        sections.push(DocSection {
            title: cur_title,
            descriptions: cur_desc,
            codes: cur_code,
            code_langs: cur_code_langs,
            source: Some(rel_path.to_string()),
        });
    }

    Ok(sections)
}

fn slide_number(name: &str) -> usize {
    let stem = name
        .rsplit('/')
        .next()
        .unwrap_or(name)
        .trim_start_matches("slide")
        .trim_end_matches(".xml");
    stem.parse::<usize>().unwrap_or(usize::MAX)
}

fn extract_office_zip_sections(
    path: &Path,
    ext: &str,
    rel_path: &str,
) -> Result<Vec<DocSection>, String> {
    let file = File::open(path).map_err(|err| format!("failed to open '{rel_path}': {err}"))?;
    let mut archive =
        ZipArchive::new(file).map_err(|err| format!("invalid ZIP archive '{rel_path}': {err}"))?;

    if archive.len() > MAX_ARCHIVE_ENTRIES {
        return Err(format!(
            "archive '{rel_path}' exceeds maximum entry count ({MAX_ARCHIVE_ENTRIES})"
        ));
    }

    let mut targets: Vec<(usize, String)> = Vec::new();
    for idx in 0..archive.len() {
        let Ok(entry) = archive.by_index(idx) else {
            continue;
        };
        let Ok(name_cow) = entry.name() else {
            continue;
        };
        let name = name_cow.into_owned();
        let matched = match ext {
            "docx" => name == "word/document.xml",
            "pptx" => {
                name.starts_with("ppt/slides/slide")
                    && Path::new(&name)
                        .extension()
                        .is_some_and(|e| e.eq_ignore_ascii_case("xml"))
            }
            "xlsx" => name == "xl/sharedStrings.xml",
            "odt" | "odp" | "ods" => name == "content.xml",
            "epub" => Path::new(&name).extension().is_some_and(|e| {
                e.eq_ignore_ascii_case("xhtml")
                    || e.eq_ignore_ascii_case("html")
                    || e.eq_ignore_ascii_case("htm")
            }),
            _ => false,
        };
        if matched {
            targets.push((idx, name));
        }
    }

    if ext == "pptx" {
        targets.sort_by_key(|(_, name)| slide_number(name));
    } else {
        targets.sort_by(|a, b| a.1.cmp(&b.1));
    }

    let mut remaining = MAX_EXTRACTED_BYTES;
    let mut sections = Vec::new();

    for (idx, name) in targets {
        let content = read_zip_entry_bounded(&mut archive, idx, &mut remaining)?;
        if content.trim().is_empty() {
            continue;
        }
        if ext == "epub" {
            let mut epub_secs = parse_html_sections(&content, rel_path, rel_path);
            sections.append(&mut epub_secs);
        } else {
            let plain = extract_xml_text(&content);
            let title = if ext == "pptx" {
                let num = slide_number(&name);
                if num == usize::MAX {
                    format!("{rel_path} ({name})")
                } else {
                    format!("{rel_path} — Slide {num}")
                }
            } else {
                rel_path.to_string()
            };
            sections.extend(lines_to_sections(&plain, &title, rel_path));
        }
    }

    Ok(sections)
}

fn extract_pdf_sections(path: &Path, rel_path: &str) -> Result<Vec<DocSection>, String> {
    let bytes = fs::read(path).map_err(|err| format!("failed to read '{rel_path}': {err}"))?;
    if bytes.len() as u64 > MAX_RICH_DOC_FILE_BYTES {
        return Err(format!(
            "PDF '{rel_path}' exceeds maximum allowed size of {MAX_RICH_DOC_FILE_BYTES} bytes"
        ));
    }

    // Wrap third-party PDF parsing in catch_unwind so malformed/corrupt PDFs never crash the server.
    let extracted = std::panic::catch_unwind(AssertUnwindSafe(|| {
        pdf_extract::extract_text_from_mem(&bytes)
    }))
    .map_err(|_| format!("PDF parser aborted on malformed document '{rel_path}'"))?
    .map_err(|err| format!("failed to extract text from PDF '{rel_path}': {err}"))?;

    let bounded = if extracted.len() > MAX_EXTRACTED_BYTES {
        let mut end = MAX_EXTRACTED_BYTES;
        while end > 0 && !extracted.is_char_boundary(end) {
            end -= 1;
        }
        &extracted[..end]
    } else {
        extracted.as_str()
    };

    Ok(lines_to_sections(bounded, rel_path, rel_path))
}

/// Extract structured documentation sections from a local file according to its extension.
///
/// Supports plain text/Markdown (`.md`, `.mdx`, `.txt`, `.rst`), Jupyter notebooks (`.ipynb`),
/// Office/OpenDocument (`.docx`, `.pptx`, `.xlsx`, `.odt`, `.odp`, `.ods`), `.epub`, and `.pdf`.
///
/// # Errors
/// Returns an error if the file exceeds size bounds, fails archive/zip-bomb checks, or cannot be
/// parsed.
pub fn extract_document_sections(path: &Path, rel_path: &str) -> Result<Vec<DocSection>, String> {
    let meta = fs::metadata(path).map_err(|err| format!("failed to stat '{rel_path}': {err}"))?;
    if !meta.is_file() || meta.len() > MAX_RICH_DOC_FILE_BYTES {
        return Err(format!(
            "file '{rel_path}' is not a regular file or exceeds {MAX_RICH_DOC_FILE_BYTES} bytes"
        ));
    }

    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map_or_default(str::to_ascii_lowercase);

    match ext.as_str() {
        "md" | "mdx" | "txt" | "rst" => {
            let text = fs::read_to_string(path)
                .map_err(|err| format!("failed to read '{rel_path}': {err}"))?;
            Ok(parse_markdown_sections(&text, "", rel_path, rel_path))
        }
        "ipynb" => {
            let raw = fs::read_to_string(path)
                .map_err(|err| format!("failed to read '{rel_path}': {err}"))?;
            extract_ipynb_sections(&raw, rel_path)
        }
        "docx" | "pptx" | "xlsx" | "odt" | "odp" | "ods" | "epub" => {
            extract_office_zip_sections(path, &ext, rel_path)
        }
        "pdf" => extract_pdf_sections(path, rel_path),
        other => Err(format!("unsupported document extension '.{other}'")),
    }
}
