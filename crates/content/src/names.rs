//! Page slugs, locale codes and the flat `<page>.<locale>.json` file names the admin API uses.

use crate::{ContentError, ContentResult};

/// A page or guide slug: `^[a-z][a-z0-9_-]{0,31}$`.
pub fn validate_page_slug(page: &str) -> ContentResult<()> {
    if page.is_empty() || page.len() > 32 {
        return Err(ContentError::new("Page slug must be 1–32 characters"));
    }
    let mut chars = page.chars();
    if !chars.next().is_some_and(|c| c.is_ascii_lowercase()) {
        return Err(ContentError::new(
            "Page slug must start with a lowercase letter",
        ));
    }
    if !chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_') {
        return Err(ContentError::new(
            "Page slug may only use lowercase letters, numbers, hyphens, and underscores",
        ));
    }
    Ok(())
}

/// A locale code: two lowercase ASCII letters.
pub fn validate_locale_code(locale: &str) -> ContentResult<()> {
    if locale.len() != 2 || !locale.chars().all(|c| c.is_ascii_lowercase()) {
        return Err(ContentError::new(
            "Locale must be a 2-letter lowercase code (e.g. en, de, fr)",
        ));
    }
    Ok(())
}

pub(crate) fn validate_page_locale(page: &str, locale: &str) -> ContentResult<()> {
    validate_page_slug(page)?;
    validate_locale_code(locale)
}

/// Split an admin file name like `home.en.json` into `(page, locale)`, if it is one.
pub fn parse_admin_filename(filename: &str) -> Option<(String, String)> {
    if filename.contains("..") || filename.contains('/') || filename.contains('\\') {
        return None;
    }
    let stem = filename.strip_suffix(".json")?;
    let (page, locale) = stem.split_once('.')?;
    if locale.contains('.') {
        return None;
    }
    validate_page_locale(page, locale).ok()?;
    Some((page.to_string(), locale.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs() {
        assert!(validate_page_slug("home").is_ok());
        assert!(validate_page_slug("multiboxing_setup-2").is_ok());
        assert!(validate_page_slug("").is_err());
        assert!(validate_page_slug("1abc").is_err());
        assert!(validate_page_slug("Abc").is_err());
        assert!(validate_page_slug("a.b").is_err());
        assert!(validate_page_slug(&"a".repeat(33)).is_err());
        assert!(validate_page_slug(&"a".repeat(32)).is_ok());
    }

    #[test]
    fn admin_filenames() {
        assert_eq!(
            parse_admin_filename("home.en.json"),
            Some(("home".into(), "en".into()))
        );
        assert_eq!(parse_admin_filename("home.eng.json"), None);
        assert_eq!(parse_admin_filename("home.en.de.json"), None);
        assert_eq!(parse_admin_filename("home.en.json.backup.1"), None);
        assert_eq!(parse_admin_filename("languages.json"), None);
        assert_eq!(parse_admin_filename("../home.en.json"), None);
        assert_eq!(parse_admin_filename("skills.yaml"), None);
    }
}
