//! Fonts are scanned once at startup and shared by every render.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use typst::foundations::Bytes;
use typst::text::{Font, FontBook, FontInfo};
use typst::utils::LazyHash;

#[derive(Debug, thiserror::Error)]
pub enum FontError {
    #[error("cannot read fonts directory {0}: {1}")]
    Dir(PathBuf, std::io::Error),
    #[error("required fonts missing from {dir}: {missing:?} (found: {found:?})")]
    Missing {
        dir: PathBuf,
        missing: Vec<String>,
        found: Vec<String>,
    },
}

/// Every font face found under `FONTS_DIR`. Faces are indexed by the
/// `FontBook`, and each face is parsed on first use.
pub struct FontStore {
    book: LazyHash<FontBook>,
    slots: Vec<FontSlot>,
}

struct FontSlot {
    data: Bytes,
    index: u32,
    font: OnceLock<Option<Font>>,
}

impl FontStore {
    /// Loads every `.ttf`/`.otf`/`.ttc`/`.otc` file under `dir` (recursively)
    /// and checks that every family in `required` is present.
    pub fn load(dir: &Path, required: &[String]) -> Result<Self, FontError> {
        let mut files = Vec::new();
        collect_font_files(dir, &mut files).map_err(|e| FontError::Dir(dir.into(), e))?;
        files.sort();

        let mut book = FontBook::new();
        let mut slots = Vec::new();
        for path in files {
            let data = match fs::read(&path) {
                Ok(data) => Bytes::new(data),
                Err(err) => {
                    tracing::warn!(path = %path.display(), %err, "skipping unreadable font");
                    continue;
                }
            };
            for (index, info) in FontInfo::iter(&data).enumerate() {
                book.push(info);
                slots.push(FontSlot {
                    data: data.clone(),
                    index: index as u32,
                    font: OnceLock::new(),
                });
            }
        }

        let missing: Vec<String> = required
            .iter()
            .filter(|family| !book.contains_family(&family.to_lowercase()))
            .cloned()
            .collect();
        if !missing.is_empty() {
            return Err(FontError::Missing {
                dir: dir.into(),
                missing,
                found: families(&book),
            });
        }

        Ok(Self {
            book: LazyHash::new(book),
            slots,
        })
    }

    pub fn book(&self) -> &LazyHash<FontBook> {
        &self.book
    }

    pub fn font(&self, index: usize) -> Option<Font> {
        let slot = self.slots.get(index)?;
        slot.font
            .get_or_init(|| Font::new(slot.data.clone(), slot.index))
            .clone()
    }

    /// Number of font faces loaded.
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Distinct family names, sorted.
    pub fn families(&self) -> Vec<String> {
        families(&self.book)
    }
}

fn families(book: &FontBook) -> Vec<String> {
    let mut names: Vec<String> = book.families().map(|(name, _)| name.to_owned()).collect();
    names.dedup();
    names
}

fn collect_font_files(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_font_files(&path, out)?;
        } else if path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
            matches!(
                e.to_ascii_lowercase().as_str(),
                "ttf" | "otf" | "ttc" | "otc"
            )
        }) {
            out.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fonts_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("fonts")
    }

    #[test]
    fn loads_bundled_fonts() {
        let store =
            FontStore::load(&fonts_dir(), &["Noto Sans Bengali".into(), "Inter".into()]).unwrap();
        let families = store.families();
        assert!(families.iter().any(|f| f == "Inter"), "{families:?}");
        assert!(
            families.iter().any(|f| f == "Noto Sans Bengali"),
            "{families:?}"
        );
        assert!(store.font(0).is_some());
    }

    #[test]
    fn missing_required_font_is_an_error() {
        let err = FontStore::load(&fonts_dir(), &["No Such Font".into()])
            .err()
            .expect("must fail");
        assert!(
            matches!(err, FontError::Missing { ref missing, .. } if missing == &["No Such Font"])
        );
    }
}
