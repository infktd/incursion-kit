//! Importing a static-site wiki: Markdown / MDX files with YAML frontmatter.
//!
//! The directory (`waitlist wiki import <dir>`) may contain:
//!
//! ```text
//! wiki/<path>.md(x)            pages; the path without extension is the page's slug
//! wiki-sections/<key>.yaml     sections: `label`, `order`, optionally `listed`
//! fc-cheatsheets/<name>.md(x)  FC cheatsheets: pages `fc-cheatsheets/<name>` in the unlisted
//!                              section `fc-cheatsheets`, readable with `fleet-view`
//! ```
//!
//! Page frontmatter: `title` (required), `section` (required for wiki pages), `order`,
//! `summary`, `for`, and optionally `status` (default `published`), `visibility`, `permission`,
//! `updated` (a date: the revision's time) and `editor` (named in the revision message). MDX
//! components are converted to directives ([`crate::mdx`]).
//!
//! [`plan`] reads and checks everything without touching the database; [`Wiki::import`] then
//! writes all of it in one transaction, or nothing when the plan has errors. Importing again is
//! safe: unchanged pages are left alone, changed ones get a new revision ("Imported from …").
//! Doctrine fit names are not checked here (the importer doesn't load the fit data); the editor
//! checks them on the next save.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;
use sqlx::FromRow;

use crate::markdown::analyze;
use crate::model::{Status, Visibility};
use crate::slug::{validate_section_key, validate_slug};
use crate::store::{
    Content, NewRow, bump, check_audience, check_label, check_summary, check_title, insert_page,
    save_content,
};
use crate::{Wiki, WikiChange, WikiResult, now};

/// The section FC cheatsheets are imported into.
pub const CHEATSHEET_SECTION: &str = "fc-cheatsheets";
/// Who may read imported cheatsheets (the FC console's permission).
pub const CHEATSHEET_PERMISSION: &str = "fleet-view";

const PAGE_KEYS: &[&str] = &[
    "title",
    "section",
    "order",
    "summary",
    "for",
    "updated",
    "editor",
    "status",
    "visibility",
    "permission",
];

/// A problem with one file. `line` is 1-based in the file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImportProblem {
    pub file: String,
    pub line: Option<usize>,
    pub message: String,
}

impl std::fmt::Display for ImportProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.line {
            Some(line) => write!(f, "{}:{line}: {}", self.file, self.message),
            None => write!(f, "{}: {}", self.file, self.message),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedSection {
    pub key: String,
    pub label: String,
    pub position: i32,
    pub listed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedPage {
    /// The file, relative to the import directory.
    pub source: String,
    pub slug: String,
    pub section: String,
    pub position: i32,
    pub title: String,
    pub summary: String,
    pub audience: Option<String>,
    pub body: String,
    pub status: Status,
    pub visibility: Visibility,
    pub permission: Option<String>,
    pub updated_at: Option<i64>,
    pub editor: Option<String>,
}

/// Everything read from an import directory.
#[derive(Debug, Clone, Default)]
pub struct ImportPlan {
    pub sections: Vec<PlannedSection>,
    pub pages: Vec<PlannedPage>,
    pub errors: Vec<ImportProblem>,
    pub warnings: Vec<ImportProblem>,
}

/// What an import did (or, with `dry_run`, would do).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ImportReport {
    pub dry_run: bool,
    pub sections_created: usize,
    pub sections_updated: usize,
    pub sections_unchanged: usize,
    pub pages_created: usize,
    pub pages_updated: usize,
    pub pages_unchanged: usize,
    pub errors: Vec<ImportProblem>,
    pub warnings: Vec<ImportProblem>,
}

fn files(dir: &Path, extensions: &[&str], recursive: bool, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            if recursive {
                files(&path, extensions, recursive, out);
            }
        } else if path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| extensions.contains(&e))
        {
            out.push(path);
        }
    }
    out.sort();
}

/// `(frontmatter, body, line of the body's first line)`.
fn split_frontmatter(text: &str) -> Result<(Option<&str>, &str, usize), String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let Some(rest) = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))
    else {
        return Ok((None, text, 1));
    };
    let mut offset = 0;
    for (i, line) in rest.split_inclusive('\n').enumerate() {
        if matches!(line.trim_end(), "---" | "...") {
            let yaml = &rest[..offset];
            let body = &rest[offset + line.len()..];
            return Ok((Some(yaml), body, i + 3));
        }
        offset += line.len();
    }
    Err("the frontmatter (`---`) is never closed".into())
}

fn text(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn integer(value: &Value) -> Option<i32> {
    match value {
        Value::Number(n) => n.as_i64().and_then(|n| i32::try_from(n).ok()),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// `2026-10-01` or an RFC 3339 time → Unix seconds.
fn date(value: &str) -> Option<i64> {
    let value = value.trim();
    if let Ok(t) = chrono::DateTime::parse_from_rfc3339(value) {
        return Some(t.timestamp());
    }
    chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .ok()
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map(|t| t.and_utc().timestamp())
}

fn parse_yaml(yaml: &str) -> Result<BTreeMap<String, Value>, String> {
    if yaml.trim().is_empty() {
        return Ok(BTreeMap::new());
    }
    let value: Value = serde_saphyr::from_str(yaml).map_err(|e| format!("invalid YAML: {e}"))?;
    match value {
        Value::Object(map) => Ok(map.into_iter().collect()),
        Value::Null => Ok(BTreeMap::new()),
        _ => Err("the YAML must be a mapping of keys to values".into()),
    }
}

struct Reader<'a> {
    root: &'a Path,
    plan: ImportPlan,
}

impl Reader<'_> {
    fn rel(&self, path: &Path) -> String {
        path.strip_prefix(self.root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/")
    }

    fn error(&mut self, file: &str, line: Option<usize>, message: impl Into<String>) {
        self.plan.errors.push(ImportProblem {
            file: file.to_string(),
            line,
            message: message.into(),
        });
    }

    fn warn(&mut self, file: &str, message: impl Into<String>) {
        self.plan.warnings.push(ImportProblem {
            file: file.to_string(),
            line: None,
            message: message.into(),
        });
    }

    fn section(&mut self, path: &Path) {
        let file = self.rel(path);
        let key = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_string();
        if let Err(e) = validate_section_key(&key) {
            return self.error(&file, None, e);
        }
        let map = match std::fs::read_to_string(path)
            .map_err(|e| format!("could not read: {e}"))
            .and_then(|t| parse_yaml(&t))
        {
            Ok(map) => map,
            Err(e) => return self.error(&file, None, e),
        };
        let Some(label) = map.get("label").and_then(text) else {
            return self.error(&file, None, "`label` is missing");
        };
        let label = match check_label(&label) {
            Ok(l) => l,
            Err(e) => return self.error(&file, None, e.message),
        };
        let position = match map.get("order") {
            None => 0,
            Some(v) => match integer(v) {
                Some(n) => n,
                None => return self.error(&file, None, "`order` must be a whole number"),
            },
        };
        let listed = match map.get("listed") {
            None => key != CHEATSHEET_SECTION,
            Some(Value::Bool(b)) => *b,
            Some(_) => return self.error(&file, None, "`listed` must be true or false"),
        };
        for unknown in map
            .keys()
            .filter(|k| !["label", "order", "listed"].contains(&k.as_str()))
        {
            let message = format!("unknown key `{unknown}` ignored");
            self.warn(&file, message);
        }
        self.plan.sections.push(PlannedSection {
            key,
            label,
            position,
            listed,
        });
    }

    fn page(&mut self, path: &Path, slug: String, cheatsheet: bool) {
        let file = self.rel(path);
        let raw = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) => return self.error(&file, None, format!("could not read: {e}")),
        };
        let (yaml, body, body_line) = match split_frontmatter(&raw) {
            Ok(parts) => parts,
            Err(e) => return self.error(&file, Some(1), e),
        };
        let map = match parse_yaml(yaml.unwrap_or_default()) {
            Ok(map) => map,
            Err(e) => return self.error(&file, Some(2), e),
        };
        let errors_before = self.plan.errors.len();
        let field_error = |reader: &mut Self, message: String| reader.error(&file, None, message);

        let title = match map.get("title").and_then(text).map(|t| check_title(&t)) {
            Some(Ok(t)) => t,
            Some(Err(e)) => {
                field_error(self, format!("title: {}", e.message));
                String::new()
            }
            None => {
                field_error(self, "`title` is missing".into());
                String::new()
            }
        };
        let section = match map.get("section").and_then(text) {
            Some(s) if !s.trim().is_empty() => s.trim().to_string(),
            _ if cheatsheet => CHEATSHEET_SECTION.to_string(),
            _ => {
                field_error(self, "`section` is missing".into());
                String::new()
            }
        };
        if !section.is_empty()
            && let Err(e) = validate_section_key(&section)
        {
            field_error(self, e);
        }
        let position = match map.get("order") {
            None => 0,
            Some(v) => integer(v).unwrap_or_else(|| {
                field_error(self, "`order` must be a whole number".into());
                0
            }),
        };
        let summary = map
            .get("summary")
            .and_then(text)
            .map(|s| check_summary(&s))
            .transpose()
            .unwrap_or_else(|e| {
                field_error(self, format!("summary: {}", e.message));
                None
            })
            .unwrap_or_default();
        let audience =
            check_audience(map.get("for").and_then(text).as_deref()).unwrap_or_else(|e| {
                field_error(self, e.message);
                None
            });
        let status = match map.get("status").and_then(text) {
            None => Status::Published,
            Some(s) => Status::parse(s.trim()).unwrap_or_else(|| {
                field_error(self, format!("unknown status `{s}` (draft or published)"));
                Status::Draft
            }),
        };
        let (default_visibility, default_permission) = if cheatsheet {
            (
                Visibility::Permission,
                Some(CHEATSHEET_PERMISSION.to_string()),
            )
        } else {
            (Visibility::Public, None)
        };
        let visibility = match map.get("visibility").and_then(text) {
            None => default_visibility,
            Some(v) => Visibility::parse(v.trim()).unwrap_or_else(|| {
                field_error(
                    self,
                    format!("unknown visibility `{v}` (public, login or permission)"),
                );
                Visibility::Public
            }),
        };
        let permission = match map.get("permission").and_then(text) {
            Some(p) if !p.trim().is_empty() => Some(p.trim().to_string()),
            _ if visibility == Visibility::Permission && !map.contains_key("visibility") => {
                default_permission
            }
            _ => None,
        };
        if visibility == Visibility::Permission && permission.is_none() {
            field_error(
                self,
                "visibility `permission` needs a `permission` key".into(),
            );
        }
        if visibility != Visibility::Permission && permission.is_some() {
            field_error(
                self,
                "`permission` is only used with visibility `permission`".into(),
            );
        }
        let updated_at = match map.get("updated").and_then(text) {
            Some(d) if !d.trim().is_empty() => {
                let parsed = date(&d);
                if parsed.is_none() {
                    let message = format!("`updated: {d}` is not a date (YYYY-MM-DD); ignored");
                    self.warn(&file, message);
                }
                parsed
            }
            _ => None,
        };
        let editor = map
            .get("editor")
            .and_then(text)
            .map(|e| e.trim().to_string())
            .filter(|e| !e.is_empty());
        for unknown in map.keys().filter(|k| !PAGE_KEYS.contains(&k.as_str())) {
            let message = format!("unknown frontmatter key `{unknown}` ignored");
            self.warn(&file, message);
        }

        let mdx = path.extension().is_some_and(|e| e == "mdx");
        let body = if mdx {
            match crate::mdx::convert(body) {
                Ok(b) => b,
                Err(e) => {
                    return self.error(&file, Some(body_line + e.line - 1), e.message);
                }
            }
        } else {
            body.to_string()
        };
        let body = body.trim_start_matches(['\n', '\r']).to_string();
        let analysis = analyze(&body, None);
        for p in &analysis.problems {
            let message = format!("{} (line {} of the converted page)", p.message, p.line);
            self.error(&file, None, message);
        }
        if body.len() > crate::MAX_BODY_BYTES {
            self.error(&file, None, "the page is larger than 512 KiB");
        }
        if self.plan.errors.len() > errors_before {
            return;
        }
        self.plan.pages.push(PlannedPage {
            source: file,
            slug,
            section,
            position,
            title,
            summary,
            audience,
            body,
            status,
            visibility,
            permission,
            updated_at,
            editor,
        });
    }
}

/// Read and check an import directory (see the module docs). Nothing is written.
pub fn plan(dir: &Path) -> ImportPlan {
    let mut reader = Reader {
        root: dir,
        plan: ImportPlan::default(),
    };
    let wiki = dir.join("wiki");
    let sections = dir.join("wiki-sections");
    let cheatsheets = dir.join("fc-cheatsheets");
    if !wiki.is_dir() && !sections.is_dir() && !cheatsheets.is_dir() {
        reader.error(
            &dir.display().to_string(),
            None,
            "no wiki/, wiki-sections/ or fc-cheatsheets/ directory here",
        );
        return reader.plan;
    }

    let mut section_files = Vec::new();
    files(&sections, &["yaml", "yml"], false, &mut section_files);
    for path in &section_files {
        reader.section(path);
    }

    let mut seen = BTreeSet::new();
    let mut page_files = Vec::new();
    files(&wiki, &["md", "mdx"], true, &mut page_files);
    for path in &page_files {
        let rel = path.strip_prefix(&wiki).unwrap_or(path).with_extension("");
        let slug = rel.to_string_lossy().replace('\\', "/");
        let file = reader.rel(path);
        if let Err(e) = validate_slug(&slug) {
            reader.error(&file, None, e);
            continue;
        }
        if !seen.insert(slug.clone()) {
            reader.error(&file, None, format!("another file is also page `{slug}`"));
            continue;
        }
        reader.page(path, slug, false);
    }

    let mut sheet_files = Vec::new();
    files(&cheatsheets, &["md", "mdx"], false, &mut sheet_files);
    for path in &sheet_files {
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        let slug = format!("{CHEATSHEET_SECTION}/{stem}");
        let file = reader.rel(path);
        if let Err(e) = validate_slug(&slug) {
            reader.error(&file, None, e);
            continue;
        }
        if !seen.insert(slug.clone()) {
            reader.error(&file, None, format!("another file is also page `{slug}`"));
            continue;
        }
        reader.page(path, slug, true);
    }
    if !sheet_files.is_empty()
        && !reader
            .plan
            .sections
            .iter()
            .any(|s| s.key == CHEATSHEET_SECTION)
    {
        let position = reader
            .plan
            .sections
            .iter()
            .map(|s| s.position)
            .max()
            .unwrap_or(0)
            + 1;
        reader.plan.sections.push(PlannedSection {
            key: CHEATSHEET_SECTION.into(),
            label: "FC cheatsheets".into(),
            position,
            listed: false,
        });
    }
    reader.plan
}

#[derive(FromRow)]
struct Existing {
    id: i64,
    section_key: String,
    position: i32,
    title: String,
    summary: String,
    audience: Option<String>,
    body: String,
    status: String,
    visibility: String,
    permission: Option<String>,
}

fn title_case(key: &str) -> String {
    key.split(['-', '_'])
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut c = w.chars();
            c.next()
                .map(|f| f.to_uppercase().chain(c).collect::<String>())
                .unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

impl Wiki {
    /// Write an import plan in one transaction (rolled back with `dry_run`). A plan with
    /// errors writes nothing.
    pub async fn import(&self, plan: &ImportPlan, dry_run: bool) -> WikiResult<ImportReport> {
        let mut report = ImportReport {
            dry_run,
            errors: plan.errors.clone(),
            warnings: plan.warnings.clone(),
            ..Default::default()
        };
        if !report.errors.is_empty() {
            return Ok(report);
        }
        let mut tx = self.db.begin().await?;
        let at = now();

        let existing: Vec<(String, String, i32, bool)> =
            sqlx::query_as("SELECT `key`, label, position, listed FROM wiki_section FOR UPDATE")
                .fetch_all(&mut *tx)
                .await?;
        let mut known: BTreeSet<String> = existing.iter().map(|s| s.0.clone()).collect();
        let mut next_position = existing
            .iter()
            .map(|s| s.2)
            .max()
            .unwrap_or(0)
            .max(plan.sections.iter().map(|s| s.position).max().unwrap_or(0));
        let mut sections = plan.sections.clone();
        for page in &plan.pages {
            if !known.contains(&page.section) && !sections.iter().any(|s| s.key == page.section) {
                next_position += 1;
                report.warnings.push(ImportProblem {
                    file: page.source.clone(),
                    line: None,
                    message: format!(
                        "section `{}` has no wiki-sections/{}.yaml: created as \"{}\"",
                        page.section,
                        page.section,
                        title_case(&page.section)
                    ),
                });
                sections.push(PlannedSection {
                    key: page.section.clone(),
                    label: title_case(&page.section),
                    position: next_position,
                    listed: true,
                });
            }
        }
        for s in &sections {
            match existing.iter().find(|e| e.0 == s.key) {
                Some(e) if e.1 == s.label && e.2 == s.position && e.3 == s.listed => {
                    report.sections_unchanged += 1;
                }
                Some(_) => {
                    sqlx::query(
                        "UPDATE wiki_section SET label = ?, position = ?, listed = ?, \
                         updated_at = ? WHERE `key` = ?",
                    )
                    .bind(&s.label)
                    .bind(s.position)
                    .bind(s.listed)
                    .bind(at)
                    .bind(&s.key)
                    .execute(&mut *tx)
                    .await?;
                    report.sections_updated += 1;
                }
                None => {
                    sqlx::query(
                        "INSERT INTO wiki_section (`key`, label, position, listed, created_at, \
                         updated_at) VALUES (?, ?, ?, ?, ?, ?)",
                    )
                    .bind(&s.key)
                    .bind(&s.label)
                    .bind(s.position)
                    .bind(s.listed)
                    .bind(at)
                    .bind(at)
                    .execute(&mut *tx)
                    .await?;
                    known.insert(s.key.clone());
                    report.sections_created += 1;
                }
            }
        }

        for page in &plan.pages {
            let when = page.updated_at.unwrap_or(at);
            let message = match &page.editor {
                Some(editor) => format!("Imported from {} (editor: {editor})", page.source),
                None => format!("Imported from {}", page.source),
            };
            let message: String = message.chars().take(255).collect();
            let links = analyze(&page.body, None).links;
            let content = Content {
                title: &page.title,
                summary: &page.summary,
                audience: page.audience.as_deref(),
                body: &page.body,
                message: &message,
                author: None,
                at: when,
            };
            let row: Option<Existing> = sqlx::query_as(
                "SELECT id, section_key, position, title, summary, audience, body, status, \
                 visibility, permission FROM wiki_page WHERE live_slug = ? FOR UPDATE",
            )
            .bind(&page.slug)
            .fetch_optional(&mut *tx)
            .await?;
            match row {
                None => {
                    let id = insert_page(
                        &mut tx,
                        &NewRow {
                            slug: &page.slug,
                            section: &page.section,
                            status: page.status,
                            visibility: page.visibility,
                            permission: page.permission.as_deref(),
                            position: page.position,
                        },
                        &page.title,
                        None,
                        when,
                    )
                    .await?;
                    save_content(&mut tx, id, &content, &links).await?;
                    report.pages_created += 1;
                }
                Some(e) => {
                    let content_changed = e.title != page.title
                        || e.summary != page.summary
                        || e.audience != page.audience
                        || e.body != page.body;
                    let meta_changed = e.section_key != page.section
                        || e.position != page.position
                        || e.status != page.status.as_str()
                        || e.visibility != page.visibility.as_str()
                        || e.permission != page.permission;
                    if content_changed {
                        save_content(&mut tx, e.id, &Content { at, ..content }, &links).await?;
                    }
                    if meta_changed {
                        sqlx::query(
                            "UPDATE wiki_page SET section_key = ?, position = ?, status = ?, \
                             visibility = ?, permission = ?, updated_at = ?, updated_by = NULL, \
                             published_at = IF(? = 'published', COALESCE(published_at, ?), \
                             published_at) WHERE id = ?",
                        )
                        .bind(&page.section)
                        .bind(page.position)
                        .bind(page.status.as_str())
                        .bind(page.visibility.as_str())
                        .bind(page.permission.as_deref())
                        .bind(at)
                        .bind(page.status.as_str())
                        .bind(at)
                        .bind(e.id)
                        .execute(&mut *tx)
                        .await?;
                    }
                    if content_changed || meta_changed {
                        report.pages_updated += 1;
                    } else {
                        report.pages_unchanged += 1;
                    }
                }
            }
        }

        let changed = report.sections_created
            + report.sections_updated
            + report.pages_created
            + report.pages_updated
            > 0;
        if dry_run {
            tx.rollback().await?;
        } else {
            if changed {
                bump(&mut tx).await?;
            }
            tx.commit().await?;
            if changed {
                self.changed(WikiChange {
                    slug: None,
                    section: None,
                    change: "imported".into(),
                    public: true,
                });
            }
        }
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontmatter() {
        let (yaml, body, line) = split_frontmatter("---\ntitle: X\n---\n\nBody\n").unwrap();
        assert_eq!(yaml, Some("title: X\n"));
        assert_eq!(body, "\nBody\n");
        assert_eq!(line, 4);
        assert_eq!(split_frontmatter("Body").unwrap(), (None, "Body", 1));
        assert!(split_frontmatter("---\ntitle: X\n").is_err());
        assert_eq!(date("2026-10-01"), Some(1_790_812_800));
        assert_eq!(title_case("fc-team"), "Fc Team");
    }

    #[test]
    fn plans_a_directory() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("wiki/guides")).unwrap();
        std::fs::create_dir_all(root.join("wiki-sections")).unwrap();
        std::fs::create_dir_all(root.join("fc-cheatsheets")).unwrap();
        std::fs::write(
            root.join("wiki-sections/start.yaml"),
            "label: Start\norder: 1\n",
        )
        .unwrap();
        std::fs::write(
            root.join("wiki/guides/a.mdx"),
            "---\ntitle: A\nsection: start\norder: 2\nsummary: >-\n  long\n  text\nfor: All\nupdated: 2026-10-01\nextra: 1\n---\n<Callout>\nHi\n</Callout>\n",
        )
        .unwrap();
        std::fs::write(
            root.join("wiki/guides/bad.mdx"),
            "---\ntitle: B\nsection: start\n---\n\n<Tabs>\n</Tabs>\n",
        )
        .unwrap();
        std::fs::write(
            root.join("fc-cheatsheets/x.mdx"),
            "---\ntitle: X\norder: 1\n---\nText\n",
        )
        .unwrap();
        let plan = plan(root);
        assert_eq!(plan.errors.len(), 1, "{:?}", plan.errors);
        assert_eq!(plan.errors[0].file, "wiki/guides/bad.mdx");
        assert_eq!(plan.errors[0].line, Some(6));
        assert_eq!(plan.warnings.len(), 1);
        let a = &plan.pages[0];
        assert_eq!(a.slug, "guides/a");
        assert_eq!(a.summary, "long text");
        assert_eq!(a.audience.as_deref(), Some("All"));
        assert_eq!(a.body, ":::callout\nHi\n:::\n");
        assert_eq!(a.updated_at, Some(1_790_812_800));
        let x = &plan.pages[1];
        assert_eq!(x.slug, "fc-cheatsheets/x");
        assert_eq!(x.visibility, Visibility::Permission);
        assert_eq!(x.permission.as_deref(), Some(CHEATSHEET_PERMISSION));
        assert!(
            plan.sections
                .iter()
                .any(|s| s.key == CHEATSHEET_SECTION && !s.listed)
        );
    }
}
