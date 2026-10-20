//! Image resolution for manifest-declared image fields.
//!
//! For every value a manifest pointer matches, the value's form decides how it
//! loads (PLAN.md §6):
//! - `http(s)://...`: fetched by [`fetch::Fetcher`] (cached, resized);
//! - `data:...`: decoded inline;
//! - anything else: a file path into a template's or `_shared`'s `assets/`;
//! - missing, `null` or `""`: the field's fallback file.
//!
//! Each result is stored as `/__img/<sha256>.<ext>` and the JSON value is
//! rewritten to that path, so templates only ever see a path.

pub mod assets;
pub mod fetch;
pub mod resize;

use std::collections::HashMap;
use std::sync::Arc;

use base64::Engine;
use futures_util::StreamExt;
use serde::Serialize;
use serde_json::Value;
use typst::foundations::Bytes;

use self::assets::{Image, resolve_file_path};
use self::fetch::Fetcher;
use crate::manifest::{OnError, Segment};
use crate::registry::{ImageSpec, Template};

/// A failed image field, in a `422` body or the `X-Render-Warnings` header.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImageIssue {
    /// JSON pointer to the value, e.g. `/items/3/photo`.
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    pub msg: String,
}

/// The resolved images of one request.
#[derive(Debug, Default)]
pub struct Resolved {
    /// `/__img/...` virtual path to bytes.
    pub images: HashMap<String, Bytes>,
    /// Values that failed and were replaced by their fallback.
    pub warnings: Vec<ImageIssue>,
}

/// Per-request limits.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_per_request: usize,
    pub fetch_concurrency: usize,
    /// Size cap for `data:` URIs (URL fetches use the fetcher's own cap).
    pub max_bytes: u64,
}

/// Resolves every image field of `data` for `template`, fetching URLs in
/// parallel. Returns the rewritten data, or every field that failed under
/// `on_error = "fail"`.
pub async fn resolve(
    template: Arc<Template>,
    mut data: Value,
    fetcher: &Fetcher,
    limits: Limits,
) -> Result<(Value, Resolved), Vec<ImageIssue>> {
    if template.image_fields.is_empty() {
        return Ok((data, Resolved::default()));
    }
    let slots = collect(&template, &data);
    let count = slots
        .iter()
        .filter(|s| !matches!(s.source, Source::Empty))
        .count();
    if count > limits.max_per_request {
        return Err(vec![ImageIssue {
            path: String::new(),
            url: None,
            msg: format!(
                "request has {count} images; the limit is {} (IMAGE_MAX_PER_REQUEST)",
                limits.max_per_request
            ),
        }]);
    }

    let mut urls: Vec<(String, Option<u32>)> = slots
        .iter()
        .filter_map(|s| match &s.source {
            Source::Url(u) => Some((u.clone(), template.image_fields[s.spec].field.max_px)),
            _ => None,
        })
        .collect();
    urls.sort();
    urls.dedup();
    let fetched: HashMap<(String, Option<u32>), Result<Image, String>> =
        futures_util::stream::iter(urls)
            .map(|key| async move {
                let result = fetcher
                    .fetch(&key.0, key.1)
                    .await
                    .map_err(|e| e.to_string());
                (key, result)
            })
            .buffer_unordered(limits.fetch_concurrency.max(1))
            .collect()
            .await;

    // Decoding and resizing `data:` URIs is CPU work: keep it off the async
    // runtime.
    tokio::task::spawn_blocking(move || {
        let resolved = finish(
            &template,
            &mut data,
            slots,
            &fetched,
            limits.max_bytes,
            false,
        )?;
        Ok((data, resolved))
    })
    .await
    .map_err(|e| {
        vec![ImageIssue {
            path: String::new(),
            url: None,
            msg: format!("image task failed: {e}"),
        }]
    })?
}

/// Resolves image fields without network access, for the startup check of a
/// template's `sample.json`: URLs use the field's fallback when it has one
/// and are otherwise left as they are.
pub fn resolve_offline(template: &Template, data: &mut Value) -> Result<Resolved, Vec<ImageIssue>> {
    let slots = collect(template, data);
    finish(template, data, slots, &HashMap::new(), u64::MAX, true)
}

/// Where a matched value is and what form it has.
struct Slot {
    location: Vec<Step>,
    /// Index into `template.image_fields`.
    spec: usize,
    source: Source,
}

#[derive(Debug, Clone)]
enum Step {
    Key(String),
    Index(usize),
}

enum Source {
    Empty,
    Url(String),
    Data(String),
    File(String),
    Invalid(String),
}

fn classify(value: Option<&Value>) -> Source {
    let s = match value {
        None | Some(Value::Null) => return Source::Empty,
        Some(Value::String(s)) => s.trim(),
        Some(_) => return Source::Invalid("must be a string".into()),
    };
    let lower = s.get(..8).unwrap_or(s).to_ascii_lowercase();
    if s.is_empty() {
        Source::Empty
    } else if lower.starts_with("http://") || lower.starts_with("https://") {
        Source::Url(s.to_owned())
    } else if lower.starts_with("data:") {
        Source::Data(s.to_owned())
    } else if s.contains("://") {
        Source::Invalid("only http and https URLs are allowed".into())
    } else {
        Source::File(s.to_owned())
    }
}

fn collect(template: &Template, data: &Value) -> Vec<Slot> {
    let mut slots = Vec::new();
    for (spec, field) in template.image_fields.iter().enumerate() {
        walk(data, &field.segments, &mut Vec::new(), spec, &mut slots);
    }
    slots
}

fn walk(
    value: &Value,
    segments: &[Segment],
    location: &mut Vec<Step>,
    spec: usize,
    out: &mut Vec<Slot>,
) {
    let Some((first, rest)) = segments.split_first() else {
        out.push(Slot {
            location: location.clone(),
            spec,
            source: classify(Some(value)),
        });
        return;
    };
    match first {
        Segment::Key(key) => {
            let Value::Object(map) = value else { return };
            location.push(Step::Key(key.clone()));
            match map.get(key) {
                Some(child) => walk(child, rest, location, spec, out),
                // A missing last key is an empty value; a missing parent
                // means there is nothing to resolve.
                None if rest.is_empty() => out.push(Slot {
                    location: location.clone(),
                    spec,
                    source: Source::Empty,
                }),
                None => {}
            }
            location.pop();
        }
        Segment::Each => {
            let Value::Array(items) = value else { return };
            for (i, item) in items.iter().enumerate() {
                location.push(Step::Index(i));
                walk(item, rest, location, spec, out);
                location.pop();
            }
        }
    }
}

fn pointer(location: &[Step]) -> String {
    location
        .iter()
        .map(|step| match step {
            Step::Key(k) => format!("/{}", k.replace('~', "~0").replace('/', "~1")),
            Step::Index(i) => format!("/{i}"),
        })
        .collect()
}

/// Replaces the value at `location`, creating a missing last key.
fn set(data: &mut Value, location: &[Step], new: Value) {
    let mut current = data;
    for step in location {
        current = match (step, current) {
            (Step::Key(k), Value::Object(map)) => map.entry(k.clone()).or_insert(Value::Null),
            (Step::Index(i), Value::Array(items)) => match items.get_mut(*i) {
                Some(item) => item,
                None => return,
            },
            _ => return,
        };
    }
    *current = new;
}

enum Outcome {
    Use(Image),
    Leave,
    Failed(String),
}

fn finish(
    template: &Template,
    data: &mut Value,
    slots: Vec<Slot>,
    fetched: &HashMap<(String, Option<u32>), Result<Image, String>>,
    max_bytes: u64,
    offline: bool,
) -> Result<Resolved, Vec<ImageIssue>> {
    let mut resolved = Resolved::default();
    let mut errors = Vec::new();
    for slot in slots {
        let spec: &ImageSpec = &template.image_fields[slot.spec];
        let field = &spec.field;
        let url = match &slot.source {
            Source::Url(u) => Some(u.clone()),
            _ => None,
        };
        let outcome = match slot.source {
            Source::Empty => match &spec.fallback {
                Some(image) => Outcome::Use(image.clone()),
                None if field.optional => Outcome::Leave,
                None => Outcome::Failed("missing; this image field is required".into()),
            },
            Source::File(path) => match resolve_file_path(&template.name, &path) {
                Ok(vpath) => match template.assets.get(&vpath) {
                    Some(image) => Outcome::Use(image.clone()),
                    None => Outcome::Failed(format!("no image asset at {path:?}")),
                },
                Err(msg) => Outcome::Failed(msg),
            },
            Source::Data(uri) => match decode_data_uri(&uri, max_bytes) {
                Ok(image) => match field.max_px {
                    Some(max_px) => match resize::downscale(image, max_px) {
                        Ok(image) => Outcome::Use(image),
                        Err(msg) => Outcome::Failed(msg),
                    },
                    None => Outcome::Use(image),
                },
                Err(msg) => Outcome::Failed(msg),
            },
            Source::Url(_) if offline => match &spec.fallback {
                Some(image) => Outcome::Use(image.clone()),
                None => Outcome::Leave,
            },
            Source::Url(url) => match fetched.get(&(url, field.max_px)) {
                Some(Ok(image)) => Outcome::Use(image.clone()),
                Some(Err(msg)) => Outcome::Failed(msg.clone()),
                None => Outcome::Failed("not fetched".into()),
            },
            Source::Invalid(msg) => Outcome::Failed(msg),
        };
        let image = match outcome {
            Outcome::Use(image) => image,
            Outcome::Leave => continue,
            Outcome::Failed(msg) => {
                let issue = ImageIssue {
                    path: pointer(&slot.location),
                    url: url.clone(),
                    msg,
                };
                match (field.on_error(), &spec.fallback) {
                    (OnError::Fallback, Some(fallback)) => {
                        resolved.warnings.push(issue);
                        fallback.clone()
                    }
                    _ => {
                        errors.push(issue);
                        continue;
                    }
                }
            }
        };
        let vpath = image.virtual_path();
        set(data, &slot.location, Value::String(vpath.clone()));
        resolved.images.entry(vpath).or_insert(image.bytes);
    }
    if errors.is_empty() {
        Ok(resolved)
    } else {
        Err(errors)
    }
}

/// Decodes `data:[<media type>][;base64],<payload>`. The media type is
/// ignored; the format comes from the magic bytes.
fn decode_data_uri(uri: &str, max_bytes: u64) -> Result<Image, String> {
    let rest = &uri["data:".len()..];
    let (meta, payload) = rest
        .split_once(',')
        .ok_or("data: URI has no ',' before its payload")?;
    let is_base64 = meta.to_ascii_lowercase().ends_with(";base64");
    // Base64 decodes to 3/4 of its length; percent-encoding only shrinks.
    let estimate = if is_base64 {
        payload.len() / 4 * 3
    } else {
        payload.len()
    };
    if estimate as u64 > max_bytes {
        return Err(format!("image exceeds {max_bytes} bytes"));
    }
    let bytes = if is_base64 {
        let cleaned: Vec<u8> = payload
            .bytes()
            .filter(|b| !b.is_ascii_whitespace())
            .collect();
        base64::engine::general_purpose::STANDARD
            .decode(&cleaned)
            .map_err(|e| format!("invalid base64 in data: URI: {e}"))?
    } else {
        percent_decode(payload)?
    };
    if bytes.len() as u64 > max_bytes {
        return Err(format!("image exceeds {max_bytes} bytes"));
    }
    Image::new(Bytes::new(bytes)).ok_or_else(|| "not a PNG, JPEG, GIF, WebP or SVG image".into())
}

fn percent_decode(s: &str) -> Result<Vec<u8>, String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = bytes
                .get(i + 1..i + 3)
                .and_then(|h| std::str::from_utf8(h).ok())
                .and_then(|h| u8::from_str_radix(h, 16).ok())
                .ok_or("invalid percent-encoding in data: URI")?;
            out.push(hex);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    Ok(out)
}

/// Formats issues for the `X-Render-Warnings` header: a JSON array with
/// non-ASCII characters escaped, cut to about `max_len` bytes.
pub fn warnings_header(issues: &[ImageIssue], max_len: usize) -> String {
    let mut shown = issues.len();
    loop {
        let mut json = serde_json::to_string(&issues[..shown]).expect("issues serialize");
        if shown < issues.len() {
            json.pop();
            if shown > 0 {
                json.push(',');
            }
            json.push_str(&format!("{{\"more\":{}}}]", issues.len() - shown));
        }
        let header = ascii_escape(&json);
        if header.len() <= max_len || shown == 0 {
            return header;
        }
        shown -= 1;
    }
}

/// Escapes characters outside printable ASCII as JSON `\uXXXX` sequences.
/// Only used on serialized JSON, where such characters appear inside strings.
fn ascii_escape(json: &str) -> String {
    let mut out = String::with_capacity(json.len());
    for c in json.chars() {
        if (' '..='~').contains(&c) {
            out.push(c);
        } else {
            let mut buf = [0u16; 2];
            for unit in c.encode_utf16(&mut buf) {
                out.push_str(&format!("\\u{unit:04x}"));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn classifies_values() {
        let c = |v: Value| match classify(Some(&v)) {
            Source::Empty => "empty",
            Source::Url(_) => "url",
            Source::Data(_) => "data",
            Source::File(_) => "file",
            Source::Invalid(_) => "invalid",
        };
        assert_eq!(c(json!(null)), "empty");
        assert_eq!(c(json!("  ")), "empty");
        assert_eq!(c(json!("https://x/a.png")), "url");
        assert_eq!(c(json!("HTTP://x/a.png")), "url");
        assert_eq!(c(json!("data:image/png;base64,AA==")), "data");
        assert_eq!(c(json!("assets/a.png")), "file");
        assert_eq!(c(json!("file:///etc/passwd")), "invalid");
        assert_eq!(c(json!(3)), "invalid");
        assert!(matches!(classify(None), Source::Empty));
    }

    #[test]
    fn set_and_pointer() {
        let mut v = json!({ "items": [{ "a/b": 1 }, {}] });
        let loc = vec![
            Step::Key("items".into()),
            Step::Index(1),
            Step::Key("a/b".into()),
        ];
        set(&mut v, &loc, json!("x"));
        assert_eq!(v, json!({ "items": [{ "a/b": 1 }, { "a/b": "x" }] }));
        assert_eq!(pointer(&loc), "/items/1/a~1b");
    }

    #[test]
    fn data_uris() {
        let png = b"\x89PNG\r\n\x1a\nxyz";
        let b64 = base64::engine::general_purpose::STANDARD.encode(png);
        let image = decode_data_uri(&format!("data:image/png;base64,{b64}"), 100).unwrap();
        assert_eq!(image.bytes.as_slice(), png);
        assert!(decode_data_uri(&format!("data:image/png;base64,{b64}"), 4).is_err());
        let svg = decode_data_uri("data:image/svg+xml,%3Csvg%20xmlns='x'/%3E", 100).unwrap();
        assert_eq!(svg.format, assets::ImageFormat::Svg);
        assert!(decode_data_uri("data:text/plain,hello", 100).is_err());
        assert!(decode_data_uri("data:image/png;base64", 100).is_err());
    }

    #[test]
    fn warnings_header_is_ascii_and_bounded() {
        let issue = |i: usize| ImageIssue {
            path: format!("/items/{i}/ছবি"),
            url: Some("https://x/a.png".into()),
            msg: "HTTP 404".into(),
        };
        let one = warnings_header(&[issue(0)], 4096);
        assert!(one.is_ascii());
        assert!(one.contains("\\u099b"));
        let many: Vec<_> = (0..100).map(issue).collect();
        let h = warnings_header(&many, 300);
        assert!(h.len() <= 300, "{h}");
        assert!(h.contains("\"more\":"));
        let parsed: Value = serde_json::from_str(&h).unwrap();
        assert!(parsed.is_array());
    }
}
