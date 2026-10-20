//! Render core: compile, PDF/A-3b export, diagnostics, sandbox, digits, fonts.

mod common;

use rustypdf::render::{RenderError, document_text};
use rustypdf::world::ProjectFiles;
use serde_json::json;

fn project(main: &str) -> ProjectFiles {
    ProjectFiles::from_memory([("/t/main.typ", main.as_bytes().to_vec())])
}

fn all_text(doc: &typst_layout::PagedDocument) -> String {
    document_text(doc)
        .into_iter()
        .map(|r| r.text)
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
fn hello_renders_pdf_a() {
    let data = common::sample("hello");
    let rendered = common::render_in(common::templates(), "/hello/main.typ", &data).unwrap();
    let pdf = &rendered.pdf;
    assert!(pdf.starts_with(b"%PDF-"));
    let text = String::from_utf8_lossy(pdf);
    // PDF/A identification lives in the XMP metadata.
    assert!(
        text.contains("<pdfaid:part>3</pdfaid:part>"),
        "missing PDF/A part"
    );
    assert!(
        text.contains("<pdfaid:conformance>B</pdfaid:conformance>"),
        "missing conformance B"
    );
    assert!(
        !text.contains("/EmbeddedFiles"),
        "output must not carry attachments"
    );
}

#[test]
fn identical_input_gives_identical_pdf() {
    let data = common::sample("hello");
    let a = common::render_in(common::templates(), "/hello/main.typ", &data).unwrap();
    let b = common::render_in(common::templates(), "/hello/main.typ", &data).unwrap();
    assert_eq!(a.pdf, b.pdf);
}

#[test]
fn data_json_values_appear_in_output() {
    let doc = common::compile(
        "hello",
        &json!({ "name": "Rahim", "message": "Invoice R-1001" }),
    )
    .unwrap();
    let text = all_text(&doc);
    assert!(text.contains("Rahim"), "{text}");
    assert!(text.contains("R-1001"), "{text}");
}

#[test]
fn syntax_error_reports_line_and_column() {
    let p = project("= Title\n\nok line\n#let x = (1, 2\n");
    let err = common::compile_in(&p, "/t/main.typ", &json!({}), &Default::default()).unwrap_err();
    let RenderError::Compile(diags) = &err else {
        panic!("{err:?}")
    };
    let d = &diags[0];
    assert_eq!(d.file.as_deref(), Some("/t/main.typ"));
    assert_eq!(d.line, Some(4), "{d:?}");
    assert!(d.column.is_some(), "{d:?}");
}

#[test]
fn runtime_error_points_at_the_call() {
    let p = project("#let data = json(\"/data.json\")\n\nName: #data.missing\n");
    let err = common::compile_in(&p, "/t/main.typ", &json!({}), &Default::default()).unwrap_err();
    let d = &err.diagnostics()[0];
    // Column 13 is the `missing` field access in `Name: #data.missing`.
    assert_eq!((d.line, d.column), (Some(3), Some(13)), "{d:?}");
    assert!(d.message.contains("missing"), "{d:?}");
}

#[test]
fn sandbox_escape_is_rejected() {
    for path in ["../../../../etc/passwd", "/etc/passwd", "/../etc/hostname"] {
        let p = project(&format!("#read(\"{path}\")"));
        let err = common::compile_in(&p, "/t/main.typ", &json!({}), &Default::default());
        assert!(err.is_err(), "{path} must not be readable");
    }
    // Real files outside the project root are never loaded.
    let p = project("#read(\"/Cargo.toml\")");
    assert!(common::compile_in(&p, "/t/main.typ", &json!({}), &Default::default()).is_err());
}

#[test]
fn packages_are_unavailable() {
    let p = project("#import \"@preview/example:0.1.0\": *");
    assert!(common::compile_in(&p, "/t/main.typ", &json!({}), &Default::default()).is_err());
}

#[test]
fn missing_main_is_reported() {
    let p = project("hi");
    let err =
        common::compile_in(&p, "/nope/main.typ", &json!({}), &Default::default()).unwrap_err();
    assert!(matches!(err, RenderError::MainNotFound(_)));
}

#[test]
fn templates_can_import_shared_files() {
    let p = ProjectFiles::from_memory([
        ("/_shared/x.typ", b"#let greet = \"shared ok\"".to_vec()),
        (
            "/t/main.typ",
            b"#import \"/_shared/x.typ\": greet\n#greet".to_vec(),
        ),
    ]);
    let doc = common::compile_in(&p, "/t/main.typ", &json!({}), &Default::default()).unwrap();
    assert!(all_text(&doc).contains("shared ok"));
}

// ---- P2: Bangla + English ------------------------------------------------

const BN: std::ops::RangeInclusive<char> = '\u{0980}'..='\u{09FF}';

#[test]
fn digits_are_never_converted() {
    let doc = common::compile("bilingual-test", &common::sample("bilingual-test")).unwrap();
    // Runs split at script changes (e.g. "৩৭৬" | "." | "৫"), so join without spaces.
    let text: String = document_text(&doc).into_iter().map(|r| r.text).collect();
    // Inputs print as received.
    assert!(text.contains("৪৫০"), "{text}");
    assert!(text.contains("450"), "{text}");
    assert!(text.contains("0123456789"), "{text}");
    assert!(text.contains("০১২৩৪৫৬৭৮৯"), "{text}");
    // Computed values follow the digit system of their input.
    assert!(text.contains("৯০০"), "Bangla input → Bangla total: {text}");
    assert!(text.contains("900"), "ASCII input → ASCII total: {text}");
    assert!(text.contains("৩৭৬.৫"), "{text}");
}

#[test]
fn digits_like_and_num_helpers() {
    let p = ProjectFiles::from_memory([
        (
            "/_shared/lang.typ",
            std::fs::read(common::repo().join("templates/_shared/lang.typ")).unwrap(),
        ),
        (
            "/t/main.typ",
            r#"#import "/_shared/lang.typ": num, digits-like
#let d = json("/data.json")
#assert.eq(num("১২৩৪.৫০"), 1234.5)
#assert.eq(num("1,234.50"), 1234.5)
#assert.eq(num(7), 7)
#assert.eq(digits-like(1234.5, "১"), "১২৩৪.৫")
#assert.eq(digits-like(1234.5, "1"), "1234.5")
#assert.eq(digits-like(42, 3), "42")
#assert.eq(digits-like(num(d.a) * 2, d.a), "৮৪")
ok"#
            .as_bytes()
            .to_vec(),
        ),
    ]);
    common::compile_in(
        &p,
        "/t/main.typ",
        &json!({ "a": "৪২" }),
        &Default::default(),
    )
    .unwrap();
}

#[test]
fn each_script_uses_its_own_font() {
    let doc = common::compile("bilingual-test", &common::sample("bilingual-test")).unwrap();
    let runs = document_text(&doc);
    let mut bangla_runs = 0;
    for run in &runs {
        let has_bn = run.text.chars().any(|c| BN.contains(&c));
        let has_latin = run.text.chars().any(|c| c.is_ascii_alphanumeric());
        if has_bn {
            bangla_runs += 1;
            assert_eq!(run.font_family, "Noto Sans Bengali", "{run:?}");
        }
        if has_latin {
            assert_eq!(run.font_family, "Inter", "{run:?}");
        }
        assert!(!(has_bn && has_latin), "one run mixes scripts: {run:?}");
    }
    assert!(bangla_runs > 10, "expected Bangla text, got {runs:?}");
}

#[test]
fn pdf_embeds_both_fonts() {
    let data = common::sample("bilingual-test");
    let rendered =
        common::render_in(common::templates(), "/bilingual-test/main.typ", &data).unwrap();
    let pdf = String::from_utf8_lossy(&rendered.pdf);
    assert!(pdf.contains("NotoSansBengali"), "Bangla font not embedded");
    assert!(pdf.contains("Inter"), "Inter not embedded");
}

#[test]
fn input_is_nfc_normalized_before_compile() {
    // কো typed as KA + E-sign + AA-sign shapes the same as the composed form.
    let decomposed =
        common::compile("hello", &json!({ "name": "\u{0995}\u{09C7}\u{09BE}" })).unwrap();
    let composed = common::compile("hello", &json!({ "name": "\u{0995}\u{09CB}" })).unwrap();
    assert_eq!(all_text(&decomposed), all_text(&composed));
}
