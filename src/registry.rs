//! The template registry: every folder with a `main.typ` under
//! `TEMPLATES_DIR` is a template, served at `POST /render/{route}` (the
//! folder name unless `manifest.toml` sets `route`).

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use arc_swap::ArcSwap;
use chrono::Utc;
use serde_json::Value;

use crate::error::FieldError;
use crate::images::{
    self,
    assets::{Image, StaticAssets, resolve_file_path},
};
use crate::manifest::{ImageField, Manifest, Segment, parse_pointer};
use crate::render::{self, Diagnostic, RenderInput};
use crate::world::{ProjectFiles, Shared};

pub struct Template {
    /// Folder name under `TEMPLATES_DIR`.
    pub name: String,
    /// Route segment: `POST /render/{route}`. The manifest's `route`, or the
    /// folder name.
    pub route: String,
    /// Display name: the manifest's `name`, or the folder name.
    pub title: String,
    /// Virtual path of the entry file, e.g. `/hello/main.typ`.
    pub main: String,
    /// The template-root snapshot this template was loaded from.
    pub project: Arc<ProjectFiles>,
    /// Checked static images of every template and `_shared`.
    pub assets: Arc<StaticAssets>,
    pub manifest: Manifest,
    /// Dynamic image fields from the manifest, with parsed pointers and
    /// their fallback images.
    pub image_fields: Vec<ImageSpec>,
    /// Raw `schema.json`, if any, and its compiled validator.
    pub schema: Option<Value>,
    validator: Option<jsonschema::Validator>,
    /// Parsed `sample.json`, if the folder has one.
    pub sample: Option<Value>,
    /// Errors from the startup test compile, empty if it succeeded.
    pub startup_errors: Vec<Diagnostic>,
}

/// One manifest image field, ready to use.
pub struct ImageSpec {
    pub pointer: String,
    pub segments: Vec<Segment>,
    pub field: ImageField,
    pub fallback: Option<Image>,
}

impl Template {
    /// Checks `data` against `schema.json`. Each error names the field by
    /// JSON pointer.
    pub fn validate(&self, data: &Value) -> Result<(), Vec<FieldError>> {
        let Some(validator) = &self.validator else {
            return Ok(());
        };
        let errors: Vec<FieldError> = validator
            .iter_errors(data)
            .map(|e| FieldError {
                path: e.instance_path().as_str().to_owned(),
                msg: e.to_string(),
            })
            .collect();
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    /// Request body limit in bytes: the manifest's `max_body_kb`, or the
    /// service default.
    pub fn max_body_bytes(&self, default: usize) -> usize {
        self.manifest
            .max_body_kb
            .map_or(default, |kb| kb as usize * 1024)
    }

    /// Render deadline: the manifest's `timeout_ms`, or the service default.
    pub fn render_timeout(&self, default: Duration) -> Duration {
        self.manifest
            .timeout_ms
            .map_or(default, Duration::from_millis)
    }
}

/// Templates keyed by route.
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

    /// Looks a template up by route.
    pub fn get(&self, route: &str) -> Option<Arc<Template>> {
        self.templates.load().get(route).cloned()
    }

    /// All templates, sorted by route.
    pub fn list(&self) -> Vec<Arc<Template>> {
        let mut list: Vec<_> = self.templates.load().values().cloned().collect();
        list.sort_by(|a, b| a.route.cmp(&b.route));
        list
    }

    /// Atomically replaces the whole set of templates.
    pub fn swap(&self, templates: Templates) {
        self.templates.store(Arc::new(templates));
    }
}

/// Scans `dir`, loading every file into one shared snapshot, and test-compiles
/// each template (with its `sample.json`, or `{}`) so errors show at startup.
///
/// A template whose `manifest.toml` or `schema.json` is invalid is skipped
/// with an error log; so is a template whose route another template uses.
pub fn load(dir: &Path, shared: &Shared) -> Result<Templates> {
    let (project, assets) = load_project(dir)?;

    let mut names: Vec<String> = project
        .paths()
        .filter_map(|p| {
            let name = p.strip_prefix('/')?.strip_suffix("/main.typ")?;
            (!name.contains('/') && !name.starts_with('_')).then(|| name.to_owned())
        })
        .collect();
    names.sort();

    let mut templates: Templates = HashMap::new();
    for name in names {
        let template = match load_template(&name, &project, &assets, shared) {
            Ok(t) => t,
            Err(err) => {
                tracing::error!(template = %name, error = %err, "skipping template");
                continue;
            }
        };
        if let Some(other) = templates.get(&template.route) {
            tracing::error!(
                template = %name,
                route = %template.route,
                other = %other.name,
                "skipping template: route already used"
            );
            continue;
        }
        templates.insert(template.route.clone(), Arc::new(template));
    }
    Ok(templates)
}

/// Loads one template folder `name` under `dir` (the dev CLI).
pub fn load_one(dir: &Path, name: &str, shared: &Shared) -> Result<Template> {
    let (project, assets) = load_project(dir)?;
    if !project.contains(&format!("/{name}/main.typ")) {
        anyhow::bail!("{name}/main.typ not found in {}", dir.display());
    }
    load_template(name, &project, &assets, shared).map_err(anyhow::Error::msg)
}

/// Snapshots `dir` and checks its static assets.
fn load_project(dir: &Path) -> Result<(Arc<ProjectFiles>, Arc<StaticAssets>)> {
    let mut project = ProjectFiles::load(dir)
        .with_context(|| format!("cannot read templates dir {}", dir.display()))?;
    let (assets, _rejected) = StaticAssets::load(&mut project);
    Ok((Arc::new(project), Arc::new(assets)))
}

fn load_template(
    name: &str,
    project: &Arc<ProjectFiles>,
    assets: &Arc<StaticAssets>,
    shared: &Shared,
) -> Result<Template, String> {
    let read_json = |file: &str| -> Result<Option<Value>, String> {
        project
            .get(&format!("/{name}/{file}"))
            .map(|bytes| {
                serde_json::from_slice(bytes).map_err(|e| format!("{file} is not valid JSON: {e}"))
            })
            .transpose()
    };

    let manifest = match project.get(&format!("/{name}/manifest.toml")) {
        None => Manifest::default(),
        Some(bytes) => {
            let text = bytes
                .as_str()
                .map_err(|_| "manifest.toml is not UTF-8".to_owned())?;
            Manifest::parse(text).map_err(|e| format!("manifest.toml: {e}"))?
        }
    };
    let route = manifest.route.clone().unwrap_or_else(|| name.to_owned());
    if !crate::manifest::valid_route(&route) {
        return Err(format!("folder name {route:?} is not a valid route"));
    }

    let mut image_fields = Vec::new();
    for (pointer, field) in &manifest.images {
        let fallback = match &field.fallback {
            None => None,
            Some(path) => {
                let vpath = resolve_file_path(name, path)
                    .map_err(|e| format!("image field {pointer:?}: fallback {path:?}: {e}"))?;
                let image = assets.get(&vpath).ok_or_else(|| {
                    format!("image field {pointer:?}: fallback {path:?} is not a valid image asset")
                })?;
                Some(image.clone())
            }
        };
        image_fields.push(ImageSpec {
            pointer: pointer.clone(),
            segments: parse_pointer(pointer)?,
            field: field.clone(),
            fallback,
        });
    }

    let schema = read_json("schema.json")?;
    let validator = schema
        .as_ref()
        .map(|s| jsonschema::validator_for(s).map_err(|e| format!("schema.json: {e}")))
        .transpose()?;

    let sample = read_json("sample.json").unwrap_or_else(|err| {
        tracing::warn!(template = %name, %err, "ignoring sample.json");
        None
    });

    let mut template = Template {
        name: name.to_owned(),
        title: manifest.name.clone().unwrap_or_else(|| name.to_owned()),
        route,
        main: format!("/{name}/main.typ"),
        project: project.clone(),
        assets: assets.clone(),
        manifest,
        image_fields,
        schema,
        validator,
        sample,
        startup_errors: Vec::new(),
    };
    template.startup_errors = startup_check(&template, shared);
    if template.startup_errors.is_empty() {
        tracing::info!(template = %name, route = %template.route, "template loaded");
    } else {
        tracing::error!(template = %name, errors = ?template.startup_errors, "template failed its startup check");
    }
    Ok(template)
}

/// Validates the sample against the schema, resolves its images without
/// network access and compiles it. Returns every problem found.
fn startup_check(template: &Template, shared: &Shared) -> Vec<Diagnostic> {
    let as_diag = |message: String| Diagnostic {
        message,
        file: None,
        line: None,
        column: None,
        hint: None,
    };
    let mut data = template
        .sample
        .clone()
        .unwrap_or_else(|| Value::Object(Default::default()));
    if template.sample.is_some()
        && let Err(errors) = template.validate(&data)
    {
        return errors
            .into_iter()
            .map(|e| as_diag(format!("sample.json {}: {}", e.path, e.msg)))
            .collect();
    }
    render::normalize_nfc(&mut data);
    let resolved = match images::resolve_offline(template, &mut data) {
        Ok(r) => r,
        Err(errors) => {
            return errors
                .into_iter()
                .map(|e| as_diag(format!("sample.json image {}: {}", e.path, e.msg)))
                .collect();
        }
    };
    match render::compile(
        shared,
        &RenderInput {
            project: &template.project,
            main: &template.main,
            data: &data,
            images: &resolved.images,
            now: Utc::now(),
        },
    ) {
        Ok(_) => Vec::new(),
        Err(err) => err.diagnostics().to_vec(),
    }
}
