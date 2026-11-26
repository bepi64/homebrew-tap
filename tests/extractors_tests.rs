use std::fs;
use std::io::Write as _;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use fast_docs::extractors::{extract_document_sections, extract_xml_text, MAX_EXTRACTED_BYTES};
use fast_docs::local_docs::search_local_docs;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

fn unique_dir(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    std::env::temp_dir().join(format!(
        "fast-docs-extract-{label}-{}-{nanos}",
        std::process::id()
    ))
}

fn build_zip_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut cursor = std::io::Cursor::new(Vec::new());
    {
        let mut writer = ZipWriter::new(&mut cursor);
        let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        for (name, data) in entries {
            assert!(writer.start_file(*name, opts).is_ok());
            assert!(writer.write_all(data).is_ok());
        }
        assert!(writer.finish().is_ok());
    }
    cursor.into_inner()
}

/// Build a minimal valid PDF 1.4 containing one text stream.
fn build_minimal_pdf(text: &str) -> Vec<u8> {
    let stream = format!("BT /F1 12 Tf 72 720 Td ({text}) Tj ET");
    let mut pdf = Vec::new();
    let mut offsets = Vec::new();

    pdf.extend_from_slice(b"%PDF-1.4\n");

    let objs = [
        "1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_string(),
        "2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".to_string(),
        "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>\nendobj\n".to_string(),
        format!(
            "4 0 obj\n<< /Length {} >>\nstream\n{stream}\nendstream\nendobj\n",
            stream.len()
        ),
        "5 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n".to_string(),
    ];

    for obj in &objs {
        offsets.push(pdf.len());
        pdf.extend_from_slice(obj.as_bytes());
    }

    let xref_pos = pdf.len();
    pdf.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for off in offsets {
        pdf.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_pos}\n%%EOF\n",
            objs.len() + 1
        )
        .as_bytes(),
    );
    pdf
}

#[test]
fn xml_text_extractor_strips_doctype_xxe_and_decodes_basic_entities() {
    let hostile_xml = r#"<?xml version="1.0"?>
<!DOCTYPE foo [ <!ENTITY xxe SYSTEM "file:///etc/passwd"> ]>
<w:document><w:body>
  <w:p><w:r><w:t>Architecture &amp; Security &lt;v1&gt; &xxe;</w:t></w:r></w:p>
  <!-- secret comment -->
  <w:p><w:r><w:t>Second paragraph</w:t></w:r></w:p>
</w:body></w:document>"#;

    let text = extract_xml_text(hostile_xml);
    assert!(text.contains("Architecture & Security <v1>"));
    assert!(text.contains("Second paragraph"));
    assert!(!text.contains("DOCTYPE"));
    assert!(!text.contains("ENTITY"));
    assert!(!text.contains("secret comment"));
    assert!(!text.contains("/etc/passwd"));
}

#[test]
fn extracts_ipynb_docx_pptx_xlsx_odt_epub_and_pdf_cleanly() {
    let dir = unique_dir("formats");
    assert!(fs::create_dir_all(&dir).is_ok());

    // 1. Jupyter Notebook (.ipynb)
    let ipynb = r##"{
      "cells": [
        { "cell_type": "markdown", "source": ["# Data Pipeline\n", "Normalizes telemetry events."] },
        { "cell_type": "code", "source": ["import pandas as pd\n", "df = pd.read_csv('events.csv')"] }
      ]
    }"##;
    let ipynb_path = dir.join("pipeline.ipynb");
    assert!(fs::write(&ipynb_path, ipynb).is_ok());
    let nb_sections = extract_document_sections(&ipynb_path, "pipeline.ipynb");
    assert!(nb_sections.is_ok());
    if let Ok(secs) = nb_sections {
        assert!(!secs.is_empty());
        assert_eq!(secs[0].title, "Data Pipeline");
        assert!(secs[0].codes.iter().any(|c| c.contains("pd.read_csv")));
    }

    // 2. Word (.docx)
    let docx_xml = b"<w:document><w:body><w:p><w:r><w:t>Zero Trust Policy Specification</w:t></w:r></w:p></w:body></w:document>";
    let docx_path = dir.join("spec.docx");
    assert!(fs::write(
        &docx_path,
        build_zip_bytes(&[("word/document.xml", docx_xml)])
    )
    .is_ok());
    let docx_secs = extract_document_sections(&docx_path, "spec.docx");
    assert!(docx_secs.is_ok());
    if let Ok(secs) = docx_secs {
        assert!(secs.iter().any(|s| s
            .descriptions
            .iter()
            .any(|d| d.contains("Zero Trust Policy"))));
    }

    // 3. PowerPoint (.pptx)
    let slide1 = b"<p:sld><p:cSld><p:spTree><p:sp><p:txBody><a:p><a:r><a:t>Architecture Slide One</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:sld>";
    let pptx_path = dir.join("deck.pptx");
    assert!(fs::write(
        &pptx_path,
        build_zip_bytes(&[("ppt/slides/slide1.xml", slide1)])
    )
    .is_ok());
    let pptx_secs = extract_document_sections(&pptx_path, "deck.pptx");
    assert!(pptx_secs.is_ok());
    if let Ok(secs) = pptx_secs {
        assert!(secs.iter().any(|s| s
            .descriptions
            .iter()
            .any(|d| d.contains("Architecture Slide One"))));
    }

    // 4. Excel (.xlsx)
    let shared = b"<sst><si><t>Quarterly Latency SLA</t></si><si><t>p99 under 5ms</t></si></sst>";
    let xlsx_path = dir.join("sla.xlsx");
    assert!(fs::write(
        &xlsx_path,
        build_zip_bytes(&[("xl/sharedStrings.xml", shared)])
    )
    .is_ok());
    let xlsx_secs = extract_document_sections(&xlsx_path, "sla.xlsx");
    assert!(xlsx_secs.is_ok());
    if let Ok(secs) = xlsx_secs {
        assert!(secs.iter().any(|s| s
            .descriptions
            .iter()
            .any(|d| d.contains("Quarterly Latency SLA"))));
    }

    // 5. OpenDocument (.odt)
    let odt_xml = b"<office:document-content><office:body><office:text><text:p>OpenDocument Engineering Handbook</text:p></office:text></office:body></office:document-content>";
    let odt_path = dir.join("handbook.odt");
    assert!(fs::write(&odt_path, build_zip_bytes(&[("content.xml", odt_xml)])).is_ok());
    let odt_secs = extract_document_sections(&odt_path, "handbook.odt");
    assert!(odt_secs.is_ok());
    if let Ok(secs) = odt_secs {
        assert!(secs.iter().any(|s| s
            .descriptions
            .iter()
            .any(|d| d.contains("OpenDocument Engineering Handbook"))));
    }

    // End-to-end search_local_docs finds rich Office formats by topic
    let found = search_local_docs(&dir, Some("Zero Trust"), 2000, 0);
    assert!(found.is_ok());
    if let Ok(text) = found {
        assert!(text.contains("SOURCE: spec.docx"));
        assert!(text.contains("Zero Trust Policy"));
    }

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn extracts_epub_and_pdf_cleanly() {
    let dir = unique_dir("epub-pdf");
    assert!(fs::create_dir_all(&dir).is_ok());

    // EPUB (.epub)
    let xhtml = b"<html><body><article><h1>Chapter One</h1><p>EPUB async networking design notes and invariants.</p><pre>let sock = TcpListener::bind(addr).await?;</pre></article></body></html>";
    let epub_path = dir.join("book.epub");
    assert!(fs::write(&epub_path, build_zip_bytes(&[("OEBPS/ch1.xhtml", xhtml)])).is_ok());
    let epub_secs = extract_document_sections(&epub_path, "book.epub");
    assert!(epub_secs.is_ok());
    if let Ok(secs) = epub_secs {
        assert!(secs.iter().any(|s| s.title == "Chapter One"));
        assert!(secs
            .iter()
            .any(|s| s.codes.iter().any(|c| c.contains("TcpListener::bind"))));
    }

    // PDF (.pdf)
    let pdf_bytes = build_minimal_pdf("FastDocs PDF Extraction Verified");
    let pdf_path = dir.join("whitepaper.pdf");
    assert!(fs::write(&pdf_path, pdf_bytes).is_ok());
    let pdf_secs = extract_document_sections(&pdf_path, "whitepaper.pdf");
    assert!(pdf_secs.is_ok());
    if let Ok(secs) = pdf_secs {
        assert!(secs.iter().any(|s| s
            .descriptions
            .iter()
            .any(|d| d.contains("FastDocs PDF Extraction Verified"))));
    }

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn rejects_zip_bomb_and_handles_corrupted_pdf_without_panicking() {
    let dir = unique_dir("hostile");
    assert!(fs::create_dir_all(&dir).is_ok());

    // Zip bomb: highly compressible payload exceeding MAX_EXTRACTED_BYTES
    let huge_xml = format!(
        "<w:document><w:body><w:p><w:t>{}</w:t></w:p></w:body></w:document>",
        "A".repeat(MAX_EXTRACTED_BYTES + 200_000)
    );
    let bomb_path = dir.join("bomb.docx");
    assert!(fs::write(
        &bomb_path,
        build_zip_bytes(&[("word/document.xml", huge_xml.as_bytes())])
    )
    .is_ok());
    let bomb_res = extract_document_sections(&bomb_path, "bomb.docx");
    assert!(
        bomb_res.is_err(),
        "zip bomb exceeding MAX_EXTRACTED_BYTES must be rejected"
    );

    // Corrupted PDF must return Err cleanly and never panic
    let bad_pdf = dir.join("corrupt.pdf");
    assert!(fs::write(&bad_pdf, b"%PDF-1.7\n\xff\xfe\x00corrupt binary data").is_ok());
    let pdf_res = extract_document_sections(&bad_pdf, "corrupt.pdf");
    assert!(pdf_res.is_err());

    let _ = fs::remove_dir_all(&dir);
}
