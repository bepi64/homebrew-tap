use std::fs;
use std::path::{Path, PathBuf};

use crate::cache::DiskCache;
use crate::extractors::{
    extract_document_sections, MAX_RICH_DOC_FILE_BYTES, SUPPORTED_DOC_EXTENSIONS,
};
use crate::ocr::{is_ocr_image_file, MAX_OCR_IMAGES};
use crate::tools::{
    render_doc_sections_paginated, DocSection, MAX_ALLOWED_OUTPUT_CHARS, MIN_OUTPUT_CHARS,
};

const MAX_DEPTH: usize = 6;
const MAX_LOCAL_FILES: usize = 200;

const IGNORED_DIRS: [&str; 13] = [
    ".git",
    ".github",
    ".ssh",
    ".gnupg",
    ".trash",
    ".cache",
    ".venv",
    "venv",
    "target",
    "node_modules",
    "vendor",
    "dist",
    "build",
];

fn local_doc_priority(rel_path: &str) -> u8 {
    let lower = rel_path.to_ascii_lowercase();
    let file_name = Path::new(&lower)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(&lower);
    if file_name.starts_with("changelog")
        || file_name.starts_with("history")
        || file_name.starts_with("changes")
    {
        return 5;
    }
    if file_name.starts_with("migration") {
        return 4;
    }
    if file_name.starts_with("contributing") {
        return 3;
    }
    if !lower.contains('/') && file_name.starts_with("readme") {
        return 0;
    }
    if lower.starts_with("docs/") || lower.starts_with("doc/") || lower.starts_with("site/") {
        return 1;
    }
    2
}

const FORBIDDEN_SECRET_EXTENSIONS: [&str; 5] = ["pem", "key", "p12", "pfx", "kdbx"];

fn is_forbidden_filename(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.starts_with(".env")
        || lower.starts_with("id_rsa")
        || lower.starts_with("id_ed25519")
        || lower == ".netrc"
        || lower == ".git-credentials"
        || Path::new(&lower)
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| FORBIDDEN_SECRET_EXTENSIONS.contains(&ext))
}

fn is_doc_file(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    if is_forbidden_filename(name) {
        return false;
    }
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            SUPPORTED_DOC_EXTENSIONS
                .iter()
                .any(|allowed| ext.eq_ignore_ascii_case(allowed))
        })
}

fn is_image_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|name| !is_forbidden_filename(name))
        && is_ocr_image_file(path)
}

fn collect_doc_files(
    dir: &Path,
    canonical_root: &Path,
    depth: usize,
    docs: &mut Vec<PathBuf>,
    images: &mut Vec<PathBuf>,
) {
    if depth > MAX_DEPTH || (docs.len() >= MAX_LOCAL_FILES && images.len() >= MAX_LOCAL_FILES) {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };

    let mut children: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    children.sort();

    for path in children {
        if docs.len() >= MAX_LOCAL_FILES && images.len() >= MAX_LOCAL_FILES {
            break;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };

        // Check symlink_metadata first to avoid traversing symlinks into ignored or external trees.
        let Ok(sym_meta) = fs::symlink_metadata(&path) else {
            continue;
        };

        if sym_meta.is_dir() {
            if IGNORED_DIRS
                .iter()
                .any(|ignored| name.eq_ignore_ascii_case(ignored))
            {
                continue;
            }
            collect_doc_files(&path, canonical_root, depth + 1, docs, images);
            continue;
        }

        let target = if is_doc_file(&path) && docs.len() < MAX_LOCAL_FILES {
            &mut *docs
        } else if is_image_file(&path) && images.len() < MAX_LOCAL_FILES {
            &mut *images
        } else {
            continue;
        };
        let Ok(canonical) = fs::canonicalize(&path) else {
            continue;
        };
        // Strictly block symlinks pointing outside the canonical project root.
        if !canonical.starts_with(canonical_root) {
            continue;
        }
        let Ok(meta) = fs::metadata(&canonical) else {
            continue;
        };
        if meta.is_file() && meta.len() <= MAX_RICH_DOC_FILE_BYTES {
            target.push(canonical);
        }
    }
}

/// Order files so `README.md` and `docs/` come before CONTRIBUTING, MIGRATION, and CHANGELOG.
fn sort_by_priority(files: &mut [PathBuf], canonical_root: &Path) {
    files.sort_by(|a, b| {
        let rel_a = a
            .strip_prefix(canonical_root)
            .unwrap_or(a)
            .to_string_lossy();
        let rel_b = b
            .strip_prefix(canonical_root)
            .unwrap_or(b)
            .to_string_lossy();
        local_doc_priority(&rel_a)
            .cmp(&local_doc_priority(&rel_b))
            .then_with(|| rel_a.cmp(&rel_b))
    });
}

/// Scan local documentation files inside `root` (Markdown, text, Jupyter notebooks, Office,
/// `OpenDocument`, EPUB, PDF, and images when built with OCR), rank sections by optional
/// `topic`, and return paginated Context7 blocks. OCR results are cached in `cache`.
///
/// # Errors
/// Returns an error if `root` does not exist, is not a directory, or contains no readable
/// documentation files.
pub fn search_local_docs(
    root: &Path,
    topic: Option<&str>,
    max_chars: usize,
    offset: usize,
    cache: Option<&DiskCache>,
) -> Result<String, String> {
    if !root.is_dir() {
        return Err(format!(
            "project directory '{}' does not exist or is not a directory",
            root.display()
        ));
    }

    let canonical_root = fs::canonicalize(root)
        .map_err(|err| format!("failed to resolve '{}': {err}", root.display()))?;

    let mut files = Vec::new();
    let mut images = Vec::new();
    collect_doc_files(&canonical_root, &canonical_root, 0, &mut files, &mut images);

    if files.is_empty() && images.is_empty() {
        return Err(format!(
            "no supported documentation files found under '{}'",
            root.display()
        ));
    }

    sort_by_priority(&mut files, &canonical_root);
    sort_by_priority(&mut images, &canonical_root);
    if images.len() > MAX_OCR_IMAGES {
        tracing::debug!(
            "OCR limited to the first {MAX_OCR_IMAGES} of {} images",
            images.len()
        );
        images.truncate(MAX_OCR_IMAGES);
    }

    let mut all_sections: Vec<DocSection> = Vec::new();
    for file in &files {
        let rel = file
            .strip_prefix(&canonical_root)
            .unwrap_or(file)
            .display()
            .to_string();
        match extract_document_sections(file, &rel) {
            Ok(sections) => all_sections.extend(sections),
            Err(err) => {
                tracing::debug!("skipping unreadable local doc '{rel}': {err}");
            }
        }
    }

    #[cfg(fast_docs_ocr)]
    all_sections.extend(crate::ocr::ocr_image_sections(
        &images,
        &canonical_root,
        cache,
    ));
    // Without OCR support no image is ever collected, so the cache has nothing to store.
    #[cfg(not(fast_docs_ocr))]
    let _ = cache;

    if all_sections.is_empty() {
        return Err(format!(
            "documentation files under '{}' contained no extractable sections",
            root.display()
        ));
    }

    let budget = max_chars.clamp(MIN_OUTPUT_CHARS, MAX_ALLOWED_OUTPUT_CHARS);
    Ok(render_doc_sections_paginated(
        &all_sections,
        "local",
        "markdown",
        topic,
        budget,
        offset,
    ))
}
