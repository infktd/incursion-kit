//! Wire types of the wiki API (also used by the service).

use serde::{Deserialize, Deserializer, Serialize};
use utoipa::ToSchema;

use crate::markdown::{Heading, Problem};

/// Whether a page is shown to readers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = WikiStatus)]
pub enum Status {
    /// Only editors (`wiki-edit`) see it.
    Draft,
    Published,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Published => "published",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "draft" => Some(Self::Draft),
            "published" => Some(Self::Published),
            _ => None,
        }
    }
}

/// Who may read a page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = WikiVisibility)]
pub enum Visibility {
    /// Everyone, logged in or not.
    Public,
    /// Any logged-in account.
    Login,
    /// Accounts holding the page's `permission` key (e.g. `fleet-view` for FCs).
    Permission,
}

impl Visibility {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Login => "login",
            Self::Permission => "permission",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "public" => Some(Self::Public),
            "login" => Some(Self::Login),
            "permission" => Some(Self::Permission),
            _ => None,
        }
    }
}

/// A character (an account's main) that made a change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[schema(as = WikiCharacter)]
pub struct CharacterRef {
    pub id: i64,
    pub name: String,
}

/// A section, by key and label.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[schema(as = WikiSectionRef)]
pub struct SectionRef {
    pub key: String,
    pub label: String,
}

/// A page in the index or a section listing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[schema(as = WikiIndexPage)]
pub struct IndexPage {
    pub id: i64,
    /// The page's path (`guides/newbro`).
    pub slug: String,
    pub title: String,
    pub summary: String,
    /// Who the page is for ("All new pilots").
    #[serde(rename = "for")]
    pub audience: Option<String>,
    /// Number in the index (`2.3`); `null` in unlisted sections.
    pub num: Option<String>,
    /// Order within the section (ascending; ties by title).
    pub position: i32,
    pub status: Status,
    pub visibility: Visibility,
    pub permission: Option<String>,
    pub revision_id: Option<i64>,
    pub updated_at: i64,
    /// The current Markdown body: only in section listings with `body=true`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
}

/// A section of the index with the pages the viewer may see.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[schema(as = WikiIndexSection)]
pub struct IndexSection {
    pub key: String,
    pub label: String,
    /// Number in the index (`2`).
    pub num: String,
    pub pages: Vec<IndexPage>,
}

/// `GET /api/wiki`: the numbered index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[schema(as = WikiIndex)]
pub struct Index {
    /// Listed sections with at least one page the viewer may see, numbered 1, 2, 3… in order.
    pub sections: Vec<IndexSection>,
    /// The viewer holds `wiki-edit` (may create and edit pages).
    pub can_edit: bool,
    /// The viewer holds `wiki-admin`.
    pub can_admin: bool,
}

/// A page linking to another.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[schema(as = WikiPageLink)]
pub struct PageLink {
    pub slug: String,
    pub title: String,
}

/// A page with its body and metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[schema(as = WikiPage)]
pub struct Page {
    pub id: i64,
    pub slug: String,
    pub title: String,
    pub summary: String,
    #[serde(rename = "for")]
    pub audience: Option<String>,
    pub section: SectionRef,
    /// Number in the index (`2.3`), when the page is listed there for this viewer.
    pub num: Option<String>,
    pub position: i32,
    pub status: Status,
    pub visibility: Visibility,
    pub permission: Option<String>,
    /// Markdown with directives (see `docs/wiki.md`).
    pub body: String,
    /// The current revision: send it back as `base_revision` (or `If-Match`) when saving.
    pub revision_id: Option<i64>,
    pub created_at: i64,
    pub created_by: Option<CharacterRef>,
    /// Last change of any kind (content, move, status, visibility).
    pub updated_at: i64,
    pub updated_by: Option<CharacterRef>,
    pub published_at: Option<i64>,
    /// Minutes to read (200 words a minute, at least 1).
    pub read_minutes: u32,
    pub words: usize,
    /// Headings of the body, for "on this page".
    pub headings: Vec<Heading>,
    /// Pages the viewer may see that link here (`/wiki/<slug>`).
    pub backlinks: Vec<PageLink>,
    /// The viewer may edit this page.
    pub can_edit: bool,
}

/// A section with its settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[schema(as = WikiSection)]
pub struct Section {
    pub key: String,
    pub label: String,
    pub position: i32,
    /// Shown in the index (unlisted sections hold pages a frontend shows elsewhere).
    pub listed: bool,
    /// Pages in it that the viewer may see.
    pub pages: i64,
}

/// `GET /api/wiki/sections/{key}/pages`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[schema(as = WikiSectionPages)]
pub struct SectionPages {
    pub section: Section,
    pub pages: Vec<IndexPage>,
}

/// A revision without its body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[schema(as = WikiRevisionSummary)]
pub struct RevisionSummary {
    pub id: i64,
    pub title: String,
    pub message: String,
    pub author: Option<CharacterRef>,
    pub created_at: i64,
    /// Body length in bytes.
    pub size: i64,
    /// The page's current revision.
    pub current: bool,
}

/// A revision with its content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[schema(as = WikiRevision)]
pub struct Revision {
    pub id: i64,
    pub page_id: i64,
    pub title: String,
    pub summary: String,
    #[serde(rename = "for")]
    pub audience: Option<String>,
    pub body: String,
    pub message: String,
    pub author: Option<CharacterRef>,
    pub created_at: i64,
    pub current: bool,
}

/// One line of a diff.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[schema(as = WikiDiffLine)]
pub struct DiffLine {
    /// `equal`, `insert` or `delete`.
    pub op: String,
    /// 1-based line in the older body (not for inserts).
    pub old_line: Option<usize>,
    /// 1-based line in the newer body (not for deletes).
    pub new_line: Option<usize>,
    /// The line, without its line break.
    pub text: String,
}

/// `GET /api/wiki/pages/{id}/diff`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[schema(as = WikiDiff)]
pub struct Diff {
    pub from: i64,
    pub to: i64,
    /// Title of each side.
    pub from_title: String,
    pub to_title: String,
    pub summary_changed: bool,
    pub insertions: usize,
    pub deletions: usize,
    /// The body diff as a unified diff (3 lines of context).
    pub unified: String,
    /// Every line of both bodies, in order.
    pub lines: Vec<DiffLine>,
}

/// An uploaded image.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[schema(as = WikiAsset)]
pub struct Asset {
    pub id: i64,
    /// File name (an opaque PNG over 1 MiB is stored as `.jpg`).
    pub filename: String,
    /// Where to load it: `/api/wiki/assets/{id}/{filename}`. Use this in Markdown images.
    pub url: String,
    pub content_type: String,
    pub size: i64,
    pub uploaded_at: i64,
    pub uploaded_by: Option<CharacterRef>,
}

/// A piece of a search snippet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[schema(as = WikiSnippetPart)]
pub struct SnippetPart {
    pub text: String,
    /// The text matched the query (highlight it).
    pub highlight: bool,
}

/// One search hit.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[schema(as = WikiSearchHit)]
pub struct SearchHit {
    pub id: i64,
    pub slug: String,
    pub title: String,
    pub summary: String,
    pub section: SectionRef,
    pub status: Status,
    /// Relevance (higher is better); only comparable within one answer.
    pub score: f64,
    /// Text around the first match in the body (or the start of the body), split into
    /// highlighted and plain parts. Plain text: no Markdown, no HTML.
    pub snippet: Vec<SnippetPart>,
}

/// `GET /api/wiki/search`.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[schema(as = WikiSearchResults)]
pub struct SearchResults {
    pub query: String,
    pub hits: Vec<SearchHit>,
}

/// The JSON body of every wiki error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[schema(as = WikiErrorBody)]
pub struct ErrorBody {
    /// `unauthenticated`, `access_denied`, `not_found`, `bad_request`, `invalid_markdown`,
    /// `invalid_json`, `conflict`, `precondition_required`, `too_large` or `internal`.
    pub error: String,
    pub message: String,
    /// `invalid_markdown`: every problem in the body.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub problems: Option<Vec<Problem>>,
    /// `conflict` on a stale save: the page's current revision.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_revision: Option<i64>,
}

/// `POST /api/wiki/pages`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[schema(as = WikiNewPage)]
pub struct NewPage {
    /// The page's path (`guides/newbro`): 1 to 6 parts of lowercase letters, digits, `-`, `_`.
    pub slug: String,
    pub title: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default, rename = "for")]
    pub audience: Option<String>,
    /// Section key.
    pub section: String,
    /// 1-based place in the section; the end when absent.
    #[serde(default)]
    pub position: Option<i32>,
    #[serde(default)]
    pub body: String,
    /// `draft` (default) or `published`.
    #[serde(default)]
    pub status: Option<Status>,
    /// `public` (default), `login` or `permission`.
    #[serde(default)]
    pub visibility: Option<Visibility>,
    /// The permission key for `visibility: permission`.
    #[serde(default)]
    pub permission: Option<String>,
    /// Revision message.
    #[serde(default)]
    pub message: Option<String>,
}

/// `PUT /api/wiki/pages/{id}`: the fields to change (absent ones are kept).
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[schema(as = WikiPageUpdate)]
pub struct PageUpdate {
    /// The revision the edit is based on (or send `If-Match: "<revision id>"`). Required when
    /// changing content (title, summary, `for`, body); a stale one is a `409 conflict`.
    #[serde(default)]
    pub base_revision: Option<i64>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    /// `null` clears it.
    #[serde(default, rename = "for", deserialize_with = "double_option")]
    #[schema(value_type = Option<String>)]
    pub audience: Option<Option<String>>,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub visibility: Option<Visibility>,
    /// `null` clears it.
    #[serde(default, deserialize_with = "double_option")]
    #[schema(value_type = Option<String>)]
    pub permission: Option<Option<String>>,
    /// Revision message.
    #[serde(default)]
    pub message: Option<String>,
}

impl PageUpdate {
    pub fn changes_content(&self) -> bool {
        self.title.is_some()
            || self.summary.is_some()
            || self.audience.is_some()
            || self.body.is_some()
    }
}

/// `POST /api/wiki/pages/{id}/move`.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[schema(as = WikiMove)]
pub struct Move {
    /// New path (links to the old one are not rewritten).
    #[serde(default)]
    pub slug: Option<String>,
    /// New section key.
    #[serde(default)]
    pub section: Option<String>,
    /// New 1-based place in the (new) section; the end when moving sections without one.
    #[serde(default)]
    pub position: Option<i32>,
}

/// `POST /api/wiki/sections`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[schema(as = WikiNewSection)]
pub struct NewSection {
    /// `[a-z0-9][a-z0-9_-]*`, at most 64 characters; can't be changed later.
    pub key: String,
    pub label: String,
    /// 1-based place among the sections; the end when absent.
    #[serde(default)]
    pub position: Option<i32>,
    /// Shown in the index (default `true`).
    #[serde(default)]
    pub listed: Option<bool>,
}

/// `PUT /api/wiki/sections/{key}`.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[schema(as = WikiSectionUpdate)]
pub struct SectionUpdate {
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub listed: Option<bool>,
}

/// A deleted page (`GET /api/wiki/deleted`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[schema(as = WikiDeletedPage)]
pub struct DeletedPage {
    pub id: i64,
    pub slug: String,
    pub title: String,
    pub section: String,
    pub deleted_at: i64,
    pub deleted_by: Option<CharacterRef>,
}

/// The result of validating a body (`POST /api/wiki/validate`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[schema(as = WikiValidation)]
pub struct Validation {
    pub valid: bool,
    pub problems: Vec<Problem>,
    pub headings: Vec<Heading>,
    /// Wiki pages the body links to.
    pub links: Vec<String>,
    pub read_minutes: u32,
    pub words: usize,
}

/// `Some(None)` for an explicit `null`, `None` when the field is absent.
fn double_option<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}
