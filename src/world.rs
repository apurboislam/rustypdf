//! `RenderWorld`: the `typst::World` that one render sees.
//!
//! Shared, long-lived parts (library, fonts, the template-root file cache) are
//! borrowed; only `/data.json` and `/__img/*` are new per request.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use chrono::{DateTime, Datelike, Duration, Utc};
use typst::diag::{FileError, FileResult, PackageError};
use typst::foundations::{Bytes, Datetime};
use typst::syntax::{FileId, RootedPath, Source, VirtualPath, VirtualRoot};
use typst::text::{Font, FontBook};
use typst::utils::LazyHash;
use typst::{Library, World};

use crate::fonts::FontStore;

/// Virtual path of the request JSON.
pub const DATA_PATH: &str = "/data.json";
/// Virtual directory of the request's resolved images.
pub const IMAGE_DIR: &str = "/__img/";

/// Files larger than this are not loaded into the template cache.
const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;

/// An in-memory snapshot of every file under the Typst project root
/// (`TEMPLATES_DIR`). Nothing outside the root can be read, because nothing
/// outside it is ever loaded.
pub struct ProjectFiles {
    root: PathBuf,
    files: HashMap<String, CachedFile>,
}

struct CachedFile {
    bytes: Bytes,
    source: OnceLock<FileResult<Source>>,
}

impl ProjectFiles {
    /// Loads every regular file under `root`, skipping hidden entries.
    pub fn load(root: &Path) -> std::io::Result<Self> {
        let mut files = HashMap::new();
        load_dir(root, root, &mut files)?;
        Ok(Self {
            root: root.to_path_buf(),
            files,
        })
    }

    /// Builds a snapshot from in-memory `(virtual path, content)` pairs.
    pub fn from_memory<'a>(entries: impl IntoIterator<Item = (&'a str, Vec<u8>)>) -> Self {
        let files = entries
            .into_iter()
            .filter_map(|(path, data)| {
                let vpath = VirtualPath::new(path).ok()?;
                Some((vpath.into_with_slash().into(), CachedFile::new(data)))
            })
            .collect();
        Self {
            root: PathBuf::new(),
            files,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Whether a file exists at the virtual path (e.g. `/hello/main.typ`).
    pub fn contains(&self, vpath: &str) -> bool {
        self.files.contains_key(vpath)
    }

    pub fn get(&self, vpath: &str) -> Option<&Bytes> {
        self.files.get(vpath).map(|f| &f.bytes)
    }

    /// Virtual paths of all cached files.
    pub fn paths(&self) -> impl Iterator<Item = &str> {
        self.files.keys().map(String::as_str)
    }

    fn source(&self, id: FileId, vpath: &str) -> FileResult<Source> {
        let file = self.files.get(vpath).ok_or_else(|| not_found(vpath))?;
        file.source
            .get_or_init(|| {
                let text = file.bytes.as_str().map_err(|_| FileError::InvalidUtf8)?;
                // Strip a BOM, as the Typst CLI does.
                let text = text.strip_prefix('\u{feff}').unwrap_or(text);
                Ok(Source::new(id, text.into()))
            })
            .clone()
    }
}

impl CachedFile {
    fn new(data: Vec<u8>) -> Self {
        Self {
            bytes: Bytes::new(data),
            source: OnceLock::new(),
        }
    }
}

fn load_dir(root: &Path, dir: &Path, out: &mut HashMap<String, CachedFile>) -> std::io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        // `file_type` does not follow symlinks, so links are skipped: they
        // could point outside the root.
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            load_dir(root, &path, out)?;
        } else if file_type.is_file() {
            if entry.metadata()?.len() > MAX_FILE_BYTES {
                tracing::warn!(path = %path.display(), "skipping file over 64 MiB");
                continue;
            }
            let Ok(vpath) = VirtualPath::virtualize(root, &path) else {
                tracing::warn!(path = %path.display(), "skipping file with unsupported path");
                continue;
            };
            out.insert(
                vpath.into_with_slash().into(),
                CachedFile::new(fs::read(&path)?),
            );
        }
    }
    Ok(())
}

fn not_found(vpath: &str) -> FileError {
    FileError::NotFound(PathBuf::from(vpath))
}

/// The parts every render shares: the standard library and the fonts.
pub struct Shared {
    pub library: LazyHash<Library>,
    pub fonts: Arc<FontStore>,
}

impl Shared {
    pub fn new(fonts: Arc<FontStore>) -> Self {
        use typst::LibraryExt;
        Self {
            library: LazyHash::new(Library::default()),
            fonts,
        }
    }
}

/// The world for one render.
pub struct RenderWorld<'a> {
    shared: &'a Shared,
    project: &'a ProjectFiles,
    main: FileId,
    data: Bytes,
    images: &'a HashMap<String, Bytes>,
    now: DateTime<Utc>,
}

impl<'a> RenderWorld<'a> {
    /// `main` is the virtual path of the entry file, e.g. `/hello/main.typ`.
    /// `images` maps `/__img/...` virtual paths to their bytes.
    pub fn new(
        shared: &'a Shared,
        project: &'a ProjectFiles,
        main: &str,
        data: Bytes,
        images: &'a HashMap<String, Bytes>,
        now: DateTime<Utc>,
    ) -> FileResult<Self> {
        let vpath = VirtualPath::new(main).map_err(|_| not_found(main))?;
        if !project.contains(vpath.get_with_slash()) {
            return Err(not_found(main));
        }
        Ok(Self {
            shared,
            project,
            main: RootedPath::new(VirtualRoot::Project, vpath).intern(),
            data,
            images,
            now,
        })
    }

    fn lookup(&self, id: FileId) -> FileResult<&'static str> {
        let path = id.get();
        match path.root() {
            VirtualRoot::Project => Ok(path.vpath().get_with_slash()),
            // Packages are not available yet (PLAN.md §8, `/packages`).
            VirtualRoot::Package(spec) => {
                Err(FileError::Package(PackageError::NotFound(spec.clone())))
            }
        }
    }
}

impl World for RenderWorld<'_> {
    fn library(&self) -> &LazyHash<Library> {
        &self.shared.library
    }

    fn book(&self) -> &LazyHash<FontBook> {
        self.shared.fonts.book()
    }

    fn main(&self) -> FileId {
        self.main
    }

    fn source(&self, id: FileId) -> FileResult<Source> {
        let vpath = self.lookup(id)?;
        if vpath == DATA_PATH {
            let text = self.data.as_str().map_err(|_| FileError::InvalidUtf8)?;
            return Ok(Source::new(id, text.into()));
        }
        if vpath.starts_with(IMAGE_DIR) {
            return Err(FileError::NotSource);
        }
        self.project.source(id, vpath)
    }

    fn file(&self, id: FileId) -> FileResult<Bytes> {
        let vpath = self.lookup(id)?;
        if vpath == DATA_PATH {
            return Ok(self.data.clone());
        }
        if vpath.starts_with(IMAGE_DIR) {
            return self
                .images
                .get(vpath)
                .cloned()
                .ok_or_else(|| not_found(vpath));
        }
        self.project
            .get(vpath)
            .cloned()
            .ok_or_else(|| not_found(vpath))
    }

    fn font(&self, index: usize) -> Option<Font> {
        self.shared.fonts.font(index)
    }

    fn today(&self, offset: Option<typst::foundations::Duration>) -> Option<Datetime> {
        let date = match offset {
            // No offset means "local"; the service runs in UTC.
            None => self.now,
            Some(offset) => self.now + Duration::seconds(offset.seconds() as i64),
        }
        .date_naive();
        Datetime::from_ymd(date.year(), date.month() as u8, date.day() as u8)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_memory_normalizes_paths() {
        let files = ProjectFiles::from_memory([("a/b.typ", b"x".to_vec())]);
        assert!(files.contains("/a/b.typ"));
        assert!(!files.contains("a/b.typ"));
    }

    #[test]
    fn load_skips_hidden_files() {
        let dir = std::env::temp_dir().join(format!("rustypdf-world-{}", std::process::id()));
        fs::create_dir_all(dir.join("t")).unwrap();
        fs::write(dir.join("t/main.typ"), "hi").unwrap();
        fs::write(dir.join("t/.secret"), "no").unwrap();
        let files = ProjectFiles::load(&dir).unwrap();
        fs::remove_dir_all(&dir).unwrap();
        assert!(files.contains("/t/main.typ"));
        assert!(!files.contains("/t/.secret"));
    }
}
