//! The wiki: sections, pages with revisions, images and search, edited in the app.
//!
//! Pages are Markdown with directive-style components ([`markdown`]); the server stores and
//! validates them, the frontend renders them. Everything lives in the database (migration
//! `0004_wiki.sql`) except uploaded images, which are files under `<data>/wiki/assets/`.
//!
//! - [`Wiki`] is the service: reads filtered by what a [`Viewer`] may see, writes checked
//!   against the `wiki-edit` / `wiki-admin` permission keys, optimistic concurrency on page
//!   content (the revision id), soft deletes.
//! - [`search`]: an in-memory ranked index over the pages, rebuilt when the `wiki_state`
//!   generation changes.
//! - [`import`]: Markdown/MDX files with frontmatter (a static-site wiki) into the database.
//!
//! `docs/wiki.md` is the user-facing description.

pub mod assets;
pub mod diff;
pub mod import;
pub mod markdown;
pub mod mdx;
pub mod model;
pub mod search;
pub mod slug;
mod store;

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub use markdown::{Heading, Problem, analyze};
pub use model::*;
pub use search::SearchCache;
pub use store::{MAX_BODY_BYTES, MAX_SEARCH_HITS, validate};

/// Permission key: create, edit, move, publish pages; upload images.
pub const EDIT_KEY: &str = "wiki-edit";
/// Permission key: sections, deleting and restoring pages, visibility of others' pages, and
/// every page regardless of its visibility.
pub const ADMIN_KEY: &str = "wiki-admin";

/// Who is asking: an account and its permission keys, or nobody (logged out).
#[derive(Debug, Clone, Copy, Default)]
pub struct Viewer {
    pub id: Option<i64>,
    pub access: Option<&'static BTreeSet<&'static str>>,
}

impl Viewer {
    pub fn anonymous() -> Self {
        Self::default()
    }

    pub fn account(id: i64, access: &'static BTreeSet<&'static str>) -> Self {
        Self {
            id: Some(id),
            access: Some(access),
        }
    }

    pub fn has(&self, key: &str) -> bool {
        self.access.is_some_and(|a| a.contains(key))
    }

    pub fn can_edit(&self) -> bool {
        self.has(EDIT_KEY) || self.has(ADMIN_KEY)
    }

    pub fn can_admin(&self) -> bool {
        self.has(ADMIN_KEY)
    }

    /// Whether a page with this visibility and status is shown to the viewer. Drafts are for
    /// editors; `wiki-admin` sees everything.
    pub fn can_view(
        &self,
        visibility: Visibility,
        permission: Option<&str>,
        status: Status,
    ) -> bool {
        if self.can_admin() {
            return true;
        }
        let allowed = match visibility {
            Visibility::Public => true,
            Visibility::Login => self.id.is_some(),
            Visibility::Permission => permission.is_some_and(|key| self.has(key)),
        };
        allowed && (status == Status::Published || self.can_edit())
    }

    fn require_login(&self) -> WikiResult<i64> {
        self.id.ok_or_else(WikiError::unauthenticated)
    }

    fn require_edit(&self) -> WikiResult<i64> {
        let id = self.require_login()?;
        if self.can_edit() {
            Ok(id)
        } else {
            Err(WikiError::access_denied(EDIT_KEY))
        }
    }

    fn require_admin(&self) -> WikiResult<i64> {
        let id = self.require_login()?;
        if self.can_admin() {
            Ok(id)
        } else {
            Err(WikiError::access_denied(ADMIN_KEY))
        }
    }
}

/// What writes are checked against, besides the viewer's keys.
#[derive(Debug, Clone, Default)]
pub struct Checks {
    /// Doctrine fit names `::fit{name=…}` may use; `None` accepts any name.
    pub fit_names: Option<HashSet<String>>,
    /// Permission keys a page may be restricted to (every key some role has). Empty accepts any.
    pub permission_keys: BTreeSet<String>,
}

/// A change the live stream announces (`wiki_update`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct WikiChange {
    /// The page's slug, or `None` for a section change.
    pub slug: Option<String>,
    /// The section the page is in (after the change), or the changed section.
    pub section: Option<String>,
    /// `created`, `updated`, `moved`, `published`, `unpublished`, `deleted`, `restored`,
    /// `section`, `imported`.
    pub change: String,
    /// Whether anonymous visitors could see the page before or after the change (section
    /// changes are public). Decides which live topic carries the event.
    #[serde(skip)]
    pub public: bool,
}

type Notifier = Arc<dyn Fn(&WikiChange) + Send + Sync>;

/// The wiki service. Cheap to clone.
#[derive(Clone)]
pub struct Wiki {
    db: wl_db::Db,
    data_dir: PathBuf,
    search: SearchCache,
    notify: Option<Notifier>,
}

impl std::fmt::Debug for Wiki {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Wiki")
            .field("data_dir", &self.data_dir)
            .finish_non_exhaustive()
    }
}

impl Wiki {
    /// The wiki over `db`, with images under `<data_dir>/wiki/assets`. `search` is the shared
    /// search index cache (keep one per process).
    pub fn new(db: wl_db::Db, data_dir: impl Into<PathBuf>, search: SearchCache) -> Self {
        Self {
            db,
            data_dir: data_dir.into(),
            search,
            notify: None,
        }
    }

    /// Call `notify` after every committed change (to publish live events).
    pub fn with_notifier(mut self, notify: impl Fn(&WikiChange) + Send + Sync + 'static) -> Self {
        self.notify = Some(Arc::new(notify));
        self
    }

    pub fn db(&self) -> &wl_db::Db {
        &self.db
    }

    /// Where uploaded images are stored.
    pub fn assets_dir(&self) -> PathBuf {
        self.data_dir.join("wiki").join("assets")
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    fn changed(&self, change: WikiChange) {
        if let Some(notify) = &self.notify {
            notify(&change);
        }
    }
}

/// What kind of error a [`WikiError`] is; decides the HTTP status and the `error` code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// 401 `unauthenticated`.
    Unauthenticated,
    /// 401 `access_denied` (a missing permission key, as everywhere in the API).
    AccessDenied,
    /// 404 `not_found` (also for pages the viewer may not see).
    NotFound,
    /// 400 `bad_request`.
    BadRequest,
    /// 400 `invalid_markdown`, with `problems`.
    InvalidMarkdown,
    /// 422 `invalid_json`: a request body that isn't the expected JSON.
    InvalidJson,
    /// 409 `conflict` (stale revision, taken slug, non-empty section), maybe with
    /// `current_revision`.
    Conflict,
    /// 428 `precondition_required`: a content update without a base revision.
    PreconditionRequired,
    /// 413 `too_large`.
    TooLarge,
    /// 500 `internal` (details are logged, not sent).
    Internal,
}

/// A wiki operation failed. The API sends it as JSON:
/// `{"error": code, "message": …, "problems"?: […], "current_revision"?: id}`.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct WikiError {
    pub kind: ErrorKind,
    pub message: String,
    pub problems: Vec<Problem>,
    pub current_revision: Option<i64>,
    /// A `conflict` deleting an image that pages show: those pages (the ones the viewer may
    /// see).
    pub used_by: Vec<model::PageLink>,
    #[source]
    pub source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

pub type WikiResult<T> = Result<T, WikiError>;

impl WikiError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            problems: Vec::new(),
            current_revision: None,
            used_by: Vec::new(),
            source: None,
        }
    }

    pub fn unauthenticated() -> Self {
        Self::new(ErrorKind::Unauthenticated, "log in first")
    }

    pub fn access_denied(key: &str) -> Self {
        Self::new(
            ErrorKind::AccessDenied,
            format!("access denied: needs `{key}`"),
        )
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::NotFound, message)
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::BadRequest, message)
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Conflict, message)
    }

    pub fn invalid_markdown(problems: Vec<Problem>) -> Self {
        let first = problems
            .first()
            .map(|p| format!("line {}: {}", p.line, p.message))
            .unwrap_or_default();
        let mut e = Self::new(
            ErrorKind::InvalidMarkdown,
            format!(
                "the page has {} problem{}: {first}",
                problems.len(),
                if problems.len() == 1 { "" } else { "s" }
            ),
        );
        e.problems = problems;
        e
    }

    pub fn internal(err: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> Self {
        let mut e = Self::new(ErrorKind::Internal, "internal error");
        e.source = Some(err.into());
        e
    }

    /// The HTTP status.
    pub fn status(&self) -> u16 {
        match self.kind {
            ErrorKind::Unauthenticated | ErrorKind::AccessDenied => 401,
            ErrorKind::NotFound => 404,
            ErrorKind::BadRequest | ErrorKind::InvalidMarkdown => 400,
            ErrorKind::InvalidJson => 422,
            ErrorKind::Conflict => 409,
            ErrorKind::PreconditionRequired => 428,
            ErrorKind::TooLarge => 413,
            ErrorKind::Internal => 500,
        }
    }

    /// The machine-readable `error` code.
    pub fn code(&self) -> &'static str {
        match self.kind {
            ErrorKind::Unauthenticated => "unauthenticated",
            ErrorKind::AccessDenied => "access_denied",
            ErrorKind::NotFound => "not_found",
            ErrorKind::BadRequest => "bad_request",
            ErrorKind::InvalidMarkdown => "invalid_markdown",
            ErrorKind::InvalidJson => "invalid_json",
            ErrorKind::Conflict => "conflict",
            ErrorKind::PreconditionRequired => "precondition_required",
            ErrorKind::TooLarge => "too_large",
            ErrorKind::Internal => "internal",
        }
    }

    /// The JSON body sent to clients.
    pub fn body(&self) -> ErrorBody {
        ErrorBody {
            error: self.code().to_string(),
            message: self.message.clone(),
            problems: (!self.problems.is_empty()).then(|| self.problems.clone()),
            current_revision: self.current_revision,
            used_by: (self.kind == ErrorKind::Conflict && !self.used_by.is_empty())
                .then(|| self.used_by.clone()),
        }
    }
}

impl From<sqlx::Error> for WikiError {
    fn from(err: sqlx::Error) -> Self {
        Self::internal(err)
    }
}

pub(crate) fn now() -> i64 {
    chrono::Utc::now().timestamp()
}
