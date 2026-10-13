//! Backups and atomic writes.

use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::{ContentError, ContentResult};

/// Backups kept per locale file (legacy waitlist behaviour).
pub(crate) const LOCALE_BACKUPS: usize = 3;
/// Backups kept per guide file and guide image (legacy waitlist behaviour).
pub(crate) const GUIDE_BACKUPS: usize = 5;

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

/// Copy `path` to `<path>.backup.<unix seconds>` (if it exists) and delete all but the newest
/// `keep` backups of it.
pub(crate) fn create_backup(path: &Path, keep: usize, what: &str) -> ContentResult<()> {
    if !path.exists() {
        return Ok(());
    }
    let invalid = || ContentError::new(format!("Invalid {what} path"));
    let parent = path.parent().ok_or_else(invalid)?;
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(invalid)?;
    let backup_path = parent.join(format!("{file_name}.backup.{}", unix_now()));
    fs::copy(path, &backup_path)
        .map_err(|e| ContentError::new(format!("Failed to create backup: {e}")))?;

    let prefix = format!("{file_name}.backup.");
    let mut backups: Vec<_> = fs::read_dir(parent)
        .map_err(|e| ContentError::new(format!("Failed to read {what} directory: {e}")))?
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let ts = name.strip_prefix(&prefix)?.parse::<u64>().ok()?;
            Some((entry.path(), ts))
        })
        .collect();
    backups.sort_by_key(|(_, ts)| std::cmp::Reverse(*ts));
    for (old, _) in backups.into_iter().skip(keep) {
        if let Err(error) = fs::remove_file(&old) {
            tracing::warn!(path = %old.display(), %error, "could not delete old backup");
        }
    }
    Ok(())
}

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Write `data` to `path` atomically: a temporary file next to it, then a rename.
pub(crate) fn atomic_write(path: &Path, data: &[u8], what: &str) -> ContentResult<()> {
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| ContentError::new(format!("Invalid {what} path")))?;
    let tmp = path.with_file_name(format!(
        ".{file_name}.{}.{}.tmp",
        std::process::id(),
        TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    if let Err(e) = fs::write(&tmp, data) {
        let _ = fs::remove_file(&tmp);
        return Err(ContentError::new(format!("Failed to write temp file: {e}")));
    }
    fs::rename(&tmp, path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        ContentError::new(format!("Failed to save {what} file: {e}"))
    })
}

pub(crate) fn create_dir(dir: &Path, what: &str) -> ContentResult<()> {
    fs::create_dir_all(dir)
        .map_err(|e| ContentError::new(format!("Failed to create {what} directory: {e}")))
}

/// The size of a file, if it exists.
pub(crate) fn file_size(path: &Path) -> Option<u64> {
    fs::metadata(path)
        .ok()
        .filter(|m| m.is_file())
        .map(|m| m.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backups_are_pruned() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("x.en.json");
        fs::write(&file, "1").unwrap();
        for ts in [100, 200, 300, 400] {
            fs::write(dir.path().join(format!("x.en.json.backup.{ts}")), "old").unwrap();
        }
        // Another file's backups are left alone.
        fs::write(dir.path().join("y.en.json.backup.1"), "other").unwrap();
        create_backup(&file, 3, "locale").unwrap();
        let mut names: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names.len(), 5, "{names:?}");
        assert!(!names.contains(&"x.en.json.backup.100".to_string()));
        assert!(!names.contains(&"x.en.json.backup.200".to_string()));
        assert!(names.contains(&"x.en.json.backup.400".to_string()));
        assert!(names.contains(&"y.en.json.backup.1".to_string()));
    }

    #[test]
    fn atomic_write_replaces_and_leaves_no_temp() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.json");
        atomic_write(&file, b"one", "locale").unwrap();
        atomic_write(&file, b"two", "locale").unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "two");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}
