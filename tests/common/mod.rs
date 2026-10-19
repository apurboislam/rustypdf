//! Helpers shared by integration tests.
#![allow(dead_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use rustypdf::fonts::FontStore;
use rustypdf::render::{self, RenderError, RenderInput, Rendered};
use rustypdf::world::{ProjectFiles, Shared};
use serde_json::Value;
use typst_layout::PagedDocument;

pub fn repo() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

pub fn shared() -> &'static Shared {
    static SHARED: OnceLock<Shared> = OnceLock::new();
    SHARED.get_or_init(|| {
        let fonts = FontStore::load(
            &repo().join("fonts"),
            &["Noto Sans Bengali".into(), "Inter".into()],
        )
        .expect("bundled fonts load");
        Shared::new(Arc::new(fonts))
    })
}

/// The repo's `templates/` folder.
pub fn templates() -> &'static ProjectFiles {
    static FILES: OnceLock<ProjectFiles> = OnceLock::new();
    FILES.get_or_init(|| ProjectFiles::load(&repo().join("templates")).unwrap())
}

pub fn sample(template: &str) -> Value {
    let path: PathBuf = repo().join("templates").join(template).join("sample.json");
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

/// A fixed timestamp, so outputs are reproducible.
pub fn fixed_now() -> chrono::DateTime<chrono::Utc> {
    "2026-01-01T00:00:00Z".parse().unwrap()
}

pub fn compile_in(
    project: &ProjectFiles,
    main: &str,
    data: &Value,
) -> Result<PagedDocument, RenderError> {
    let mut data = data.clone();
    render::normalize_nfc(&mut data);
    let images = HashMap::new();
    render::compile(
        shared(),
        &RenderInput {
            project,
            main,
            data: &data,
            images: &images,
            now: fixed_now(),
        },
    )
    .map(|(doc, _)| doc)
}

pub fn render_in(
    project: &ProjectFiles,
    main: &str,
    data: &Value,
) -> Result<Rendered, RenderError> {
    let mut data = data.clone();
    render::normalize_nfc(&mut data);
    let images = HashMap::new();
    render::render(
        shared(),
        &RenderInput {
            project,
            main,
            data: &data,
            images: &images,
            now: fixed_now(),
        },
    )
}

/// Compiles a repo template with `data`.
pub fn compile(template: &str, data: &Value) -> Result<PagedDocument, RenderError> {
    compile_in(templates(), &format!("/{template}/main.typ"), data)
}
