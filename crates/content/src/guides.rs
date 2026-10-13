//! Guides: `<data>/guides/<slug>/<slug>.<locale>.json` plus images in the same directory.
//!
//! A guide file is a flat JSON object of strings (validated like any translation file) with the
//! keys `body` (markdown) and optionally `title`, `subtitle`, `icon`, `section` and `access`.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

use crate::assets::optimize_guide_image;
use crate::fsutil::{self, GUIDE_BACKUPS};
use crate::locales::parse_locale_json;
use crate::names::{parse_admin_filename, validate_page_locale, validate_page_slug};
use crate::{Content, ContentError, ContentResult};

/// Guides that exist on every install (listed even without a directory of their own).
const KNOWN_GUIDE_SLUGS: &[&str] = &["ddd", "marauder", "documentation", "trainee"];

/// Largest accepted guide image.
const MAX_ASSET_BYTES: usize = 10 * 1024 * 1024;
/// Images larger than this are resized/re-encoded on upload.
const OPTIMIZE_MIN_BYTES: usize = 1024 * 1024;

/// One entry of `GET /api/v2/guides`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct GuideListing {
    pub slug: String,
    pub title: String,
    pub subtitle: Option<String>,
    pub icon: Option<String>,
    /// `public` (guides index) or `fc` (FC dashboard).
    pub section: String,
    /// Permission key the frontend uses to hide the guide; advisory only.
    pub access: Option<String>,
}

/// One guide image, as listed by the admin editor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct GuideAssetInfo {
    pub name: String,
    pub size: u64,
}

fn validate_asset_filename(filename: &str) -> ContentResult<()> {
    if filename.is_empty()
        || filename.contains("..")
        || filename.contains('/')
        || filename.contains('\\')
    {
        return Err(ContentError::new("Invalid asset filename"));
    }
    let extension = Path::new(filename)
        .extension()
        .and_then(|e| e.to_str())
        .ok_or_else(|| ContentError::new("Asset must have a file extension"))?;
    if !matches!(
        extension.to_ascii_lowercase().as_str(),
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg"
    ) {
        return Err(ContentError::new("Unsupported guide asset type"));
    }
    Ok(())
}

/// The `Content-Type` of a guide image, by extension.
pub fn asset_mime_type(filename: &str) -> ContentResult<&'static str> {
    validate_asset_filename(filename)?;
    let extension = Path::new(filename)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    Ok(match extension.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        _ => "application/octet-stream",
    })
}

fn optional_string_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

fn title_from_body(body: &str) -> String {
    let first = body.lines().next().unwrap_or("").trim();
    first
        .strip_prefix('#')
        .map(|t| t.trim().to_string())
        .unwrap_or_default()
}

fn slug_display_name(slug: &str) -> String {
    slug.split('_')
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                None => String::new(),
                Some(first) => first.to_uppercase().chain(chars).collect(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn default_section(slug: &str) -> &'static str {
    if matches!(slug, "documentation" | "trainee") {
        "fc"
    } else {
        "public"
    }
}

fn default_access(slug: &str) -> Option<&'static str> {
    match slug {
        "documentation" => Some("waitlist-tag:HQ-FC"),
        "trainee" => Some("waitlist-tag:TRAINEE"),
        _ => None,
    }
}

fn guide_not_found(page: &str, locale: &str) -> ContentError {
    ContentError::new(format!("Locale file not found: {page}.{locale}.json"))
}

impl Content {
    /// Whether translations for `page` are a guide (stored under `guides/`).
    pub fn is_guide_page(&self, page: &str) -> bool {
        if page == "home" || page == "nav" {
            return false;
        }
        KNOWN_GUIDE_SLUGS.contains(&page)
            || (validate_page_slug(page).is_ok() && self.guide_root().join(page).is_dir())
    }

    fn guide_dir(&self, page: &str) -> ContentResult<PathBuf> {
        validate_page_slug(page)?;
        Ok(self.guide_root().join(page))
    }

    fn guide_locale_path(&self, page: &str, locale: &str) -> ContentResult<PathBuf> {
        validate_page_locale(page, locale)?;
        Ok(self
            .guide_root()
            .join(page)
            .join(format!("{page}.{locale}.json")))
    }

    /// Where guides were stored before they had their own directory.
    fn legacy_guide_locale_path(&self, page: &str, locale: &str) -> PathBuf {
        self.locale_dir().join(format!("{page}.{locale}.json"))
    }

    fn list_guide_slugs(&self) -> Vec<String> {
        let mut slugs = BTreeSet::new();
        if let Ok(entries) = fs::read_dir(self.guide_root()) {
            for entry in entries.flatten() {
                if entry.file_type().is_ok_and(|t| t.is_dir()) {
                    slugs.insert(entry.file_name().to_string_lossy().into_owned());
                }
            }
        }
        slugs.extend(KNOWN_GUIDE_SLUGS.iter().map(|s| s.to_string()));
        for (_, page, _) in self.locale_dir_filenames() {
            if self.is_guide_page(&page) {
                slugs.insert(page);
            }
        }
        slugs.into_iter().collect()
    }

    /// Every readable guide, sorted by slug. Guides without a valid file are left out.
    pub fn list_guides(&self) -> Vec<GuideListing> {
        self.list_guide_slugs()
            .into_iter()
            .filter_map(|slug| self.guide_listing(&slug).ok())
            .collect()
    }

    fn guide_listing(&self, slug: &str) -> ContentResult<GuideListing> {
        let value = self
            .read_locale_json(slug, "en")
            .or_else(|_| self.read_locale_json(slug, &self.first_locale_for_slug(slug)?))?;
        let body = value.get("body").and_then(Value::as_str).unwrap_or("");
        let title = optional_string_field(&value, "title")
            .filter(|s| !s.is_empty())
            .or_else(|| Some(title_from_body(body)).filter(|t| !t.is_empty()))
            .unwrap_or_else(|| slug_display_name(slug));
        Ok(GuideListing {
            slug: slug.to_string(),
            title,
            subtitle: optional_string_field(&value, "subtitle"),
            icon: optional_string_field(&value, "icon"),
            section: optional_string_field(&value, "section")
                .unwrap_or_else(|| default_section(slug).to_string()),
            access: optional_string_field(&value, "access")
                .or_else(|| default_access(slug).map(str::to_string)),
        })
    }

    fn first_locale_for_slug(&self, slug: &str) -> ContentResult<String> {
        let not_found = || ContentError::new(format!("Guide not found: {slug}"));
        let entries = fs::read_dir(self.guide_dir(slug)?).map_err(|_| not_found())?;
        let mut locales: Vec<String> = entries
            .flatten()
            .filter_map(|entry| {
                let (page, locale) = parse_admin_filename(&entry.file_name().to_string_lossy())?;
                (page == slug).then_some(locale)
            })
            .collect();
        locales.sort();
        locales.into_iter().next().ok_or_else(not_found)
    }

    /// Admin file names of every guide translation (guide directories first, then the old
    /// `locales/` location), sorted and de-duplicated.
    pub(crate) fn guide_admin_filenames(&self) -> Vec<String> {
        let mut names = BTreeSet::new();
        if let Ok(entries) = fs::read_dir(self.guide_root()) {
            for guide in entries.flatten() {
                if !guide.file_type().is_ok_and(|t| t.is_dir()) {
                    continue;
                }
                let slug = guide.file_name().to_string_lossy().into_owned();
                let Ok(files) = fs::read_dir(guide.path()) else {
                    continue;
                };
                for file in files.flatten() {
                    let name = file.file_name().to_string_lossy().into_owned();
                    if !name.ends_with(".json") || name.contains(".backup.") {
                        continue;
                    }
                    if parse_admin_filename(&name).is_some()
                        && name.starts_with(&format!("{slug}."))
                    {
                        names.insert(name);
                    }
                }
            }
        }
        for (name, page, _) in self.locale_dir_filenames() {
            if self.is_guide_page(&page) {
                names.insert(name);
            }
        }
        names.into_iter().collect()
    }

    pub(crate) fn guide_locale_size(&self, page: &str, locale: &str) -> u64 {
        self.guide_locale_path(page, locale)
            .ok()
            .and_then(|path| fsutil::file_size(&path))
            .or_else(|| fsutil::file_size(&self.legacy_guide_locale_path(page, locale)))
            .unwrap_or(0)
    }

    pub(crate) fn read_guide_locale(&self, page: &str, locale: &str) -> ContentResult<String> {
        let path = self.guide_locale_path(page, locale)?;
        let read = |path: &Path| {
            fs::read_to_string(path)
                .map_err(|e| ContentError::new(format!("Failed to read locale file: {e}")))
        };
        if path.exists() {
            return read(&path);
        }
        let legacy = self.legacy_guide_locale_path(page, locale);
        if legacy.exists() {
            return read(&legacy);
        }
        Err(guide_not_found(page, locale))
    }

    /// Validate and save a guide file into `guides/<page>/`, creating the guide if needed.
    pub fn save_guide_locale(&self, page: &str, locale: &str, content: &str) -> ContentResult<()> {
        let path = self.guide_locale_path(page, locale)?;
        parse_locale_json(content)?;
        let _guard = self.write_guard();
        fsutil::create_dir(&self.guide_dir(page)?, "guide")?;
        fsutil::create_backup(&path, GUIDE_BACKUPS, "guide")?;
        fsutil::atomic_write(&path, content.as_bytes(), "locale")
    }

    pub(crate) fn delete_guide_locale(&self, page: &str, locale: &str) -> ContentResult<()> {
        let path = self.guide_locale_path(page, locale)?;
        if !path.exists() {
            return Err(guide_not_found(page, locale));
        }
        let _guard = self.write_guard();
        fsutil::create_backup(&path, GUIDE_BACKUPS, "guide")?;
        fs::remove_file(&path)
            .map_err(|e| ContentError::new(format!("Failed to delete locale file: {e}")))
    }

    /// The bytes of a guide image.
    pub fn read_asset(&self, page: &str, filename: &str) -> ContentResult<Vec<u8>> {
        validate_asset_filename(filename)?;
        let path = self.guide_dir(page)?.join(filename);
        if !path.exists() {
            return Err(ContentError::new(format!(
                "Guide asset not found: {filename}"
            )));
        }
        fs::read(&path).map_err(|e| ContentError::new(format!("Failed to read asset: {e}")))
    }

    /// The images of a guide, sorted by name (empty when the guide has no directory).
    pub fn list_assets(&self, page: &str) -> ContentResult<Vec<GuideAssetInfo>> {
        let dir = self.guide_dir(page)?;
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let entries = fs::read_dir(&dir)
            .map_err(|e| ContentError::new(format!("Failed to read guide directory: {e}")))?;
        let mut assets: Vec<_> = entries
            .flatten()
            .filter(|entry| entry.file_type().is_ok_and(|t| t.is_file()))
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.contains(".backup.") || validate_asset_filename(&name).is_err() {
                    return None;
                }
                let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                Some(GuideAssetInfo { name, size })
            })
            .collect();
        assets.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(assets)
    }

    /// Save a guide image, optimizing large ones (see [`optimize_guide_image`]). Returns the
    /// saved file name (an opaque PNG becomes a `.jpg`) and its size.
    pub fn save_asset(
        &self,
        page: &str,
        filename: &str,
        data: &[u8],
    ) -> ContentResult<(String, u64)> {
        validate_asset_filename(filename)?;
        if data.is_empty() {
            return Err(ContentError::new("Asset must not be empty"));
        }
        if data.len() > MAX_ASSET_BYTES {
            return Err(ContentError::new("Asset exceeds 10 MiB upload limit"));
        }
        let dir = self.guide_dir(page)?;

        let (bytes, save_filename) = if data.len() > OPTIMIZE_MIN_BYTES {
            match optimize_guide_image(filename, data) {
                Ok(optimized) => {
                    tracing::info!(
                        from = filename,
                        to = %optimized.filename,
                        before = data.len(),
                        after = optimized.bytes.len(),
                        "optimized guide image"
                    );
                    (optimized.bytes, optimized.filename)
                }
                Err(reason) => {
                    if reason != "not optimized" {
                        tracing::warn!(filename, reason, "guide image optimization skipped");
                    }
                    (data.to_vec(), filename.to_string())
                }
            }
        } else {
            (data.to_vec(), filename.to_string())
        };
        if bytes.is_empty() {
            return Err(ContentError::new("Asset must not be empty"));
        }
        if bytes.len() > MAX_ASSET_BYTES {
            return Err(ContentError::new(
                "Asset still exceeds 10 MiB after compression; use a smaller image",
            ));
        }

        let _guard = self.write_guard();
        fsutil::create_dir(&dir, "guide")?;
        let path = dir.join(&save_filename);
        fsutil::create_backup(&path, GUIDE_BACKUPS, "guide")?;
        fsutil::atomic_write(&path, &bytes, "asset")?;
        Ok((save_filename, bytes.len() as u64))
    }

    /// Delete a guide image (no backup is kept, as in the legacy waitlist).
    pub fn delete_asset(&self, page: &str, filename: &str) -> ContentResult<()> {
        validate_asset_filename(filename)?;
        let path = self.guide_dir(page)?.join(filename);
        if !path.exists() {
            return Err(ContentError::new(format!(
                "Guide asset not found: {filename}"
            )));
        }
        let _guard = self.write_guard();
        fs::remove_file(&path)
            .map_err(|e| ContentError::new(format!("Failed to delete asset: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn setup() -> (tempfile::TempDir, Content) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("guides/ddd")).unwrap();
        fs::create_dir_all(root.join("guides/multiboxing_setup")).unwrap();
        fs::create_dir_all(root.join("locales")).unwrap();
        fs::write(
            root.join("guides/ddd/ddd.en.json"),
            r#"{"title":"DDD Guide","subtitle":"sub","icon":"i","body":"x"}"#,
        )
        .unwrap();
        fs::write(root.join("guides/ddd/ddd.de.json"), r#"{"body":"y"}"#).unwrap();
        fs::write(root.join("guides/ddd/ddd.en.json.backup.5"), "{}").unwrap();
        fs::write(root.join("guides/ddd/pic.png"), b"png").unwrap();
        fs::write(root.join("guides/ddd/notes.txt"), b"txt").unwrap();
        fs::write(
            root.join("guides/multiboxing_setup/multiboxing_setup.de.json"),
            "{\"body\":\"# Boxing  \\nmore\"}",
        )
        .unwrap();
        // A guide stored the old way, and a broken one.
        fs::write(
            root.join("locales/marauder.en.json"),
            r#"{"body":"no heading","section":"fc"}"#,
        )
        .unwrap();
        fs::write(root.join("locales/trainee.en.json"), r#"{"body":1}"#).unwrap();
        fs::write(root.join("locales/home.en.json"), r#"{"a":"b"}"#).unwrap();
        let content = Content::new(root);
        (dir, content)
    }

    #[test]
    fn lists_guides_with_metadata_fallbacks() {
        let (_dir, content) = setup();
        let guides = content.list_guides();
        let slugs: Vec<_> = guides.iter().map(|g| g.slug.as_str()).collect();
        assert_eq!(slugs, vec!["ddd", "marauder", "multiboxing_setup"]);
        assert_eq!(guides[0].title, "DDD Guide");
        assert_eq!(guides[0].subtitle.as_deref(), Some("sub"));
        assert_eq!(guides[0].section, "public");
        assert_eq!(guides[1].title, "Marauder");
        assert_eq!(guides[1].section, "fc");
        assert_eq!(guides[2].title, "Boxing");
    }

    #[test]
    fn admin_filenames_merge_guides_and_locales() {
        let (_dir, content) = setup();
        assert_eq!(
            content.list_admin_filenames(),
            vec![
                "ddd.de.json",
                "ddd.en.json",
                "home.en.json",
                "marauder.en.json",
                "multiboxing_setup.de.json",
                "trainee.en.json",
            ]
        );
        assert_eq!(content.admin_file_size("ddd.fr.json"), Some(0));
        assert_eq!(content.admin_file_size("home.en.json"), Some(9));
        assert_eq!(content.admin_file_size("skills.yaml"), None);
    }

    #[test]
    fn guide_reads_fall_back_to_locales_dir() {
        let (_dir, content) = setup();
        assert!(content.read_locale("marauder", "en").is_ok());
        assert_eq!(
            content.read_locale("marauder", "de").unwrap_err().0,
            "Locale file not found: marauder.de.json"
        );
        assert!(content.read_locale_json("trainee", "en").is_err());
    }

    #[test]
    fn saving_a_guide_creates_its_directory() {
        let (dir, content) = setup();
        content
            .save_guide_locale("new_guide", "en", r##"{"body":"# New"}"##)
            .unwrap();
        assert!(
            dir.path()
                .join("guides/new_guide/new_guide.en.json")
                .exists()
        );
        assert!(content.is_guide_page("new_guide"));
        // Later saves through the generic path go to the guide directory too.
        content
            .save_locale("new_guide", "de", r##"{"body":"# Neu"}"##)
            .unwrap();
        assert!(
            dir.path()
                .join("guides/new_guide/new_guide.de.json")
                .exists()
        );
        assert!(
            content
                .save_guide_locale("Bad", "en", r#"{"a":"b"}"#)
                .is_err()
        );
    }

    #[test]
    fn assets() {
        let (_dir, content) = setup();
        assert_eq!(
            content.list_assets("ddd").unwrap(),
            vec![GuideAssetInfo {
                name: "pic.png".into(),
                size: 3
            }]
        );
        assert_eq!(content.list_assets("nothing").unwrap(), vec![]);
        assert_eq!(content.read_asset("ddd", "pic.png").unwrap(), b"png");
        assert_eq!(
            content.read_asset("ddd", "missing.png").unwrap_err().0,
            "Guide asset not found: missing.png"
        );
        assert_eq!(
            content.read_asset("ddd", "notes.txt").unwrap_err().0,
            "Unsupported guide asset type"
        );
        assert_eq!(asset_mime_type("a.SVG").unwrap(), "image/svg+xml");

        let (name, size) = content.save_asset("ddd", "small.gif", b"GIF89a").unwrap();
        assert_eq!((name.as_str(), size), ("small.gif", 6));
        assert!(content.save_asset("ddd", "empty.gif", b"").is_err());
        content.delete_asset("ddd", "small.gif").unwrap();
        assert!(content.delete_asset("ddd", "small.gif").is_err());
    }
}
