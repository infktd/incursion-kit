//! Uploaded images: metadata in `wiki_asset`, bytes in `<data>/wiki/assets/<id>.<ext>`.
//!
//! Uploads go through the same checks as guide images (`wl-content`): PNG, JPEG, GIF, WebP or
//! SVG, at most 10 MiB, and images over 1 MiB are resized (longest edge 1600 px) and re-encoded,
//! an opaque PNG becoming a JPEG. Files are named by id, so a URL never changes meaning; an
//! upload with the same file name is a new image.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use sqlx::FromRow;

use crate::model::{Asset, CharacterRef, PageLink, Status, Visibility};
use crate::{ErrorKind, Viewer, Wiki, WikiError, WikiResult, now};

/// The path prefix of image URLs (see [`asset_url`]).
const URL_PREFIX: &str = "/api/wiki/assets/";

/// Largest accepted upload.
pub const MAX_ASSET_BYTES: usize = 10 * 1024 * 1024;
/// Images larger than this are optimized on upload.
const OPTIMIZE_MIN_BYTES: usize = 1024 * 1024;

#[derive(Debug, FromRow)]
struct Row {
    id: i64,
    filename: String,
    content_type: String,
    size: i64,
    stored_name: String,
    uploaded_by: Option<i64>,
    name: Option<String>,
    uploaded_at: i64,
}

impl Row {
    fn into_asset(self) -> Asset {
        Asset {
            url: asset_url(self.id, &self.filename),
            id: self.id,
            filename: self.filename,
            content_type: self.content_type,
            size: self.size,
            uploaded_at: self.uploaded_at,
            uploaded_by: self.uploaded_by.map(|id| CharacterRef {
                id,
                name: self.name.unwrap_or_else(|| format!("#{id}")),
            }),
            used_by: Vec::new(),
        }
    }
}

/// The ids of the images a page body references: every `/api/wiki/assets/{id}/` in the text
/// (Markdown images, links, MDX attributes; relative or absolute URLs).
pub fn referenced_assets(body: &str) -> BTreeSet<i64> {
    body.match_indices(URL_PREFIX)
        .filter_map(|(start, _)| {
            let rest = &body[start + URL_PREFIX.len()..];
            let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
            (digits > 0 && rest[digits..].starts_with('/'))
                .then(|| rest[..digits].parse().ok())
                .flatten()
        })
        .collect()
}

/// A live page whose body references an image.
#[derive(Debug, FromRow)]
struct UsageRow {
    slug: String,
    title: String,
    status: String,
    visibility: String,
    permission: Option<String>,
    body: String,
}

/// The pages using each image: every page, and the ones `viewer` may see.
#[derive(Debug, Default)]
struct Usage {
    total: BTreeMap<i64, usize>,
    visible: BTreeMap<i64, Vec<PageLink>>,
}

impl Usage {
    fn count(&self, id: i64) -> usize {
        self.total.get(&id).copied().unwrap_or_default()
    }

    fn visible(&self, id: i64) -> Vec<PageLink> {
        self.visible.get(&id).cloned().unwrap_or_default()
    }
}

/// `/api/wiki/assets/{id}/{filename}`.
pub fn asset_url(id: i64, filename: &str) -> String {
    let encoded: String = filename
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect();
    format!("/api/wiki/assets/{id}/{encoded}")
}

const SELECT: &str = "SELECT a.id, a.filename, a.content_type, a.size, a.stored_name, \
                      a.uploaded_by, c.name, a.uploaded_at FROM wiki_asset a \
                      LEFT JOIN `character` c ON c.id = a.uploaded_by";

fn io_error(e: impl std::fmt::Display) -> WikiError {
    WikiError::internal(e.to_string())
}

/// Validate, maybe optimize: the bytes and file name to store.
fn prepare(filename: &str, data: &[u8]) -> WikiResult<(Vec<u8>, String, &'static str)> {
    let filename = filename.trim();
    if filename.chars().count() > 255 {
        return Err(WikiError::bad_request(
            "a file name is at most 255 characters",
        ));
    }
    wl_content::asset_mime_type(filename).map_err(|e| WikiError::bad_request(e.0))?;
    if data.is_empty() {
        return Err(WikiError::bad_request("the image is empty"));
    }
    if data.len() > MAX_ASSET_BYTES {
        return Err(WikiError::new(
            ErrorKind::TooLarge,
            "images are at most 10 MiB",
        ));
    }
    let (bytes, name) = if data.len() > OPTIMIZE_MIN_BYTES {
        match wl_content::optimize_guide_image(filename, data) {
            Ok(optimized) => (optimized.bytes, optimized.filename),
            Err(reason) => {
                if reason != "not optimized" {
                    tracing::warn!(filename, reason, "wiki image optimization skipped");
                }
                (data.to_vec(), filename.to_string())
            }
        }
    } else {
        (data.to_vec(), filename.to_string())
    };
    let mime = wl_content::asset_mime_type(&name).map_err(|e| WikiError::bad_request(e.0))?;
    Ok((bytes, name, mime))
}

fn extension(name: &str) -> String {
    Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("bin")
        .to_ascii_lowercase()
}

impl Wiki {
    /// Store an image (`wiki-edit`).
    pub async fn upload_asset(
        &self,
        viewer: &Viewer,
        filename: &str,
        data: Vec<u8>,
    ) -> WikiResult<Asset> {
        let author = viewer.require_edit()?;
        let filename = filename.to_string();
        let (bytes, name, mime) = tokio::task::spawn_blocking(move || prepare(&filename, &data))
            .await
            .map_err(WikiError::internal)??;

        let mut tx = self.db.begin().await?;
        let id = sqlx::query(
            "INSERT INTO wiki_asset (filename, content_type, size, stored_name, uploaded_by, \
             uploaded_at) VALUES (?, ?, ?, '', ?, ?)",
        )
        .bind(&name)
        .bind(mime)
        .bind(bytes.len() as i64)
        .bind(author)
        .bind(now())
        .execute(&mut *tx)
        .await?
        .last_insert_id() as i64;
        let stored = format!("{id}.{}", extension(&name));
        sqlx::query("UPDATE wiki_asset SET stored_name = ? WHERE id = ?")
            .bind(&stored)
            .bind(id)
            .execute(&mut *tx)
            .await?;
        let dir = self.assets_dir();
        let path = dir.join(&stored);
        tokio::task::spawn_blocking(move || {
            std::fs::create_dir_all(&dir)?;
            let tmp = dir.join(format!(".{stored}.tmp"));
            std::fs::write(&tmp, &bytes)?;
            std::fs::rename(&tmp, &path)
        })
        .await
        .map_err(WikiError::internal)?
        .map_err(io_error)?;
        // The file exists before the row is visible; a failed commit leaves an orphan file
        // that nothing references (harmless).
        tx.commit().await?;
        self.asset(id).await
    }

    /// One image's metadata.
    pub async fn asset(&self, id: i64) -> WikiResult<Asset> {
        Ok(self.asset_row(id).await?.into_asset())
    }

    async fn asset_row(&self, id: i64) -> WikiResult<Row> {
        let row: Option<Row> =
            sqlx::query_as(sqlx::AssertSqlSafe(format!("{SELECT} WHERE a.id = ?")))
                .bind(id)
                .fetch_optional(&self.db)
                .await?;
        row.ok_or_else(|| WikiError::not_found("image not found"))
    }

    /// Which live pages show which images, from the current bodies.
    async fn usage(&self, viewer: &Viewer) -> WikiResult<Usage> {
        let rows: Vec<UsageRow> = sqlx::query_as(
            "SELECT slug, title, status, visibility, permission, body FROM wiki_page \
             WHERE deleted_at IS NULL AND body LIKE '%/api/wiki/assets/%' ORDER BY title, id",
        )
        .fetch_all(&self.db)
        .await?;
        let mut usage = Usage::default();
        for row in rows {
            let visible = viewer.can_view(
                Visibility::parse(&row.visibility).unwrap_or(Visibility::Permission),
                row.permission.as_deref(),
                Status::parse(&row.status).unwrap_or(Status::Draft),
            );
            for id in referenced_assets(&row.body) {
                *usage.total.entry(id).or_default() += 1;
                if visible {
                    usage.visible.entry(id).or_default().push(PageLink {
                        slug: row.slug.clone(),
                        title: row.title.clone(),
                    });
                }
            }
        }
        Ok(usage)
    }

    /// Every image, newest first, with the pages that show it (`wiki-edit`).
    pub async fn assets(&self, viewer: &Viewer) -> WikiResult<Vec<Asset>> {
        viewer.require_edit()?;
        let rows: Vec<Row> =
            sqlx::query_as(sqlx::AssertSqlSafe(format!("{SELECT} ORDER BY a.id DESC")))
                .fetch_all(&self.db)
                .await?;
        let usage = self.usage(viewer).await?;
        Ok(rows
            .into_iter()
            .map(|row| {
                let mut asset = row.into_asset();
                asset.used_by = usage.visible(asset.id);
                asset
            })
            .collect())
    }

    /// An image's bytes and content type (public: anyone with the URL may load it).
    pub async fn asset_bytes(&self, id: i64) -> WikiResult<(Asset, Vec<u8>)> {
        let row = self.asset_row(id).await?;
        let path = self.assets_dir().join(&row.stored_name);
        let bytes = tokio::task::spawn_blocking(move || std::fs::read(path))
            .await
            .map_err(WikiError::internal)?
            .map_err(|e| {
                tracing::warn!(id, error = %e, "wiki image file missing");
                WikiError::not_found("image not found")
            })?;
        Ok((row.into_asset(), bytes))
    }

    /// Delete an image (`wiki-edit`). An image that live pages show is a `conflict` (with
    /// `used_by`) unless `force`; forced, those pages get a broken image.
    pub async fn delete_asset(&self, viewer: &Viewer, id: i64, force: bool) -> WikiResult<()> {
        viewer.require_edit()?;
        let row = self.asset_row(id).await?;
        if !force {
            let usage = self.usage(viewer).await?;
            let count = usage.count(id);
            if count > 0 {
                let mut e = WikiError::conflict(format!(
                    "the image is shown by {count} page{}: remove it from {} first, or delete \
                     with `force=true`",
                    if count == 1 { "" } else { "s" },
                    if count == 1 { "that page" } else { "them" },
                ));
                e.used_by = usage.visible(id);
                return Err(e);
            }
        }
        sqlx::query("DELETE FROM wiki_asset WHERE id = ?")
            .bind(id)
            .execute(&self.db)
            .await?;
        let path = self.assets_dir().join(&row.stored_name);
        if let Err(e) = tokio::task::spawn_blocking(move || std::fs::remove_file(path))
            .await
            .map_err(WikiError::internal)?
        {
            tracing::warn!(id, error = %e, "could not delete wiki image file");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_referenced_images() {
        let body = "![a](/api/wiki/assets/3/a.png) [b](https://x.y/api/wiki/assets/12/b%20c.jpg)\n\
                    <Img src=\"/api/wiki/assets/3/again.png\" /> /api/wiki/assets/7 \
                    /api/wiki/assets/x/ /api/wiki/assets/40/";
        assert_eq!(
            referenced_assets(body).into_iter().collect::<Vec<_>>(),
            [3, 12, 40]
        );
        assert!(referenced_assets("no images").is_empty());
    }

    #[test]
    fn urls_are_encoded() {
        assert_eq!(asset_url(3, "a b.png"), "/api/wiki/assets/3/a%20b.png");
    }

    #[test]
    fn prepare_checks_type_and_size() {
        assert!(prepare("x.txt", b"x").is_err());
        assert!(prepare("x.png", b"").is_err());
        assert_eq!(
            prepare("x.png", &vec![0u8; MAX_ASSET_BYTES + 1])
                .unwrap_err()
                .kind,
            ErrorKind::TooLarge
        );
        let (bytes, name, mime) = prepare("x.gif", b"GIF89a").unwrap();
        assert_eq!(
            (bytes.len(), name.as_str(), mime),
            (6, "x.gif", "image/gif")
        );
    }
}
