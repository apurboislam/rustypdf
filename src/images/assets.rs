//! Static template and `_shared` assets, preloaded in memory.
//!
//! Every file under `<template>/assets/` and `_shared/assets/` is already in
//! the template-root snapshot ([`ProjectFiles`]). This module checks each one
//! by magic bytes once at load time, drops files that are not images, and
//! keeps a content hash so request-time lookups cost nothing.

use std::collections::HashMap;
use std::fmt;

use sha2::{Digest, Sha256};
use typst::foundations::Bytes;

use crate::world::ProjectFiles;

/// Image formats Typst can embed in a PDF/A document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
    Png,
    Jpeg,
    Gif,
    Webp,
    Svg,
}

impl ImageFormat {
    /// Detects the format from the file's first bytes, never from its name or
    /// a `Content-Type` header.
    pub fn sniff(data: &[u8]) -> Option<Self> {
        if data.starts_with(b"\x89PNG\r\n\x1a\n") {
            Some(Self::Png)
        } else if data.starts_with(b"\xff\xd8\xff") {
            Some(Self::Jpeg)
        } else if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
            Some(Self::Gif)
        } else if data.len() >= 12 && &data[..4] == b"RIFF" && &data[8..12] == b"WEBP" {
            Some(Self::Webp)
        } else if looks_like_svg(data) {
            Some(Self::Svg)
        } else {
            None
        }
    }

    pub fn ext(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
            Self::Gif => "gif",
            Self::Webp => "webp",
            Self::Svg => "svg",
        }
    }
}

impl fmt::Display for ImageFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.ext())
    }
}

/// An SVG starts (after an optional BOM, whitespace, XML declaration,
/// comments and doctype) with an `<svg` element. Only the first 4 KiB are
/// inspected.
fn looks_like_svg(data: &[u8]) -> bool {
    let head = &data[..data.len().min(4096)];
    let Ok(text) = std::str::from_utf8(head).or_else(|e| {
        // The 4 KiB cut may split a character.
        std::str::from_utf8(&head[..e.valid_up_to()])
    }) else {
        return false;
    };
    let mut rest = text.strip_prefix('\u{feff}').unwrap_or(text);
    loop {
        rest = rest.trim_start();
        if rest.starts_with("<svg") {
            return true;
        }
        let close = if rest.starts_with("<?") {
            "?>"
        } else if rest.starts_with("<!--") {
            "-->"
        } else if rest.starts_with("<!") {
            ">"
        } else {
            return false;
        };
        match rest.find(close) {
            Some(i) => rest = &rest[i + close.len()..],
            None => return false,
        }
    }
}

/// A checked image: its bytes, format and content hash.
#[derive(Debug, Clone)]
pub struct Image {
    pub bytes: Bytes,
    pub format: ImageFormat,
    /// Hex SHA-256 of `bytes`.
    pub sha256: String,
}

impl Image {
    /// Checks `bytes` by magic bytes and hashes them.
    pub fn new(bytes: Bytes) -> Option<Self> {
        let format = ImageFormat::sniff(&bytes)?;
        Some(Self {
            sha256: hex::encode(Sha256::digest(&bytes)),
            bytes,
            format,
        })
    }

    /// The request-scoped virtual path, `/__img/<sha256>.<ext>`.
    pub fn virtual_path(&self) -> String {
        format!("{}{}.{}", crate::world::IMAGE_DIR, self.sha256, self.format)
    }
}

/// Every valid image under an `assets/` folder of a template or `_shared`,
/// keyed by virtual path (e.g. `/invoice/assets/logo.png`).
#[derive(Debug, Default)]
pub struct StaticAssets {
    images: HashMap<String, Image>,
}

impl StaticAssets {
    /// Checks every asset in `project`. Files that are not a supported image
    /// are logged, removed from `project` (so templates cannot embed them)
    /// and returned as `(path, reason)` pairs.
    pub fn load(project: &mut ProjectFiles) -> (Self, Vec<(String, String)>) {
        let paths: Vec<String> = project
            .paths()
            .filter(|p| is_asset_path(p))
            .map(str::to_owned)
            .collect();
        let mut images = HashMap::new();
        let mut rejected = Vec::new();
        for path in paths {
            let bytes = project.get(&path).expect("path came from the snapshot");
            match Image::new(bytes.clone()) {
                Some(image) => {
                    images.insert(path, image);
                }
                None => {
                    tracing::error!(%path, "rejecting asset: not a PNG, JPEG, GIF, WebP or SVG image");
                    project.remove(&path);
                    rejected.push((path, "not a PNG, JPEG, GIF, WebP or SVG image".into()));
                }
            }
        }
        (Self { images }, rejected)
    }

    pub fn get(&self, vpath: &str) -> Option<&Image> {
        self.images.get(vpath)
    }

    pub fn len(&self) -> usize {
        self.images.len()
    }

    pub fn is_empty(&self) -> bool {
        self.images.is_empty()
    }
}

/// `/<folder>/assets/...`: a file in the assets folder of a template or of
/// `_shared`.
fn is_asset_path(vpath: &str) -> bool {
    let mut parts = vpath.trim_start_matches('/').split('/');
    matches!((parts.next(), parts.next(), parts.next()), (Some(_), Some("assets"), Some(f)) if !f.is_empty())
}

/// Resolves an image file path from a request (or a manifest `fallback`) to
/// the virtual path of a static asset, for template `template`.
///
/// - `assets/x.png` is relative to the template folder;
/// - `/_shared/assets/x.png` and `/<template>/assets/x.png` are absolute.
///
/// `.`/`..` segments, backslashes, empty segments and paths into other
/// templates are rejected, and the result must be inside an `assets/` folder.
pub fn resolve_file_path(template: &str, value: &str) -> Result<String, String> {
    if value.contains('\\') || value.contains('\0') {
        return Err("file path must not contain backslashes".into());
    }
    let (absolute, rest) = match value.strip_prefix('/') {
        Some(rest) => (true, rest),
        None => (false, value),
    };
    if rest
        .split('/')
        .any(|s| s.is_empty() || s == "." || s == "..")
    {
        return Err("file path must not contain empty, `.` or `..` segments".into());
    }
    let vpath = if absolute {
        let first = rest.split('/').next().unwrap_or_default();
        if first != "_shared" && first != template {
            return Err(
                "absolute file paths must start with /_shared/ or this template's folder".into(),
            );
        }
        format!("/{rest}")
    } else {
        format!("/{template}/{rest}")
    };
    if !is_asset_path(&vpath) {
        return Err("file path must point into an assets/ folder".into());
    }
    Ok(vpath)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniffs_formats() {
        assert_eq!(
            ImageFormat::sniff(b"\x89PNG\r\n\x1a\nrest"),
            Some(ImageFormat::Png)
        );
        assert_eq!(
            ImageFormat::sniff(b"\xff\xd8\xff\xe0"),
            Some(ImageFormat::Jpeg)
        );
        assert_eq!(ImageFormat::sniff(b"GIF89a.."), Some(ImageFormat::Gif));
        assert_eq!(
            ImageFormat::sniff(b"RIFF\0\0\0\0WEBPVP8 "),
            Some(ImageFormat::Webp)
        );
        assert_eq!(
            ImageFormat::sniff(b"<svg xmlns=''/>"),
            Some(ImageFormat::Svg)
        );
        assert_eq!(
            ImageFormat::sniff(
                b"\xef\xbb\xbf<?xml version='1.0'?>\n<!-- hi -->\n<!DOCTYPE svg>\n<svg/>"
            ),
            Some(ImageFormat::Svg)
        );
        assert_eq!(ImageFormat::sniff(b"<html><svg/></html>"), None);
        assert_eq!(ImageFormat::sniff(b"%PDF-1.7"), None);
        assert_eq!(ImageFormat::sniff(b""), None);
    }

    #[test]
    fn asset_paths() {
        assert!(is_asset_path("/t/assets/logo.png"));
        assert!(is_asset_path("/_shared/assets/sign/a.png"));
        assert!(!is_asset_path("/t/main.typ"));
        assert!(!is_asset_path("/t/assets/"));
        assert!(!is_asset_path("/t/sub/assets/x.png"));
    }

    #[test]
    fn file_paths_are_sandboxed() {
        assert_eq!(
            resolve_file_path("t", "assets/a.png").unwrap(),
            "/t/assets/a.png"
        );
        assert_eq!(
            resolve_file_path("t", "/_shared/assets/a.png").unwrap(),
            "/_shared/assets/a.png"
        );
        assert_eq!(
            resolve_file_path("t", "/t/assets/a.png").unwrap(),
            "/t/assets/a.png"
        );
        for bad in [
            "../other/assets/a.png",
            "assets/../main.typ",
            "assets/./a.png",
            "/other/assets/a.png",
            "/etc/passwd",
            "main.typ",
            "assets//a.png",
            "assets\\a.png",
            "",
        ] {
            assert!(resolve_file_path("t", bad).is_err(), "{bad:?}");
        }
    }
}
