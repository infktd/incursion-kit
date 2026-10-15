//! The database side of [`Wiki`]: reads filtered by the viewer, checked writes.

use std::collections::HashMap;
use std::sync::Arc;

use sqlx::{FromRow, MySql, Transaction};

use crate::markdown::{Analysis, analyze};
use crate::model::*;
use crate::search::{Document, SearchIndex};
use crate::slug::{validate_section_key, validate_slug};
use crate::{ADMIN_KEY, Checks, ErrorKind, Viewer, Wiki, WikiChange, WikiError, WikiResult, now};

pub(crate) type Tx = Transaction<'static, MySql>;

/// Longest body.
pub const MAX_BODY_BYTES: usize = 512 * 1024;
const MAX_SUMMARY_CHARS: usize = 2000;
const MAX_LINE_CHARS: usize = 255;
/// Most search hits per request.
pub const MAX_SEARCH_HITS: usize = 50;

macro_rules! page_select {
    ($rest:literal) => {
        concat!(
            "SELECT p.id, p.slug, p.section_key, p.position, p.title, p.summary, p.audience, \
             p.body, p.status, p.visibility, p.permission, p.revision_id, p.created_at, \
             p.created_by, cc.name AS created_by_name, p.updated_at, p.updated_by, \
             cu.name AS updated_by_name, p.published_at \
             FROM wiki_page p \
             LEFT JOIN `character` cc ON cc.id = p.created_by \
             LEFT JOIN `character` cu ON cu.id = p.updated_by ",
            $rest
        )
    };
}

#[derive(Debug, Clone, FromRow)]
pub(crate) struct PageRow {
    pub id: i64,
    pub slug: String,
    pub section_key: String,
    pub position: i32,
    pub title: String,
    pub summary: String,
    pub audience: Option<String>,
    pub body: String,
    pub status: String,
    pub visibility: String,
    pub permission: Option<String>,
    pub revision_id: Option<i64>,
    pub created_at: i64,
    pub created_by: Option<i64>,
    pub created_by_name: Option<String>,
    pub updated_at: i64,
    pub updated_by: Option<i64>,
    pub updated_by_name: Option<String>,
    pub published_at: Option<i64>,
}

impl PageRow {
    pub fn status(&self) -> Status {
        Status::parse(&self.status).unwrap_or(Status::Draft)
    }

    pub fn visibility(&self) -> Visibility {
        // An unknown value is treated as the most restrictive.
        Visibility::parse(&self.visibility).unwrap_or(Visibility::Permission)
    }

    pub fn visible_to(&self, viewer: &Viewer) -> bool {
        viewer.can_view(self.visibility(), self.permission.as_deref(), self.status())
    }

    pub fn is_public(&self) -> bool {
        is_public(self.status(), self.visibility())
    }
}

pub(crate) fn is_public(status: Status, visibility: Visibility) -> bool {
    status == Status::Published && visibility == Visibility::Public
}

/// A page row without its body (index listings).
#[derive(Debug, Clone, FromRow)]
struct ListRow {
    id: i64,
    slug: String,
    section_key: String,
    position: i32,
    title: String,
    summary: String,
    audience: Option<String>,
    status: String,
    visibility: String,
    permission: Option<String>,
    revision_id: Option<i64>,
    updated_at: i64,
}

impl ListRow {
    fn status(&self) -> Status {
        Status::parse(&self.status).unwrap_or(Status::Draft)
    }

    fn visible_to(&self, viewer: &Viewer) -> bool {
        viewer.can_view(
            Visibility::parse(&self.visibility).unwrap_or(Visibility::Permission),
            self.permission.as_deref(),
            self.status(),
        )
    }

    fn to_index_page(&self, num: Option<String>) -> IndexPage {
        IndexPage {
            id: self.id,
            slug: self.slug.clone(),
            title: self.title.clone(),
            summary: self.summary.clone(),
            audience: self.audience.clone(),
            num,
            position: self.position,
            status: self.status(),
            visibility: Visibility::parse(&self.visibility).unwrap_or(Visibility::Permission),
            permission: self.permission.clone(),
            revision_id: self.revision_id,
            updated_at: self.updated_at,
            body: None,
        }
    }
}

#[derive(Debug, Clone, FromRow)]
pub(crate) struct SectionRow {
    pub key: String,
    pub label: String,
    pub position: i32,
    pub listed: bool,
}

#[derive(Debug, Clone, FromRow)]
struct RevisionRow {
    id: i64,
    page_id: i64,
    title: String,
    summary: String,
    audience: Option<String>,
    body: String,
    message: String,
    author_id: Option<i64>,
    author_name: Option<String>,
    created_at: i64,
}

fn character(id: Option<i64>, name: Option<String>) -> Option<CharacterRef> {
    id.map(|id| CharacterRef {
        id,
        name: name.unwrap_or_else(|| format!("#{id}")),
    })
}

pub(crate) fn is_duplicate(err: &sqlx::Error) -> bool {
    err.as_database_error()
        .and_then(|e| e.code())
        .is_some_and(|code| code == "23000")
}

// --- field checks ----------------------------------------------------------------------------

pub(crate) fn check_title(title: &str) -> WikiResult<String> {
    let title = title.trim();
    if title.is_empty() || title.chars().count() > MAX_LINE_CHARS {
        return Err(WikiError::bad_request("a title is 1 to 255 characters"));
    }
    Ok(title.to_string())
}

pub(crate) fn check_summary(summary: &str) -> WikiResult<String> {
    let summary = summary.trim();
    if summary.chars().count() > MAX_SUMMARY_CHARS {
        return Err(WikiError::bad_request(format!(
            "a summary is at most {MAX_SUMMARY_CHARS} characters"
        )));
    }
    Ok(summary.to_string())
}

pub(crate) fn check_audience(audience: Option<&str>) -> WikiResult<Option<String>> {
    let audience = audience.map(str::trim).filter(|a| !a.is_empty());
    if audience.is_some_and(|a| a.chars().count() > MAX_LINE_CHARS) {
        return Err(WikiError::bad_request("`for` is at most 255 characters"));
    }
    Ok(audience.map(str::to_string))
}

pub(crate) fn check_message(message: Option<&str>) -> WikiResult<String> {
    let message = message.map(str::trim).unwrap_or_default();
    if message.chars().count() > MAX_LINE_CHARS {
        return Err(WikiError::bad_request(
            "a revision message is at most 255 characters",
        ));
    }
    Ok(message.to_string())
}

pub(crate) fn check_label(label: &str) -> WikiResult<String> {
    let label = label.trim();
    if label.is_empty() || label.chars().count() > MAX_LINE_CHARS {
        return Err(WikiError::bad_request(
            "a section label is 1 to 255 characters",
        ));
    }
    Ok(label.to_string())
}

/// Validate a body: size and components.
pub(crate) fn check_body(body: &str, checks: &Checks) -> WikiResult<Analysis> {
    if body.len() > MAX_BODY_BYTES {
        return Err(WikiError::new(
            ErrorKind::TooLarge,
            format!("a page body is at most {} KiB", MAX_BODY_BYTES / 1024),
        ));
    }
    let analysis = analyze(body, checks.fit_names.as_ref());
    if !analysis.problems.is_empty() {
        return Err(WikiError::invalid_markdown(analysis.problems));
    }
    Ok(analysis)
}

fn check_slug(slug: &str) -> WikiResult<String> {
    let slug = slug.trim().trim_matches('/');
    validate_slug(slug).map_err(WikiError::bad_request)?;
    Ok(slug.to_string())
}

/// The permission to store for `visibility`; only keys the viewer holds (unless `wiki-admin`).
fn check_visibility(
    viewer: &Viewer,
    checks: &Checks,
    visibility: Visibility,
    permission: Option<String>,
) -> WikiResult<Option<String>> {
    let permission = permission
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty());
    match visibility {
        Visibility::Permission => {
            let key = permission.ok_or_else(|| {
                WikiError::bad_request("visibility `permission` needs a `permission` key")
            })?;
            if !checks.permission_keys.is_empty() && !checks.permission_keys.contains(&key) {
                return Err(WikiError::bad_request(format!(
                    "unknown permission key `{key}`"
                )));
            }
            if !viewer.can_admin() && !viewer.has(&key) {
                return Err(WikiError::new(
                    ErrorKind::AccessDenied,
                    format!(
                        "access denied: you can only restrict pages to keys you hold (`{key}`)"
                    ),
                ));
            }
            Ok(Some(key))
        }
        _ => {
            if permission.is_some() {
                return Err(WikiError::bad_request(
                    "`permission` is only used with visibility `permission`",
                ));
            }
            Ok(None)
        }
    }
}

// --- shared write helpers --------------------------------------------------------------------

pub(crate) async fn bump(tx: &mut Tx) -> WikiResult<()> {
    sqlx::query("UPDATE wiki_state SET generation = generation + 1 WHERE id = 1")
        .execute(&mut **tx)
        .await?;
    Ok(())
}

pub(crate) async fn section_exists(tx: &mut Tx, key: &str) -> WikiResult<bool> {
    let found: Option<String> =
        sqlx::query_scalar("SELECT `key` FROM wiki_section WHERE `key` = ?")
            .bind(key)
            .fetch_optional(&mut **tx)
            .await?;
    Ok(found.is_some())
}

/// Live pages of a section in index order, locked.
pub(crate) async fn section_order(tx: &mut Tx, key: &str) -> WikiResult<Vec<i64>> {
    Ok(sqlx::query_scalar(
        "SELECT id FROM wiki_page WHERE section_key = ? AND deleted_at IS NULL \
         ORDER BY position, title, id FOR UPDATE",
    )
    .bind(key)
    .fetch_all(&mut **tx)
    .await?)
}

/// Positions 1..n in the given order.
pub(crate) async fn renumber(tx: &mut Tx, ids: &[i64]) -> WikiResult<()> {
    for (i, id) in ids.iter().enumerate() {
        sqlx::query("UPDATE wiki_page SET position = ? WHERE id = ?")
            .bind(i as i32 + 1)
            .bind(id)
            .execute(&mut **tx)
            .await?;
    }
    Ok(())
}

/// Insert `id` at the 1-based `position` (clamped; `None` = the end).
fn place(mut ids: Vec<i64>, id: i64, position: Option<i32>) -> Vec<i64> {
    ids.retain(|x| *x != id);
    let index = match position {
        Some(p) => (p.max(1) as usize - 1).min(ids.len()),
        None => ids.len(),
    };
    ids.insert(index, id);
    ids
}

pub(crate) struct Content<'a> {
    pub title: &'a str,
    pub summary: &'a str,
    pub audience: Option<&'a str>,
    pub body: &'a str,
    pub message: &'a str,
    pub author: Option<i64>,
    pub at: i64,
}

/// Store new content for a page: a revision, the page's current fields, its links.
pub(crate) async fn save_content(
    tx: &mut Tx,
    page_id: i64,
    content: &Content<'_>,
    links: &[String],
) -> WikiResult<i64> {
    let revision_id = sqlx::query(
        "INSERT INTO wiki_revision (page_id, title, summary, audience, body, message, author_id, \
         created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(page_id)
    .bind(content.title)
    .bind(content.summary)
    .bind(content.audience)
    .bind(content.body)
    .bind(content.message)
    .bind(content.author)
    .bind(content.at)
    .execute(&mut **tx)
    .await?
    .last_insert_id() as i64;
    sqlx::query(
        "UPDATE wiki_page SET title = ?, summary = ?, audience = ?, body = ?, revision_id = ?, \
         updated_at = ?, updated_by = ? WHERE id = ?",
    )
    .bind(content.title)
    .bind(content.summary)
    .bind(content.audience)
    .bind(content.body)
    .bind(revision_id)
    .bind(content.at)
    .bind(content.author)
    .bind(page_id)
    .execute(&mut **tx)
    .await?;
    sqlx::query("DELETE FROM wiki_link WHERE page_id = ?")
        .bind(page_id)
        .execute(&mut **tx)
        .await?;
    for target in links {
        sqlx::query("INSERT INTO wiki_link (page_id, target) VALUES (?, ?)")
            .bind(page_id)
            .bind(target)
            .execute(&mut **tx)
            .await?;
    }
    Ok(revision_id)
}

pub(crate) struct NewRow<'a> {
    pub slug: &'a str,
    pub section: &'a str,
    pub status: Status,
    pub visibility: Visibility,
    pub permission: Option<&'a str>,
    pub position: i32,
}

/// Insert an empty page row (content follows with [`save_content`]).
pub(crate) async fn insert_page(
    tx: &mut Tx,
    row: &NewRow<'_>,
    title: &str,
    author: Option<i64>,
    at: i64,
) -> WikiResult<i64> {
    let result = sqlx::query(
        "INSERT INTO wiki_page (slug, section_key, position, title, summary, body, status, \
         visibility, permission, created_at, created_by, updated_at, updated_by, published_at) \
         VALUES (?, ?, ?, ?, '', '', ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(row.slug)
    .bind(row.section)
    .bind(row.position)
    .bind(title)
    .bind(row.status.as_str())
    .bind(row.visibility.as_str())
    .bind(row.permission)
    .bind(at)
    .bind(author)
    .bind(at)
    .bind(author)
    .bind((row.status == Status::Published).then_some(at))
    .execute(&mut **tx)
    .await;
    match result {
        Ok(done) => Ok(done.last_insert_id() as i64),
        Err(e) if is_duplicate(&e) => Err(slug_taken(row.slug)),
        Err(e) => Err(e.into()),
    }
}

fn slug_taken(slug: &str) -> WikiError {
    WikiError::conflict(format!("a page with the path `{slug}` already exists"))
}

pub(crate) async fn slug_free(tx: &mut Tx, slug: &str, except: Option<i64>) -> WikiResult<bool> {
    let found: Option<i64> =
        sqlx::query_scalar("SELECT id FROM wiki_page WHERE live_slug = ? FOR UPDATE")
            .bind(slug)
            .fetch_optional(&mut **tx)
            .await?;
    Ok(found.is_none() || found == except)
}

async fn lock_page(tx: &mut Tx, id: i64, viewer: &Viewer) -> WikiResult<PageRow> {
    let row: Option<PageRow> = sqlx::query_as(page_select!(
        "WHERE p.id = ? AND p.deleted_at IS NULL FOR UPDATE OF p"
    ))
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?;
    row.filter(|r| r.visible_to(viewer))
        .ok_or_else(|| WikiError::not_found("page not found"))
}

fn sort_rows(rows: &mut [ListRow]) {
    rows.sort_by(|a, b| {
        a.section_key
            .cmp(&b.section_key)
            .then(a.position.cmp(&b.position))
            .then_with(|| a.title.cmp(&b.title))
            .then(a.id.cmp(&b.id))
    });
}

impl Wiki {
    async fn begin(&self) -> WikiResult<Tx> {
        Ok(self.db.begin().await?)
    }

    async fn section_rows(&self) -> WikiResult<Vec<SectionRow>> {
        Ok(sqlx::query_as(
            "SELECT `key`, label, position, listed FROM wiki_section ORDER BY position, `key`",
        )
        .fetch_all(&self.db)
        .await?)
    }

    async fn list_rows(&self) -> WikiResult<Vec<ListRow>> {
        let mut rows: Vec<ListRow> = sqlx::query_as(
            "SELECT id, slug, section_key, position, title, summary, audience, status, \
             visibility, permission, revision_id, updated_at \
             FROM wiki_page WHERE deleted_at IS NULL",
        )
        .fetch_all(&self.db)
        .await?;
        sort_rows(&mut rows);
        Ok(rows)
    }

    // --- reads -------------------------------------------------------------------------------

    /// The numbered index of listed sections. Drafts only with `drafts` (and `wiki-edit`).
    pub async fn index(&self, viewer: &Viewer, drafts: bool) -> WikiResult<Index> {
        let sections = self.section_rows().await?;
        let rows = self.list_rows().await?;
        Ok(Index {
            sections: build_index(viewer, &sections, &rows, drafts),
            can_edit: viewer.can_edit(),
            can_admin: viewer.can_admin(),
        })
    }

    /// A page by its path.
    pub async fn page_by_slug(&self, viewer: &Viewer, slug: &str) -> WikiResult<Page> {
        let row: Option<PageRow> = sqlx::query_as(page_select!("WHERE p.live_slug = ?"))
            .bind(slug.trim_matches('/'))
            .fetch_optional(&self.db)
            .await?;
        let row = row
            .filter(|r| r.visible_to(viewer))
            .ok_or_else(|| WikiError::not_found("page not found"))?;
        self.page_view(viewer, row).await
    }

    /// A page by id.
    pub async fn page(&self, viewer: &Viewer, id: i64) -> WikiResult<Page> {
        let row = self.page_row(viewer, id).await?;
        self.page_view(viewer, row).await
    }

    async fn page_row(&self, viewer: &Viewer, id: i64) -> WikiResult<PageRow> {
        let row: Option<PageRow> =
            sqlx::query_as(page_select!("WHERE p.id = ? AND p.deleted_at IS NULL"))
                .bind(id)
                .fetch_optional(&self.db)
                .await?;
        row.filter(|r| r.visible_to(viewer))
            .ok_or_else(|| WikiError::not_found("page not found"))
    }

    async fn page_view(&self, viewer: &Viewer, row: PageRow) -> WikiResult<Page> {
        let sections = self.section_rows().await?;
        let rows = self.list_rows().await?;
        let num = build_index(viewer, &sections, &rows, false)
            .into_iter()
            .flat_map(|s| s.pages)
            .find(|p| p.id == row.id)
            .and_then(|p| p.num);
        let label = sections
            .iter()
            .find(|s| s.key == row.section_key)
            .map(|s| s.label.clone())
            .unwrap_or_else(|| row.section_key.clone());

        #[derive(FromRow)]
        struct Backlink {
            slug: String,
            title: String,
            status: String,
            visibility: String,
            permission: Option<String>,
        }
        let backlinks: Vec<Backlink> = sqlx::query_as(
            "SELECT p.slug, p.title, p.status, p.visibility, p.permission \
             FROM wiki_link l JOIN wiki_page p ON p.id = l.page_id \
             WHERE l.target = ? AND p.id <> ? AND p.deleted_at IS NULL ORDER BY p.title, p.id",
        )
        .bind(&row.slug)
        .bind(row.id)
        .fetch_all(&self.db)
        .await?;
        let backlinks = backlinks
            .into_iter()
            .filter(|b| {
                viewer.can_view(
                    Visibility::parse(&b.visibility).unwrap_or(Visibility::Permission),
                    b.permission.as_deref(),
                    Status::parse(&b.status).unwrap_or(Status::Draft),
                )
            })
            .map(|b| PageLink {
                slug: b.slug,
                title: b.title,
            })
            .collect();

        let analysis = analyze(&row.body, None);
        Ok(Page {
            id: row.id,
            section: SectionRef {
                key: row.section_key.clone(),
                label,
            },
            num,
            position: row.position,
            status: row.status(),
            visibility: row.visibility(),
            permission: row.permission.clone(),
            revision_id: row.revision_id,
            created_at: row.created_at,
            created_by: character(row.created_by, row.created_by_name),
            updated_at: row.updated_at,
            updated_by: character(row.updated_by, row.updated_by_name),
            published_at: row.published_at,
            read_minutes: analysis.read_minutes(),
            words: analysis.words,
            headings: analysis.headings,
            backlinks,
            can_edit: viewer.can_edit(),
            slug: row.slug,
            title: row.title,
            summary: row.summary,
            audience: row.audience,
            body: row.body,
        })
    }

    /// Every section with the number of pages the viewer may see in it.
    pub async fn sections(&self, viewer: &Viewer) -> WikiResult<Vec<Section>> {
        let sections = self.section_rows().await?;
        let rows = self.list_rows().await?;
        Ok(sections
            .into_iter()
            .map(|s| {
                let pages = rows
                    .iter()
                    .filter(|r| r.section_key == s.key && r.visible_to(viewer))
                    .count() as i64;
                Section {
                    key: s.key,
                    label: s.label,
                    position: s.position,
                    listed: s.listed,
                    pages,
                }
            })
            .collect())
    }

    /// The pages of one section (listed or not) in order, optionally with their bodies (FC
    /// cheatsheets: one request for all of them).
    pub async fn section_pages(
        &self,
        viewer: &Viewer,
        key: &str,
        drafts: bool,
        with_body: bool,
    ) -> WikiResult<SectionPages> {
        let sections = self.sections(viewer).await?;
        let section = sections
            .into_iter()
            .find(|s| s.key == key)
            .ok_or_else(|| WikiError::not_found("section not found"))?;
        let rows = self.list_rows().await?;
        let numbers: HashMap<i64, Option<String>> = {
            let section_rows = self.section_rows().await?;
            build_index(viewer, &section_rows, &rows, drafts)
                .into_iter()
                .flat_map(|s| s.pages)
                .map(|p| (p.id, p.num))
                .collect()
        };
        let mut pages: Vec<IndexPage> = rows
            .iter()
            .filter(|r| {
                r.section_key == key
                    && r.visible_to(viewer)
                    && (r.status() == Status::Published || (drafts && viewer.can_edit()))
            })
            .map(|r| r.to_index_page(numbers.get(&r.id).cloned().flatten()))
            .collect();
        if with_body && !pages.is_empty() {
            let bodies: Vec<(i64, String)> = sqlx::query_as(
                "SELECT id, body FROM wiki_page WHERE section_key = ? AND deleted_at IS NULL",
            )
            .bind(key)
            .fetch_all(&self.db)
            .await?;
            let bodies: HashMap<i64, String> = bodies.into_iter().collect();
            for page in &mut pages {
                page.body = bodies.get(&page.id).cloned();
            }
        }
        Ok(SectionPages { section, pages })
    }

    // --- page writes -------------------------------------------------------------------------

    /// Create a page (`wiki-edit`).
    pub async fn create_page(
        &self,
        viewer: &Viewer,
        checks: &Checks,
        new: NewPage,
    ) -> WikiResult<Page> {
        let author = viewer.require_edit()?;
        let slug = check_slug(&new.slug)?;
        let title = check_title(&new.title)?;
        let summary = check_summary(&new.summary)?;
        let audience = check_audience(new.audience.as_deref())?;
        let message = check_message(new.message.as_deref())?;
        let visibility = new.visibility.unwrap_or(Visibility::Public);
        let permission = check_visibility(viewer, checks, visibility, new.permission)?;
        let status = new.status.unwrap_or(Status::Draft);
        let analysis = check_body(&new.body, checks)?;

        let mut tx = self.begin().await?;
        if !section_exists(&mut tx, &new.section).await? {
            return Err(WikiError::bad_request(format!(
                "unknown section `{}`",
                new.section
            )));
        }
        if !slug_free(&mut tx, &slug, None).await? {
            return Err(slug_taken(&slug));
        }
        let order = section_order(&mut tx, &new.section).await?;
        let at = now();
        let id = insert_page(
            &mut tx,
            &NewRow {
                slug: &slug,
                section: &new.section,
                status,
                visibility,
                permission: permission.as_deref(),
                position: 0,
            },
            &title,
            Some(author),
            at,
        )
        .await?;
        save_content(
            &mut tx,
            id,
            &Content {
                title: &title,
                summary: &summary,
                audience: audience.as_deref(),
                body: &new.body,
                message: if message.is_empty() {
                    "Created"
                } else {
                    &message
                },
                author: Some(author),
                at,
            },
            &analysis.links,
        )
        .await?;
        renumber(&mut tx, &place(order, id, new.position)).await?;
        bump(&mut tx).await?;
        tx.commit().await?;
        self.changed(WikiChange {
            slug: Some(slug),
            section: Some(new.section),
            change: "created".into(),
            public: is_public(status, visibility),
        });
        self.page(viewer, id).await
    }

    /// Change a page's content and/or visibility (`wiki-edit`; another author's page's
    /// visibility needs `wiki-admin`). Content changes need the current revision as `base`.
    pub async fn update_page(
        &self,
        viewer: &Viewer,
        checks: &Checks,
        id: i64,
        update: PageUpdate,
    ) -> WikiResult<Page> {
        let author = viewer.require_edit()?;
        let mut tx = self.begin().await?;
        let row = lock_page(&mut tx, id, viewer).await?;
        match update.base_revision {
            None if update.changes_content() => {
                return Err(WikiError::new(
                    ErrorKind::PreconditionRequired,
                    "send the revision the edit is based on (`base_revision` or `If-Match`)",
                ));
            }
            Some(base) if Some(base) != row.revision_id => {
                let mut e = WikiError::conflict(
                    "the page was changed since this edit started: reload it and merge",
                );
                e.current_revision = row.revision_id;
                return Err(e);
            }
            _ => {}
        }

        let visibility = update.visibility.unwrap_or(row.visibility());
        let permission = match update.permission.clone() {
            Some(p) => p,
            None if visibility == Visibility::Permission => row.permission.clone(),
            None => None,
        };
        let permission = check_visibility(viewer, checks, visibility, permission)?;
        let visibility_changed = visibility != row.visibility() || permission != row.permission;
        if visibility_changed && row.created_by != Some(author) && !viewer.can_admin() {
            return Err(WikiError::access_denied(ADMIN_KEY));
        }

        let title = match &update.title {
            Some(t) => check_title(t)?,
            None => row.title.clone(),
        };
        let summary = match &update.summary {
            Some(s) => check_summary(s)?,
            None => row.summary.clone(),
        };
        let audience = match &update.audience {
            Some(a) => check_audience(a.as_deref())?,
            None => row.audience.clone(),
        };
        let body = update.body.clone().unwrap_or_else(|| row.body.clone());
        let message = check_message(update.message.as_deref())?;
        let content_changed = title != row.title
            || summary != row.summary
            || audience != row.audience
            || body != row.body;
        let at = now();
        if content_changed {
            let analysis = check_body(&body, checks)?;
            save_content(
                &mut tx,
                id,
                &Content {
                    title: &title,
                    summary: &summary,
                    audience: audience.as_deref(),
                    body: &body,
                    message: &message,
                    author: Some(author),
                    at,
                },
                &analysis.links,
            )
            .await?;
        }
        if visibility_changed {
            sqlx::query(
                "UPDATE wiki_page SET visibility = ?, permission = ?, updated_at = ?, \
                 updated_by = ? WHERE id = ?",
            )
            .bind(visibility.as_str())
            .bind(permission.as_deref())
            .bind(at)
            .bind(author)
            .bind(id)
            .execute(&mut *tx)
            .await?;
        }
        if content_changed || visibility_changed {
            bump(&mut tx).await?;
        }
        tx.commit().await?;
        if content_changed || visibility_changed {
            self.changed(WikiChange {
                slug: Some(row.slug.clone()),
                section: Some(row.section_key.clone()),
                change: "updated".into(),
                public: row.is_public() || is_public(row.status(), visibility),
            });
        }
        self.page(viewer, id).await
    }

    /// Rename, move to another section and/or reorder (`wiki-edit`).
    pub async fn move_page(&self, viewer: &Viewer, id: i64, mv: Move) -> WikiResult<Page> {
        let author = viewer.require_edit()?;
        let mut tx = self.begin().await?;
        let row = lock_page(&mut tx, id, viewer).await?;
        let slug = match &mv.slug {
            Some(s) => check_slug(s)?,
            None => row.slug.clone(),
        };
        if slug != row.slug && !slug_free(&mut tx, &slug, Some(id)).await? {
            return Err(slug_taken(&slug));
        }
        let section = mv
            .section
            .clone()
            .unwrap_or_else(|| row.section_key.clone());
        if section != row.section_key {
            if !section_exists(&mut tx, &section).await? {
                return Err(WikiError::bad_request(format!(
                    "unknown section `{section}`"
                )));
            }
            let mut old = section_order(&mut tx, &row.section_key).await?;
            old.retain(|x| *x != id);
            renumber(&mut tx, &old).await?;
        }
        if section != row.section_key || mv.position.is_some() {
            let order = section_order(&mut tx, &section).await?;
            renumber(&mut tx, &place(order, id, mv.position)).await?;
        }
        sqlx::query(
            "UPDATE wiki_page SET slug = ?, section_key = ?, updated_at = ?, updated_by = ? \
             WHERE id = ?",
        )
        .bind(&slug)
        .bind(&section)
        .bind(now())
        .bind(author)
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(|e| {
            if is_duplicate(&e) {
                slug_taken(&slug)
            } else {
                e.into()
            }
        })?;
        bump(&mut tx).await?;
        tx.commit().await?;
        if slug != row.slug {
            // Readers of the old path learn it is gone.
            self.changed(WikiChange {
                slug: Some(row.slug.clone()),
                section: Some(row.section_key.clone()),
                change: "moved".into(),
                public: row.is_public(),
            });
        }
        self.changed(WikiChange {
            slug: Some(slug),
            section: Some(section),
            change: "moved".into(),
            public: row.is_public(),
        });
        self.page(viewer, id).await
    }

    /// Publish or unpublish (`wiki-edit`).
    pub async fn set_status(&self, viewer: &Viewer, id: i64, status: Status) -> WikiResult<Page> {
        let author = viewer.require_edit()?;
        let mut tx = self.begin().await?;
        let row = lock_page(&mut tx, id, viewer).await?;
        if row.status() != status {
            let at = now();
            sqlx::query(
                "UPDATE wiki_page SET status = ?, updated_at = ?, updated_by = ?, \
                 published_at = IF(? = 'published', ?, published_at) WHERE id = ?",
            )
            .bind(status.as_str())
            .bind(at)
            .bind(author)
            .bind(status.as_str())
            .bind(at)
            .bind(id)
            .execute(&mut *tx)
            .await?;
            bump(&mut tx).await?;
            tx.commit().await?;
            self.changed(WikiChange {
                slug: Some(row.slug.clone()),
                section: Some(row.section_key.clone()),
                change: match status {
                    Status::Published => "published",
                    Status::Draft => "unpublished",
                }
                .into(),
                public: row.is_public() || is_public(status, row.visibility()),
            });
        }
        self.page(viewer, id).await
    }

    /// Soft-delete a page (`wiki-admin`). Its revisions are kept; it can be restored.
    pub async fn delete_page(&self, viewer: &Viewer, id: i64) -> WikiResult<()> {
        let author = viewer.require_admin()?;
        let mut tx = self.begin().await?;
        let row = lock_page(&mut tx, id, viewer).await?;
        sqlx::query("UPDATE wiki_page SET deleted_at = ?, deleted_by = ? WHERE id = ?")
            .bind(now())
            .bind(author)
            .bind(id)
            .execute(&mut *tx)
            .await?;
        let mut order = section_order(&mut tx, &row.section_key).await?;
        order.retain(|x| *x != id);
        renumber(&mut tx, &order).await?;
        bump(&mut tx).await?;
        tx.commit().await?;
        self.changed(WikiChange {
            slug: Some(row.slug.clone()),
            section: Some(row.section_key.clone()),
            change: "deleted".into(),
            public: row.is_public(),
        });
        Ok(())
    }

    /// Deleted pages, newest first (`wiki-admin`).
    pub async fn deleted_pages(&self, viewer: &Viewer) -> WikiResult<Vec<DeletedPage>> {
        viewer.require_admin()?;
        #[derive(FromRow)]
        struct Row {
            id: i64,
            slug: String,
            title: String,
            section_key: String,
            deleted_at: i64,
            deleted_by: Option<i64>,
            name: Option<String>,
        }
        let rows: Vec<Row> = sqlx::query_as(
            "SELECT p.id, p.slug, p.title, p.section_key, p.deleted_at, p.deleted_by, c.name \
             FROM wiki_page p LEFT JOIN `character` c ON c.id = p.deleted_by \
             WHERE p.deleted_at IS NOT NULL ORDER BY p.deleted_at DESC, p.id DESC",
        )
        .fetch_all(&self.db)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| DeletedPage {
                id: r.id,
                slug: r.slug,
                title: r.title,
                section: r.section_key,
                deleted_at: r.deleted_at,
                deleted_by: character(r.deleted_by, r.name),
            })
            .collect())
    }

    /// Restore a deleted page (`wiki-admin`) at the end of its section, or of `section`.
    pub async fn restore_page(
        &self,
        viewer: &Viewer,
        id: i64,
        section: Option<String>,
    ) -> WikiResult<Page> {
        let author = viewer.require_admin()?;
        let mut tx = self.begin().await?;
        let row: Option<(String, String, String, String)> = sqlx::query_as(
            "SELECT slug, section_key, status, visibility FROM wiki_page \
             WHERE id = ? AND deleted_at IS NOT NULL FOR UPDATE",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
        let (slug, old_section, status, visibility) =
            row.ok_or_else(|| WikiError::not_found("no deleted page with this id"))?;
        let section = section.unwrap_or(old_section);
        if !section_exists(&mut tx, &section).await? {
            return Err(WikiError::conflict(format!(
                "section `{section}` no longer exists: restore the page into another `section`"
            )));
        }
        if !slug_free(&mut tx, &slug, None).await? {
            return Err(WikiError::conflict(format!(
                "another page now uses the path `{slug}`: move it first"
            )));
        }
        let order = section_order(&mut tx, &section).await?;
        sqlx::query(
            "UPDATE wiki_page SET deleted_at = NULL, deleted_by = NULL, section_key = ?, \
             position = ?, updated_at = ?, updated_by = ? WHERE id = ?",
        )
        .bind(&section)
        .bind(order.len() as i32 + 1)
        .bind(now())
        .bind(author)
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(|e| {
            if is_duplicate(&e) {
                slug_taken(&slug)
            } else {
                e.into()
            }
        })?;
        bump(&mut tx).await?;
        tx.commit().await?;
        self.changed(WikiChange {
            slug: Some(slug),
            section: Some(section),
            change: "restored".into(),
            public: is_public(
                Status::parse(&status).unwrap_or(Status::Draft),
                Visibility::parse(&visibility).unwrap_or(Visibility::Permission),
            ),
        });
        self.page(viewer, id).await
    }

    // --- revisions ---------------------------------------------------------------------------

    async fn revision_rows(&self, page_id: i64, id: Option<i64>) -> WikiResult<Vec<RevisionRow>> {
        let rows = match id {
            Some(id) => sqlx::query_as(
                "SELECT r.id, r.page_id, r.title, r.summary, r.audience, r.body, r.message, \
                 r.author_id, c.name AS author_name, r.created_at FROM wiki_revision r \
                 LEFT JOIN `character` c ON c.id = r.author_id WHERE r.page_id = ? AND r.id = ?",
            )
            .bind(page_id)
            .bind(id),
            None => sqlx::query_as(
                "SELECT r.id, r.page_id, r.title, r.summary, r.audience, r.body, r.message, \
                 r.author_id, c.name AS author_name, r.created_at FROM wiki_revision r \
                 LEFT JOIN `character` c ON c.id = r.author_id WHERE r.page_id = ? \
                 ORDER BY r.id DESC",
            )
            .bind(page_id),
        }
        .fetch_all(&self.db)
        .await?;
        Ok(rows)
    }

    async fn one_revision(&self, page_id: i64, id: i64) -> WikiResult<RevisionRow> {
        self.revision_rows(page_id, Some(id))
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| WikiError::not_found("revision not found"))
    }

    /// A page's revisions, newest first (`wiki-edit`).
    pub async fn revisions(&self, viewer: &Viewer, id: i64) -> WikiResult<Vec<RevisionSummary>> {
        viewer.require_edit()?;
        let page = self.page_row(viewer, id).await?;
        Ok(self
            .revision_rows(id, None)
            .await?
            .into_iter()
            .map(|r| RevisionSummary {
                current: Some(r.id) == page.revision_id,
                id: r.id,
                title: r.title,
                message: r.message,
                author: character(r.author_id, r.author_name),
                created_at: r.created_at,
                size: r.body.len() as i64,
            })
            .collect())
    }

    /// One revision with its content (`wiki-edit`).
    pub async fn revision(&self, viewer: &Viewer, id: i64, revision: i64) -> WikiResult<Revision> {
        viewer.require_edit()?;
        let page = self.page_row(viewer, id).await?;
        let r = self.one_revision(id, revision).await?;
        Ok(Revision {
            current: Some(r.id) == page.revision_id,
            id: r.id,
            page_id: r.page_id,
            title: r.title,
            summary: r.summary,
            audience: r.audience,
            body: r.body,
            message: r.message,
            author: character(r.author_id, r.author_name),
            created_at: r.created_at,
        })
    }

    /// The diff from one revision to another (default: the current one) (`wiki-edit`).
    pub async fn diff(
        &self,
        viewer: &Viewer,
        id: i64,
        from: i64,
        to: Option<i64>,
    ) -> WikiResult<Diff> {
        viewer.require_edit()?;
        let page = self.page_row(viewer, id).await?;
        let to = to
            .or(page.revision_id)
            .ok_or_else(|| WikiError::not_found("revision not found"))?;
        let old = self.one_revision(id, from).await?;
        let new = self.one_revision(id, to).await?;
        let (lines, insertions, deletions, unified) = crate::diff::diff_lines(&old.body, &new.body);
        Ok(Diff {
            from,
            to,
            from_title: old.title,
            to_title: new.title,
            summary_changed: old.summary != new.summary || old.audience != new.audience,
            from_summary: old.summary,
            to_summary: new.summary,
            from_for: old.audience,
            to_for: new.audience,
            insertions,
            deletions,
            unified,
            lines,
        })
    }

    /// Make an old revision's content current again, as a new revision (`wiki-edit`).
    pub async fn revert(
        &self,
        viewer: &Viewer,
        checks: &Checks,
        id: i64,
        revision: i64,
        base: Option<i64>,
        message: Option<String>,
    ) -> WikiResult<Page> {
        viewer.require_edit()?;
        let current = self.page_row(viewer, id).await?;
        let old = self.one_revision(id, revision).await?;
        let message = match message.filter(|m| !m.trim().is_empty()) {
            Some(m) => m,
            None => format!("Reverted to revision {revision}"),
        };
        self.update_page(
            viewer,
            checks,
            id,
            PageUpdate {
                base_revision: base.or(current.revision_id),
                title: Some(old.title),
                summary: Some(old.summary),
                audience: Some(old.audience),
                body: Some(old.body),
                message: Some(message),
                ..Default::default()
            },
        )
        .await
    }

    // --- sections ----------------------------------------------------------------------------

    async fn section_view(&self, viewer: &Viewer, key: &str) -> WikiResult<Section> {
        self.sections(viewer)
            .await?
            .into_iter()
            .find(|s| s.key == key)
            .ok_or_else(|| WikiError::not_found("section not found"))
    }

    async fn locked_section_keys(tx: &mut Tx) -> WikiResult<Vec<String>> {
        Ok(
            sqlx::query_scalar(
                "SELECT `key` FROM wiki_section ORDER BY position, `key` FOR UPDATE",
            )
            .fetch_all(&mut **tx)
            .await?,
        )
    }

    async fn renumber_sections(tx: &mut Tx, keys: &[String]) -> WikiResult<()> {
        for (i, key) in keys.iter().enumerate() {
            sqlx::query("UPDATE wiki_section SET position = ? WHERE `key` = ?")
                .bind(i as i32 + 1)
                .bind(key)
                .execute(&mut **tx)
                .await?;
        }
        Ok(())
    }

    fn section_changed(&self, key: &str) {
        self.changed(WikiChange {
            slug: None,
            section: Some(key.to_string()),
            change: "section".into(),
            public: true,
        });
    }

    /// Create a section (`wiki-admin`).
    pub async fn create_section(&self, viewer: &Viewer, new: NewSection) -> WikiResult<Section> {
        viewer.require_admin()?;
        validate_section_key(&new.key).map_err(WikiError::bad_request)?;
        let label = check_label(&new.label)?;
        let mut tx = self.begin().await?;
        let mut keys = Self::locked_section_keys(&mut tx).await?;
        if keys.contains(&new.key) {
            return Err(WikiError::conflict(format!(
                "section `{}` already exists",
                new.key
            )));
        }
        let at = now();
        sqlx::query(
            "INSERT INTO wiki_section (`key`, label, position, listed, created_at, updated_at) \
             VALUES (?, ?, 0, ?, ?, ?)",
        )
        .bind(&new.key)
        .bind(&label)
        .bind(new.listed.unwrap_or(true))
        .bind(at)
        .bind(at)
        .execute(&mut *tx)
        .await
        .map_err(|e| {
            if is_duplicate(&e) {
                WikiError::conflict(format!("section `{}` already exists", new.key))
            } else {
                e.into()
            }
        })?;
        let index = match new.position {
            Some(p) => (p.max(1) as usize - 1).min(keys.len()),
            None => keys.len(),
        };
        keys.insert(index, new.key.clone());
        Self::renumber_sections(&mut tx, &keys).await?;
        bump(&mut tx).await?;
        tx.commit().await?;
        self.section_changed(&new.key);
        self.section_view(viewer, &new.key).await
    }

    /// Rename a section or list / unlist it (`wiki-admin`).
    pub async fn update_section(
        &self,
        viewer: &Viewer,
        key: &str,
        update: SectionUpdate,
    ) -> WikiResult<Section> {
        viewer.require_admin()?;
        let label = update.label.as_deref().map(check_label).transpose()?;
        let done = sqlx::query(
            "UPDATE wiki_section SET label = COALESCE(?, label), listed = COALESCE(?, listed), \
             updated_at = ? WHERE `key` = ?",
        )
        .bind(label)
        .bind(update.listed)
        .bind(now())
        .bind(key)
        .execute(&self.db)
        .await?;
        if done.rows_affected() == 0 {
            // MySQL counts only changed rows: tell "unchanged" from "missing".
            self.section_view(viewer, key).await?;
        }
        let mut tx = self.begin().await?;
        bump(&mut tx).await?;
        tx.commit().await?;
        self.section_changed(key);
        self.section_view(viewer, key).await
    }

    /// Put the sections in this order (`wiki-admin`); `keys` lists every section once.
    pub async fn order_sections(
        &self,
        viewer: &Viewer,
        keys: Vec<String>,
    ) -> WikiResult<Vec<Section>> {
        viewer.require_admin()?;
        let mut tx = self.begin().await?;
        let current = Self::locked_section_keys(&mut tx).await?;
        let mut given = keys.clone();
        given.sort();
        let mut existing = current.clone();
        existing.sort();
        if given != existing {
            return Err(WikiError::bad_request(format!(
                "list every section exactly once (sections: {})",
                current.join(", ")
            )));
        }
        Self::renumber_sections(&mut tx, &keys).await?;
        bump(&mut tx).await?;
        tx.commit().await?;
        self.section_changed(keys.first().map(String::as_str).unwrap_or_default());
        self.sections(viewer).await
    }

    /// Delete an empty section (`wiki-admin`); a section with (live) pages is a conflict.
    pub async fn delete_section(&self, viewer: &Viewer, key: &str) -> WikiResult<()> {
        viewer.require_admin()?;
        let mut tx = self.begin().await?;
        let mut keys = Self::locked_section_keys(&mut tx).await?;
        if !keys.iter().any(|k| k == key) {
            return Err(WikiError::not_found("section not found"));
        }
        let pages = section_order(&mut tx, key).await?;
        if !pages.is_empty() {
            return Err(WikiError::conflict(format!(
                "section `{key}` still has {} page(s): move or delete them first",
                pages.len()
            )));
        }
        sqlx::query("DELETE FROM wiki_section WHERE `key` = ?")
            .bind(key)
            .execute(&mut *tx)
            .await?;
        keys.retain(|k| k != key);
        Self::renumber_sections(&mut tx, &keys).await?;
        bump(&mut tx).await?;
        tx.commit().await?;
        self.section_changed(key);
        Ok(())
    }

    // --- search ------------------------------------------------------------------------------

    async fn search_index(&self) -> WikiResult<Arc<SearchIndex>> {
        let generation: i64 = sqlx::query_scalar("SELECT generation FROM wiki_state WHERE id = 1")
            .fetch_optional(&self.db)
            .await?
            .unwrap_or_default();
        if let Some(index) = self.search.get(generation) {
            return Ok(index);
        }
        let sections = self.section_rows().await?;
        let labels: HashMap<&str, &str> = sections
            .iter()
            .map(|s| (s.key.as_str(), s.label.as_str()))
            .collect();
        #[derive(FromRow)]
        struct Row {
            id: i64,
            slug: String,
            section_key: String,
            title: String,
            summary: String,
            body: String,
            status: String,
            visibility: String,
            permission: Option<String>,
        }
        let rows: Vec<Row> = sqlx::query_as(
            "SELECT id, slug, section_key, title, summary, body, status, visibility, permission \
             FROM wiki_page WHERE deleted_at IS NULL",
        )
        .fetch_all(&self.db)
        .await?;
        let docs = rows
            .into_iter()
            .map(|r| {
                let analysis = analyze(&r.body, None);
                Document {
                    section: SectionRef {
                        label: labels
                            .get(r.section_key.as_str())
                            .map(|l| l.to_string())
                            .unwrap_or_else(|| r.section_key.clone()),
                        key: r.section_key,
                    },
                    id: r.id,
                    slug: r.slug,
                    title: r.title,
                    summary: r.summary,
                    status: Status::parse(&r.status).unwrap_or(Status::Draft),
                    visibility: Visibility::parse(&r.visibility).unwrap_or(Visibility::Permission),
                    permission: r.permission,
                    headings: analysis
                        .headings
                        .iter()
                        .map(|h| h.text.as_str())
                        .collect::<Vec<_>>()
                        .join("\n"),
                    body_text: analysis.plain_text,
                }
            })
            .collect();
        let index = Arc::new(SearchIndex::build(docs));
        self.search.put(generation, index.clone());
        Ok(index)
    }

    /// Ranked full-text search over the pages the viewer may see (at most 50 hits).
    pub async fn search(
        &self,
        viewer: &Viewer,
        query: &str,
        limit: usize,
    ) -> WikiResult<SearchResults> {
        let query: String = query.chars().take(200).collect();
        let index = self.search_index().await?;
        let hits = index.search(&query, limit.clamp(1, MAX_SEARCH_HITS), |d| {
            viewer.can_view(d.visibility, d.permission.as_deref(), d.status)
        });
        Ok(SearchResults { query, hits })
    }
}

/// Validate a body without saving it.
pub fn validate(body: &str, checks: &Checks) -> Validation {
    let analysis = analyze(body, checks.fit_names.as_ref());
    let mut problems = analysis.problems.clone();
    if body.len() > MAX_BODY_BYTES {
        problems.insert(
            0,
            crate::Problem {
                line: 1,
                code: "too_large".into(),
                message: format!("a page body is at most {} KiB", MAX_BODY_BYTES / 1024),
            },
        );
    }
    Validation {
        valid: problems.is_empty(),
        problems,
        read_minutes: analysis.read_minutes(),
        words: analysis.words,
        headings: analysis.headings,
        links: analysis.links,
    }
}

fn build_index(
    viewer: &Viewer,
    sections: &[SectionRow],
    rows: &[ListRow],
    drafts: bool,
) -> Vec<IndexSection> {
    let mut out = Vec::new();
    for section in sections.iter().filter(|s| s.listed) {
        let pages: Vec<&ListRow> = rows
            .iter()
            .filter(|r| {
                r.section_key == section.key
                    && r.visible_to(viewer)
                    && (r.status() == Status::Published || (drafts && viewer.can_edit()))
            })
            .collect();
        if pages.is_empty() {
            continue;
        }
        let num = out.len() + 1;
        out.push(IndexSection {
            key: section.key.clone(),
            label: section.label.clone(),
            num: num.to_string(),
            pages: pages
                .iter()
                .enumerate()
                .map(|(i, r)| r.to_index_page(Some(format!("{num}.{}", i + 1))))
                .collect(),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placing() {
        assert_eq!(place(vec![1, 2, 3], 9, None), vec![1, 2, 3, 9]);
        assert_eq!(place(vec![1, 2, 3], 9, Some(1)), vec![9, 1, 2, 3]);
        assert_eq!(place(vec![1, 2, 3], 9, Some(0)), vec![9, 1, 2, 3]);
        assert_eq!(place(vec![1, 2, 3], 9, Some(99)), vec![1, 2, 3, 9]);
        assert_eq!(place(vec![1, 2, 3], 1, Some(3)), vec![2, 3, 1]);
    }
}
