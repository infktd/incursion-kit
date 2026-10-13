//! The bundled example data set: a small, neutral doctrine (Vindicator, marauders, Nestor and
//! Loki logistics) with its categories, skills, skill plans, modules, implants, notes and tags.
//!
//! It seeds an empty data directory on first run (`waitlist serve`, `waitlist init-data`) so a
//! fresh install works out of the box, and it is the data set the tests run against. `rules.yaml`
//! is not part of it: without that file the built-in defaults ([`crate::DEFAULT_RULES`]) apply.

use std::io;
use std::path::Path;

/// `(file name, contents)` of every file in the example data set.
pub const FILES: [(&str, &str); 8] = [
    (
        "categories.yaml",
        include_str!("../example-data/categories.yaml"),
    ),
    (
        "fitnotes.yaml",
        include_str!("../example-data/fitnotes.yaml"),
    ),
    ("fits.dat", include_str!("../example-data/fits.dat")),
    (
        "implants.yaml",
        include_str!("../example-data/implants.yaml"),
    ),
    ("modules.yaml", include_str!("../example-data/modules.yaml")),
    (
        "skillplan.yaml",
        include_str!("../example-data/skillplan.yaml"),
    ),
    ("skills.yaml", include_str!("../example-data/skills.yaml")),
    ("tags.yaml", include_str!("../example-data/tags.yaml")),
];

/// What [`seed`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeedOutcome {
    /// The example files were written.
    Seeded,
    /// The directory already had files; nothing was written.
    NotEmpty,
}

/// Writes the example data set into `dir`, creating it if needed.
///
/// Without `force` this only happens when `dir` is missing or empty (hidden files such as
/// `.gitkeep` don't count), so existing community data is never touched. With `force` the
/// example files overwrite files of the same name; other files are left alone.
pub fn seed(dir: &Path, force: bool) -> io::Result<SeedOutcome> {
    if !force && !is_empty(dir)? {
        return Ok(SeedOutcome::NotEmpty);
    }
    std::fs::create_dir_all(dir)?;
    for (name, contents) in FILES {
        std::fs::write(dir.join(name), contents)?;
    }
    Ok(SeedOutcome::Seeded)
}

/// Whether `dir` is missing or has no entries other than hidden files.
pub fn is_empty(dir: &Path) -> io::Result<bool> {
    match std::fs::read_dir(dir) {
        Ok(entries) => {
            for entry in entries {
                if !entry?.file_name().to_string_lossy().starts_with('.') {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(true),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DataFile;

    #[test]
    fn covers_every_required_data_file() {
        for file in DataFile::ALL.into_iter().filter(|f| !f.optional()) {
            assert!(
                FILES.iter().any(|(name, _)| *name == file.file_name()),
                "{} missing from the example data",
                file.file_name()
            );
        }
    }

    #[test]
    fn seeds_only_empty_directories_unless_forced() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("data");
        assert_eq!(seed(&dir, false).unwrap(), SeedOutcome::Seeded);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), FILES.len());

        std::fs::write(dir.join("tags.yaml"), "public_tags: []\n").unwrap();
        assert_eq!(seed(&dir, false).unwrap(), SeedOutcome::NotEmpty);
        assert_eq!(
            std::fs::read_to_string(dir.join("tags.yaml")).unwrap(),
            "public_tags: []\n"
        );

        std::fs::write(dir.join("rules.yaml"), "# custom\n").unwrap();
        assert_eq!(seed(&dir, true).unwrap(), SeedOutcome::Seeded);
        assert_eq!(
            std::fs::read_to_string(dir.join("tags.yaml")).unwrap(),
            FILES[7].1
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("rules.yaml")).unwrap(),
            "# custom\n"
        );

        let hidden = root.path().join("hidden");
        std::fs::create_dir(&hidden).unwrap();
        std::fs::write(hidden.join(".gitkeep"), "").unwrap();
        assert!(is_empty(&hidden).unwrap());
    }
}
