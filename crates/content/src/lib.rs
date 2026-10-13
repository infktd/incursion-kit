//! File-backed site content: UI translations (locales), guides and guide images.
//!
//! Everything lives under the data directory (`config.data.dir`), with the same layout as the
//! legacy waitlist, so an existing `data/` directory works unchanged:
//!
//! ```text
//! <data>/locales/<page>.<locale>.json        flat { key: string } translation maps
//! <data>/locales/languages.json              { code: display label }
//! <data>/guides/<slug>/<slug>.<locale>.json  guides (markdown `body` + metadata), one per locale
//! <data>/guides/<slug>/<image>               guide images
//! <data>/locales/<slug>.<locale>.json        guides from before guides had their own directory
//! ```
//!
//! Content is read from disk on every request (nothing is cached), so edits made by hand show up
//! immediately. Writes are validated **before** anything touches the disk, then written to a
//! temporary file and renamed into place, after copying the previous version to
//! `<file>.backup.<unix seconds>` (the newest few backups are kept, see [`fsutil`]).
//!
//! All operations are synchronous file I/O; async callers should run them on a blocking thread.
//! Errors are [`ContentError`]s whose messages are the legacy waitlist's (they are sent to the
//! client as plain text).

mod assets;
mod fsutil;
mod guides;
mod locales;
mod names;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

pub use assets::{OptimizedImage, optimize_guide_image};
pub use guides::{GuideAssetInfo, GuideListing, asset_mime_type};
pub use locales::LANGUAGE_LABELS_FILENAME;
pub use names::{parse_admin_filename, validate_locale_code, validate_page_slug};

/// A content operation failed. The message is meant for the client (legacy wording).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ContentError(pub String);

impl ContentError {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

pub type ContentResult<T> = Result<T, ContentError>;

/// Handle to the content under a data directory. Cheap to clone.
#[derive(Clone, Debug)]
pub struct Content {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    dir: PathBuf,
    /// Serializes writes (backup + temp file + rename) so concurrent saves cannot interleave.
    write_lock: Mutex<()>,
}

impl Content {
    /// Content stored under `dir` (the data directory, not its `locales/` subdirectory).
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            inner: Arc::new(Inner {
                dir: dir.into(),
                write_lock: Mutex::new(()),
            }),
        }
    }

    /// The data directory.
    pub fn dir(&self) -> &Path {
        &self.inner.dir
    }

    fn locale_dir(&self) -> PathBuf {
        self.inner.dir.join("locales")
    }

    fn guide_root(&self) -> PathBuf {
        self.inner.dir.join("guides")
    }

    fn write_guard(&self) -> MutexGuard<'_, ()> {
        // The guarded data is `()`: a panic while holding it cannot leave anything inconsistent.
        self.inner
            .write_lock
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }
}
