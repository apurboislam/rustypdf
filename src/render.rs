//! Compile a template with request data and export PDF/A-3b.

use std::collections::HashMap;

use chrono::{DateTime, Datelike, Timelike, Utc};
use serde::Serialize;
use serde_json::Value;
use typst::diag::SourceDiagnostic;
use typst::foundations::{Bytes, Datetime, Smart};
use typst::syntax::{FileId, VirtualRoot};
use typst::{World, WorldExt};
use typst_layout::PagedDocument;
use typst_pdf::{PdfOptions, PdfStandard, PdfStandards, Timestamp};
use unicode_normalization::{IsNormalized, UnicodeNormalization, is_nfc_quick};

use crate::world::{ProjectFiles, RenderWorld, Shared};

/// A Typst diagnostic, mapped to a stable JSON shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Diagnostic {
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    #[error("template entry file not found: {0}")]
    MainNotFound(String),
    #[error("template failed to compile: {}", summary(.0))]
    Compile(Vec<Diagnostic>),
    #[error("PDF export failed: {}", summary(.0))]
    Pdf(Vec<Diagnostic>),
}

impl RenderError {
    pub fn diagnostics(&self) -> &[Diagnostic] {
        match self {
            Self::MainNotFound(_) => &[],
            Self::Compile(d) | Self::Pdf(d) => d,
        }
    }
}

fn summary(diags: &[Diagnostic]) -> String {
    let Some(first) = diags.first() else {
        return "unknown error".into();
    };
    let mut s = first.message.clone();
    if let (Some(file), Some(line), Some(col)) = (&first.file, first.line, first.column) {
        s = format!("{file}:{line}:{col}: {s}");
    }
    if diags.len() > 1 {
        s.push_str(&format!(" (and {} more)", diags.len() - 1));
    }
    s
}

/// Everything one render needs besides the shared state.
pub struct RenderInput<'a> {
    pub project: &'a ProjectFiles,
    /// Virtual path of the entry file, e.g. `/hello/main.typ`.
    pub main: &'a str,
    /// The request JSON, served as `/data.json`.
    pub data: &'a Value,
    /// Resolved images, keyed by `/__img/...` virtual path.
    pub images: &'a HashMap<String, Bytes>,
    pub now: DateTime<Utc>,
}

#[derive(Debug)]
pub struct Rendered {
    pub pdf: Vec<u8>,
    pub warnings: Vec<Diagnostic>,
}

/// Compiles the template into a laid-out document.
pub fn compile(
    shared: &Shared,
    input: &RenderInput,
) -> Result<(PagedDocument, Vec<Diagnostic>), RenderError> {
    compile_data(shared, input, serialize_data(input.data))
}

/// [`compile`] with the request JSON already serialized.
fn compile_data(
    shared: &Shared,
    input: &RenderInput,
    data: Bytes,
) -> Result<(PagedDocument, Vec<Diagnostic>), RenderError> {
    let world = RenderWorld::new(
        shared,
        input.project,
        input.main,
        data,
        input.images,
        input.now,
    )
    .map_err(|_| RenderError::MainNotFound(input.main.into()))?;
    let result = typst::compile::<PagedDocument>(&world);
    let warnings = map_diagnostics(&world, &result.warnings);
    match result.output {
        Ok(doc) => Ok((doc, warnings)),
        Err(errors) => Err(RenderError::Compile(map_diagnostics(&world, &errors))),
    }
}

/// Compiles the template and exports it as PDF/A-3b.
pub fn render(shared: &Shared, input: &RenderInput) -> Result<Rendered, RenderError> {
    let data = serialize_data(input.data);
    let (doc, warnings) = compile_data(shared, input, data.clone())?;
    let options = PdfOptions {
        // Stable per template + data, so identical requests give identical IDs.
        ident: Smart::Custom(format!(
            "{:032x}",
            typst::utils::hash128(&(input.main, data.as_slice()))
        )),
        timestamp: Some(timestamp(input.now)),
        standards: PdfStandards::new(&[PdfStandard::A_3b])
            .expect("PDF/A-3b alone is always a valid standard set"),
        ..PdfOptions::default()
    };
    let pdf = typst_pdf::pdf(&doc, &options).map_err(|errors| {
        // Export diagnostics may point into sources; resolve them against a
        // world equal to the one used to compile.
        match RenderWorld::new(
            shared,
            input.project,
            input.main,
            data,
            input.images,
            input.now,
        ) {
            Ok(world) => RenderError::Pdf(map_diagnostics(&world, &errors)),
            Err(_) => RenderError::MainNotFound(input.main.into()),
        }
    })?;
    Ok(Rendered { pdf, warnings })
}

/// NFC-normalizes every string (keys and values) in place, so text from
/// different input methods shapes the same way.
pub fn normalize_nfc(value: &mut Value) {
    match value {
        Value::String(s) => {
            if is_nfc_quick(s.chars()) != IsNormalized::Yes {
                *s = s.nfc().collect();
            }
        }
        Value::Array(items) => items.iter_mut().for_each(normalize_nfc),
        Value::Object(map) => {
            if map
                .keys()
                .any(|k| is_nfc_quick(k.chars()) != IsNormalized::Yes)
            {
                let old = std::mem::take(map);
                *map = old
                    .into_iter()
                    .map(|(k, v)| (k.nfc().collect(), v))
                    .collect();
            }
            map.values_mut().for_each(normalize_nfc);
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn serialize_data(data: &Value) -> Bytes {
    Bytes::new(serde_json::to_vec(data).expect("JSON values always serialize"))
}

fn timestamp(now: DateTime<Utc>) -> Timestamp {
    let dt = Datetime::from_ymd_hms(
        now.year(),
        now.month() as u8,
        now.day() as u8,
        now.hour() as u8,
        now.minute() as u8,
        now.second() as u8,
    )
    .expect("chrono yields valid date parts");
    Timestamp::new_utc(dt)
}

fn map_diagnostics(world: &RenderWorld, diags: &[SourceDiagnostic]) -> Vec<Diagnostic> {
    diags
        .iter()
        .map(|diag| {
            let id = diag.span.id();
            let (line, column) = id
                .and_then(|id| line_col(world, id, world.range(diag.span)?.start))
                .unzip();
            let hints: Vec<&str> = diag.hints.iter().map(|h| h.v.as_str()).collect();
            Diagnostic {
                message: diag.message.to_string(),
                file: id.map(display_path),
                line,
                column,
                hint: (!hints.is_empty()).then(|| hints.join("\n")),
            }
        })
        .collect()
}

/// 1-based line and column (in characters) of a byte offset in a file.
fn line_col(world: &RenderWorld, id: FileId, byte: usize) -> Option<(usize, usize)> {
    if let Ok(source) = world.source(id) {
        let (line, col) = source.lines().byte_to_line_column(byte)?;
        return Some((line + 1, col + 1));
    }
    // Non-Typst files, e.g. a malformed `/data.json` read as data.
    let bytes = world.file(id).ok()?;
    Some(typst::diag::LineCol::try_from_byte_pos(byte, &bytes)?.numbers())
}

fn display_path(id: FileId) -> String {
    match id.root() {
        VirtualRoot::Project => id.vpath().get_with_slash().to_owned(),
        VirtualRoot::Package(spec) => format!("{spec}{}", id.vpath().get_with_slash()),
    }
}

/// Plain text of every text run on every page, in layout order. Used by
/// tests and diagnostics.
pub fn document_text(doc: &PagedDocument) -> Vec<TextRun> {
    let mut out = Vec::new();
    for page in doc.pages() {
        collect_text(&page.frame, &mut out);
    }
    out
}

/// One shaped text run and the font family it was set in.
#[derive(Debug, Clone)]
pub struct TextRun {
    pub text: String,
    pub font_family: String,
}

fn collect_text(frame: &typst::layout::Frame, out: &mut Vec<TextRun>) {
    use typst::layout::FrameItem;
    for (_, item) in frame.items() {
        match item {
            FrameItem::Group(group) => collect_text(&group.frame, out),
            FrameItem::Text(text) => out.push(TextRun {
                text: text.text.to_string(),
                font_family: text.font.font().info().family.clone(),
            }),
            _ => {}
        }
    }
}

/// Renders one page to PNG at `pixel_per_pt` scale (golden tests, previews).
pub fn page_png(doc: &PagedDocument, page: usize, pixel_per_pt: f32) -> Option<Vec<u8>> {
    let page = doc.pages().get(page)?;
    let options = typst_render::RenderOptions {
        pixel_per_pt: typst::utils::Scalar::new(pixel_per_pt as f64),
        render_bleed: false,
    };
    typst_render::render(page, &options).encode_png().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn nfc_normalizes_keys_and_values() {
        // "কো" written as KA + E-sign + AA-sign (decomposed) vs. the composed O-sign.
        let decomposed = "\u{0995}\u{09C7}\u{09BE}";
        let composed = "\u{0995}\u{09CB}";
        let mut v = json!({ decomposed: [decomposed, 1, null], "n": { "x": decomposed } });
        normalize_nfc(&mut v);
        assert_eq!(
            v,
            json!({ composed: [composed, 1, null], "n": { "x": composed } })
        );
    }
}
