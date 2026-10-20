//! `manifest.toml`: optional per-template metadata, limits and the dynamic
//! image fields (PLAN.md §3).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// Display name; defaults to the folder name.
    pub name: Option<String>,
    /// Route segment for `POST /render/{route}`; defaults to the folder name.
    pub route: Option<String>,
    /// Overrides `MAX_BODY_KB` for this template.
    pub max_body_kb: Option<u64>,
    /// Overrides `RENDER_TIMEOUT_MS` for this template.
    pub timeout_ms: Option<u64>,
    /// Dynamic image fields, keyed by JSON pointer (`[]` = every array item).
    #[serde(default)]
    pub images: BTreeMap<String, ImageField>,
}

#[derive(Debug, Default, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ImageField {
    /// Downscale so the longest side is at most this many pixels.
    pub max_px: Option<u32>,
    /// File used when the value is missing, empty or (with
    /// `on_error = "fallback"`) fails to load.
    pub fallback: Option<String>,
    /// The field may be missing or empty; it is then left as is.
    #[serde(default)]
    pub optional: bool,
    /// What a failing value does. Defaults to `fallback` when a fallback file
    /// is set, else `fail`.
    pub on_error: Option<OnError>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum OnError {
    /// Use the fallback file and add an `X-Render-Warnings` entry.
    Fallback,
    /// Reject the request with a 422.
    Fail,
}

impl ImageField {
    pub fn on_error(&self) -> OnError {
        match (self.on_error, &self.fallback) {
            (Some(policy), _) => policy,
            (None, Some(_)) => OnError::Fallback,
            (None, None) => OnError::Fail,
        }
    }
}

/// One segment of an image-field pointer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    Key(String),
    /// `[]`: every item of an array.
    Each,
}

/// Parses a manifest pointer such as `/items/[]/photo` (RFC 6901 escapes
/// `~0` and `~1` apply to keys).
pub fn parse_pointer(pointer: &str) -> Result<Vec<Segment>, String> {
    let Some(rest) = pointer.strip_prefix('/') else {
        return Err(format!("image pointer {pointer:?} must start with '/'"));
    };
    rest.split('/')
        .map(|s| match s {
            "[]" => Ok(Segment::Each),
            "" => Err(format!("image pointer {pointer:?} has an empty segment")),
            s => Ok(Segment::Key(s.replace("~1", "/").replace("~0", "~"))),
        })
        .collect()
}

impl Manifest {
    pub fn parse(text: &str) -> Result<Self, String> {
        let manifest: Self = toml::from_str(text).map_err(|e| e.to_string())?;
        if let Some(route) = &manifest.route
            && !valid_route(route)
        {
            return Err(format!(
                "route {route:?} may only contain letters, digits, '-' and '_', and must not start with '_'"
            ));
        }
        for (pointer, field) in &manifest.images {
            parse_pointer(pointer)?;
            if field.on_error() == OnError::Fallback && field.fallback.is_none() {
                return Err(format!(
                    "image field {pointer:?} has on_error = \"fallback\" but no fallback file"
                ));
            }
            if field.max_px == Some(0) {
                return Err(format!("image field {pointer:?} has max_px = 0"));
            }
        }
        Ok(manifest)
    }
}

pub fn valid_route(route: &str) -> bool {
    !route.is_empty()
        && !route.starts_with('_')
        && route
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plan_example() {
        let m = Manifest::parse(
            r#"
            name = "Retail invoice"
            route = "invoice-retail"
            max_body_kb = 512
            timeout_ms = 5000

            [images."/items/[]/photo"]
            max_px   = 400
            fallback = "/_shared/assets/no-photo.png"
            on_error = "fallback"

            [images."/signature"]
            optional = true
            on_error = "fail"
            "#,
        )
        .unwrap();
        assert_eq!(m.route.as_deref(), Some("invoice-retail"));
        assert_eq!(m.max_body_kb, Some(512));
        let photo = &m.images["/items/[]/photo"];
        assert_eq!(photo.max_px, Some(400));
        assert_eq!(photo.on_error(), OnError::Fallback);
        let sig = &m.images["/signature"];
        assert!(sig.optional);
        assert_eq!(sig.on_error(), OnError::Fail);
    }

    #[test]
    fn on_error_defaults_follow_fallback() {
        let m = Manifest::parse("[images.\"/a\"]\nfallback = \"assets/x.png\"\n[images.\"/b\"]\n")
            .unwrap();
        assert_eq!(m.images["/a"].on_error(), OnError::Fallback);
        assert_eq!(m.images["/b"].on_error(), OnError::Fail);
    }

    #[test]
    fn rejects_bad_manifests() {
        for bad in [
            "route = \"a/b\"",
            "route = \"_x\"",
            "unknown = 1",
            "[images.\"items\"]",
            "[images.\"/a//b\"]",
            "[images.\"/a\"]\non_error = \"fallback\"",
            "[images.\"/a\"]\non_error = \"ignore\"",
            "[images.\"/a\"]\nmax_px = 0",
        ] {
            assert!(Manifest::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn pointers() {
        assert_eq!(
            parse_pointer("/items/[]/photo").unwrap(),
            [
                Segment::Key("items".into()),
                Segment::Each,
                Segment::Key("photo".into())
            ]
        );
        assert_eq!(
            parse_pointer("/a~1b/c~0d").unwrap(),
            [Segment::Key("a/b".into()), Segment::Key("c~d".into())]
        );
    }
}
