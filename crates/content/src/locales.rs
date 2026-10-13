//! UI translations: `<data>/locales/<page>.<locale>.json` and `languages.json`.
//!
//! Guide pages are delegated to [`crate::guides`]: their translations live in the guide's own
//! directory (falling back to the old `locales/` location when reading).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;

use serde_json::Value;

use crate::fsutil::{self, LOCALE_BACKUPS};
use crate::names::{parse_admin_filename, validate_locale_code, validate_page_locale};
use crate::{Content, ContentError, ContentResult};

/// The admin file name of the language labels file.
pub const LANGUAGE_LABELS_FILENAME: &str = "languages.json";

/// Parse and validate a translation file: a non-empty JSON object of non-empty keys to strings.
pub(crate) fn parse_locale_json(content: &str) -> ContentResult<Value> {
    let value: Value = serde_json::from_str(content)
        .map_err(|e| ContentError::new(format!("Invalid JSON: {e}")))?;
    let obj = value
        .as_object()
        .ok_or_else(|| ContentError::new("Locale file must be a JSON object"))?;
    if obj.is_empty() {
        return Err(ContentError::new("Locale file must not be empty"));
    }
    for (key, val) in obj {
        if key.is_empty() {
            return Err(ContentError::new("Keys must not be empty"));
        }
        if !val.is_string() {
            return Err(ContentError::new(format!(
                "Key \"{key}\" must have a string value"
            )));
        }
    }
    Ok(value)
}

fn parse_language_labels(value: &Value) -> ContentResult<BTreeMap<String, String>> {
    let obj = value
        .as_object()
        .ok_or_else(|| ContentError::new("Language labels must be a JSON object"))?;
    let mut map = BTreeMap::new();
    for (key, val) in obj {
        validate_locale_code(key)?;
        let label = val.as_str().ok_or_else(|| {
            ContentError::new(format!("Language label for {key} must be a string"))
        })?;
        if label.is_empty() {
            return Err(ContentError::new(format!(
                "Language label for {key} must not be empty"
            )));
        }
        map.insert(key.clone(), label.to_string());
    }
    Ok(map)
}

fn not_found(page: &str, locale: &str) -> ContentError {
    ContentError::new(format!("Locale file not found: {page}.{locale}.json"))
}

impl Content {
    fn language_labels_path(&self) -> PathBuf {
        self.locale_dir().join(LANGUAGE_LABELS_FILENAME)
    }

    /// Path of a non-guide translation file.
    fn plain_locale_path(&self, page: &str, locale: &str) -> ContentResult<PathBuf> {
        validate_page_locale(page, locale)?;
        Ok(self.locale_dir().join(format!("{page}.{locale}.json")))
    }

    /// File names (`<page>.<locale>.json`, no backups) of the translation files in `locales/`.
    pub(crate) fn locale_dir_filenames(&self) -> Vec<(String, String, String)> {
        let Ok(entries) = fs::read_dir(self.locale_dir()) else {
            return Vec::new();
        };
        let mut names: Vec<_> = entries
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                if !name.ends_with(".json") || name.contains(".backup.") {
                    return None;
                }
                let (page, locale) = parse_admin_filename(&name)?;
                Some((name, page, locale))
            })
            .collect();
        names.sort();
        names
    }

    fn locales_for_page(&self, page: &str) -> BTreeSet<String> {
        self.locale_dir_filenames()
            .into_iter()
            .filter(|(_, p, _)| p == page)
            .map(|(_, _, locale)| locale)
            .collect()
    }

    /// Languages with both a `home` and a `nav` translation, sorted.
    pub fn list_valid_languages(&self) -> Vec<String> {
        let home = self.locales_for_page("home");
        let nav = self.locales_for_page("nav");
        home.intersection(&nav).cloned().collect()
    }

    /// The raw `languages.json`, or `{}` when there is none.
    pub fn read_language_labels_json(&self) -> ContentResult<String> {
        let path = self.language_labels_path();
        if !path.exists() {
            return Ok("{}".to_string());
        }
        fs::read_to_string(path)
            .map_err(|e| ContentError::new(format!("Failed to read language labels: {e}")))
    }

    fn read_language_labels_map(&self) -> ContentResult<BTreeMap<String, String>> {
        let content = self.read_language_labels_json()?;
        let value: Value = serde_json::from_str(&content)
            .map_err(|e| ContentError::new(format!("Invalid language labels JSON: {e}")))?;
        parse_language_labels(&value)
    }

    /// A display label for each language: from `languages.json`, else the upper-cased code. An
    /// invalid `languages.json` is ignored.
    pub fn language_labels_for(&self, languages: &[String]) -> BTreeMap<String, String> {
        let file_labels = self.read_language_labels_map().unwrap_or_default();
        languages
            .iter()
            .map(|code| {
                let label = file_labels
                    .get(code)
                    .cloned()
                    .unwrap_or_else(|| code.to_uppercase());
                (code.clone(), label)
            })
            .collect()
    }

    /// Validate and save `languages.json`.
    pub fn save_language_labels(&self, content: &str) -> ContentResult<()> {
        let value: Value = serde_json::from_str(content)
            .map_err(|e| ContentError::new(format!("Invalid JSON: {e}")))?;
        parse_language_labels(&value)?;
        let _guard = self.write_guard();
        fsutil::create_dir(&self.locale_dir(), "locale")?;
        fsutil::atomic_write(
            &self.language_labels_path(),
            content.as_bytes(),
            "language labels",
        )
        .map_err(|e| ContentError::new(format!("Failed to save language labels: {e}")))
    }

    /// Size of `languages.json`, if it exists.
    pub fn language_labels_size(&self) -> Option<u64> {
        fsutil::file_size(&self.language_labels_path())
    }

    /// The raw text of a translation (or guide) file.
    pub fn read_locale(&self, page: &str, locale: &str) -> ContentResult<String> {
        if self.is_guide_page(page) {
            return self.read_guide_locale(page, locale);
        }
        let path = self.plain_locale_path(page, locale)?;
        if !path.exists() {
            return Err(not_found(page, locale));
        }
        fs::read_to_string(path)
            .map_err(|e| ContentError::new(format!("Failed to read locale file: {e}")))
    }

    /// A translation (or guide) file, parsed and validated.
    pub fn read_locale_json(&self, page: &str, locale: &str) -> ContentResult<Value> {
        parse_locale_json(&self.read_locale(page, locale)?)
    }

    /// Validate and save a translation file. Guide pages are saved into the guide's directory.
    pub fn save_locale(&self, page: &str, locale: &str, content: &str) -> ContentResult<()> {
        if self.is_guide_page(page) {
            return self.save_guide_locale(page, locale, content);
        }
        let path = self.plain_locale_path(page, locale)?;
        parse_locale_json(content)?;
        let _guard = self.write_guard();
        fsutil::create_dir(&self.locale_dir(), "locale")?;
        fsutil::create_backup(&path, LOCALE_BACKUPS, "locale")?;
        fsutil::atomic_write(&path, content.as_bytes(), "locale")
    }

    /// Delete a translation (or guide) file, keeping a backup.
    pub fn delete_locale(&self, page: &str, locale: &str) -> ContentResult<()> {
        if self.is_guide_page(page) {
            return self.delete_guide_locale(page, locale);
        }
        let path = self.plain_locale_path(page, locale)?;
        if !path.exists() {
            return Err(not_found(page, locale));
        }
        let _guard = self.write_guard();
        fsutil::create_backup(&path, LOCALE_BACKUPS, "locale")?;
        fs::remove_file(&path)
            .map_err(|e| ContentError::new(format!("Failed to delete locale file: {e}")))
    }

    /// Every translation and guide file the admin editor lists (`<page>.<locale>.json`), sorted.
    pub fn list_admin_filenames(&self) -> Vec<String> {
        let mut names = self.guide_admin_filenames();
        for (name, page, _) in self.locale_dir_filenames() {
            if !self.is_guide_page(&page) {
                names.push(name);
            }
        }
        names.sort();
        names
    }

    /// Size of the file behind an admin file name (0 when it does not exist), or `None` when the
    /// name is not a translation file name.
    pub fn admin_file_size(&self, filename: &str) -> Option<u64> {
        let (page, locale) = parse_admin_filename(filename)?;
        if self.is_guide_page(&page) {
            return Some(self.guide_locale_size(&page, &locale));
        }
        let path = self.plain_locale_path(&page, &locale).ok()?;
        Some(fsutil::file_size(&path).unwrap_or(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn setup() -> (tempfile::TempDir, Content) {
        let dir = tempfile::tempdir().unwrap();
        let locales = dir.path().join("locales");
        fs::create_dir_all(&locales).unwrap();
        for name in [
            "home.en.json",
            "home.de.json",
            "nav.en.json",
            "nav.de.json",
            "home.fr.json.backup.1",
            "nav.fr.json",
            "home.ru.json",
        ] {
            fs::write(locales.join(name), r#"{"a":"b"}"#).unwrap();
        }
        fs::write(
            locales.join("languages.json"),
            r#"{"en":"English","fr":"Français"}"#,
        )
        .unwrap();
        let content = Content::new(dir.path());
        (dir, content)
    }

    #[test]
    fn languages_need_home_and_nav() {
        let (_dir, content) = setup();
        let langs = content.list_valid_languages();
        assert_eq!(langs, vec!["de".to_string(), "en".to_string()]);
        let labels = content.language_labels_for(&langs);
        assert_eq!(labels["en"], "English");
        assert_eq!(labels["de"], "DE");
    }

    #[test]
    fn locale_validation() {
        assert!(parse_locale_json(r#"{"a":"b"}"#).is_ok());
        assert_eq!(
            parse_locale_json("{}").unwrap_err().0,
            "Locale file must not be empty"
        );
        assert_eq!(
            parse_locale_json("[]").unwrap_err().0,
            "Locale file must be a JSON object"
        );
        assert_eq!(
            parse_locale_json(r#"{"a":1}"#).unwrap_err().0,
            "Key \"a\" must have a string value"
        );
        assert_eq!(
            parse_locale_json(r#"{"":"x"}"#).unwrap_err().0,
            "Keys must not be empty"
        );
        assert!(
            parse_locale_json("nope")
                .unwrap_err()
                .0
                .starts_with("Invalid JSON: ")
        );
    }

    #[test]
    fn save_validates_before_writing_and_keeps_backups() {
        let (dir, content) = setup();
        let path = dir.path().join("locales/home.en.json");
        assert!(content.save_locale("home", "en", r#"{"a":1}"#).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), r#"{"a":"b"}"#);

        content.save_locale("home", "en", r#"{"a":"c"}"#).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), r#"{"a":"c"}"#);
        let backups = fs::read_dir(dir.path().join("locales"))
            .unwrap()
            .flatten()
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("home.en.json.backup.")
            })
            .count();
        assert_eq!(backups, 1);

        assert_eq!(
            content.read_locale("home", "xx").unwrap_err().0,
            "Locale file not found: home.xx.json"
        );
        content.delete_locale("home", "en").unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn language_labels() {
        let (_dir, content) = setup();
        assert!(content.save_language_labels(r#"{"eng":"x"}"#).is_err());
        assert!(content.save_language_labels(r#"{"en":""}"#).is_err());
        content.save_language_labels(r#"{"en":"E"}"#).unwrap();
        assert_eq!(
            content.read_language_labels_json().unwrap(),
            r#"{"en":"E"}"#
        );
    }
}
