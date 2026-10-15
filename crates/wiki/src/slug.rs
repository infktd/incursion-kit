//! Page slugs (`guides/newbro`), section keys (`start`) and permission names.

/// Longest slug path.
pub const MAX_SLUG_LEN: usize = 191;
/// Most `/`-separated segments in a slug.
pub const MAX_SLUG_SEGMENTS: usize = 6;
/// Longest section key.
pub const MAX_SECTION_KEY_LEN: usize = 64;

fn valid_segment(segment: &str) -> bool {
    segment.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && segment
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// A page slug: 1 to 6 segments of `[a-z0-9][a-z0-9_-]*` separated by `/`, at most 191
/// characters (`guides/newbro`).
pub fn validate_slug(slug: &str) -> Result<(), String> {
    if slug.is_empty() || slug.len() > MAX_SLUG_LEN {
        return Err(format!("a page path is 1 to {MAX_SLUG_LEN} characters"));
    }
    let segments: Vec<&str> = slug.split('/').collect();
    if segments.len() > MAX_SLUG_SEGMENTS {
        return Err(format!(
            "a page path has at most {MAX_SLUG_SEGMENTS} `/`-separated parts"
        ));
    }
    if !segments.iter().all(|s| valid_segment(s)) {
        return Err(format!(
            "invalid page path `{slug}`: use lowercase letters, digits, `-` and `_`, in parts \
             separated by `/` (e.g. `guides/new-pilots`)"
        ));
    }
    Ok(())
}

/// A section key: `[a-z0-9][a-z0-9_-]*`, at most 64 characters.
pub fn validate_section_key(key: &str) -> Result<(), String> {
    if key.is_empty() || key.len() > MAX_SECTION_KEY_LEN || !valid_segment(key) {
        return Err(format!(
            "invalid section key `{key}`: 1 to {MAX_SECTION_KEY_LEN} lowercase letters, digits, \
             `-` and `_`"
        ));
    }
    Ok(())
}

/// Lowercase a free-form name into a slug segment (`New Pilot Guide` → `new-pilot-guide`).
pub fn slugify(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs() {
        assert!(validate_slug("guides/newbro").is_ok());
        assert!(validate_slug("fc-cheatsheets/target_priority").is_ok());
        assert!(validate_slug("a").is_ok());
        for bad in [
            "",
            "/a",
            "a/",
            "a//b",
            "A",
            "a b",
            "a/../b",
            "-a",
            "a/b/c/d/e/f/g",
        ] {
            assert!(validate_slug(bad).is_err(), "{bad}");
        }
        assert!(validate_section_key("start").is_ok());
        assert!(validate_section_key("a/b").is_err());
        assert_eq!(slugify("New Pilot Guide!"), "new-pilot-guide");
    }
}
