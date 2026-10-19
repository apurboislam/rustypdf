//! The template registry: every folder with a `main.typ` under
//! `TEMPLATES_DIR` is a template, served at `POST /render/{name}`.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use arc_swap::ArcSwap;
use chrono::Utc;
use serde_json::Value;

use crate::render::{self, Diagnostic, RenderInput};
use crate::world::{ProjectFiles, Shared};

pub struct Template {
    pub name: String,
    /// Virtual path of the entry file, e.g. `/hello/main.typ`.
    pub main: String,
    /// The template-root snapshot this template was loaded from.
    pub project: Arc<ProjectFiles>,
    /// Parsed `sample.json`, if the folder has one.
    pub sample: Option<Value>,
    /// Errors from the startup test compile, empty if it succeeded.
    pub startup_errors: Vec<Diagnostic>,
}

pub type Templates = HashMap<String, Arc<Template>>;

pub struct TemplateRegistry {
    templates: ArcSwap<Templates>,
}

impl TemplateRegistry {
    pub fn new(templates: Templates) -> Self {
        Self {
            templates: ArcSwap::from_pointee(templates),
        }
    }

    pub fn get(&self, name: &str) -> Option<Arc<Template>> {
        self.templates.load().get(name).cloned()
    }

    /// All templates, sorted by name.
    pub fn list(&self) -> Vec<Arc<Template>> {
        let mut list: Vec<_> = self.templates.load().values().cloned().collect();
        list.sort_by(|a, b| a.name.cmp(&b.name));
        list
    }

    /// Atomically replaces the whole set of templates.
    pub fn swap(&self, templates: Templates) {
        self.templates.store(Arc::new(templates));
    }
}

/// Scans `dir`, loading every file into one shared snapshot, and test-compiles
/// each template (with its `sample.json`, or `{}`) so errors show at startup.
pub fn load(dir: &Path, shared: &Shared) -> Result<Templates> {
    let project = Arc::new(
        ProjectFiles::load(dir)
            .with_context(|| format!("cannot read templates dir {}", dir.display()))?,
    );

    let mut names: Vec<String> = project
        .paths()
        .filter_map(|p| {
            let name = p.strip_prefix('/')?.strip_suffix("/main.typ")?;
            (!name.contains('/') && !name.starts_with('_')).then(|| name.to_owned())
        })
        .collect();
    names.sort();

    let mut templates = HashMap::new();
    for name in names {
        let sample = match project.get(&format!("/{name}/sample.json")) {
            None => None,
            Some(bytes) => match serde_json::from_slice(bytes) {
                Ok(v) => Some(v),
                Err(err) => {
                    tracing::warn!(template = %name, %err, "sample.json is not valid JSON");
                    None
                }
            },
        };
        let main = format!("/{name}/main.typ");
        let mut data = sample
            .clone()
            .unwrap_or_else(|| Value::Object(Default::default()));
        render::normalize_nfc(&mut data);
        let images = HashMap::new();
        let startup_errors = match render::compile(
            shared,
            &RenderInput {
                project: &project,
                main: &main,
                data: &data,
                images: &images,
                now: Utc::now(),
            },
        ) {
            Ok(_) => {
                tracing::info!(template = %name, "template loaded");
                Vec::new()
            }
            Err(err) => {
                tracing::error!(template = %name, error = %err, "template failed its startup compile");
                err.diagnostics().to_vec()
            }
        };
        templates.insert(
            name.clone(),
            Arc::new(Template {
                name,
                main,
                project: project.clone(),
                sample,
                startup_errors,
            }),
        );
    }
    Ok(templates)
}
