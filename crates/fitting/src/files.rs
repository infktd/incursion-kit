//! The data files in the data directory and their raw contents.

use std::{collections::BTreeMap, path::Path};

use crate::DataError;

/// One of the community data files read from the data directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DataFile {
    /// `categories.yaml`: waitlist categories and the hull/module rules that assign them.
    Categories,
    /// `fits.dat`: doctrine fits, as an in-game fitting link dump.
    Fits,
    /// `fitnotes.yaml`: notes shown next to doctrine fits.
    FitNotes,
    /// `implants.yaml`: the implant guide shown on the fits page.
    Implants,
    /// `modules.yaml`: module alternatives, downgrade rules and fit-check exceptions.
    Modules,
    /// `skillplan.yaml`: skill plans.
    SkillPlans,
    /// `skills.yaml`: skill categories and per-hull skill tiers.
    Skills,
    /// `tags.yaml`: tags visible to every pilot.
    Tags,
    /// `rules.yaml`: community rules for the fit checker, x-up and skill plans. Optional: when
    /// it is absent, built-in defaults reproducing the legacy waitlist are used.
    Rules,
}

impl DataFile {
    pub const ALL: [Self; 9] = [
        Self::Categories,
        Self::Fits,
        Self::FitNotes,
        Self::Implants,
        Self::Modules,
        Self::SkillPlans,
        Self::Skills,
        Self::Tags,
        Self::Rules,
    ];

    /// The file name inside the data directory.
    pub fn file_name(self) -> &'static str {
        match self {
            Self::Categories => "categories.yaml",
            Self::Fits => "fits.dat",
            Self::FitNotes => "fitnotes.yaml",
            Self::Implants => "implants.yaml",
            Self::Modules => "modules.yaml",
            Self::SkillPlans => "skillplan.yaml",
            Self::Skills => "skills.yaml",
            Self::Tags => "tags.yaml",
            Self::Rules => "rules.yaml",
        }
    }

    pub fn from_file_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|file| file.file_name() == name)
    }

    /// Whether the legacy admin API reports this file as needing a reload after editing
    /// (`requires_reload` in `GET /api/admin/data-files`). Every file is reloaded after a write
    /// here, but the flag is part of the API.
    pub fn legacy_requires_reload(self) -> bool {
        matches!(
            self,
            Self::Skills | Self::Categories | Self::Modules | Self::Tags | Self::Fits
        )
    }

    /// Whether the legacy admin API lists and edits this file.
    pub fn legacy_editable(self) -> bool {
        self != Self::Rules
    }

    /// Whether the data set can be loaded without this file.
    pub fn optional(self) -> bool {
        self == Self::Rules
    }
}

impl std::fmt::Display for DataFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.file_name())
    }
}

/// The built-in `rules.yaml`, used when the data directory has none.
pub const DEFAULT_RULES: &str = include_str!("../defaults/rules.yaml");

/// Raw contents of every data file.
#[derive(Debug, Clone, Default)]
pub struct DataSources {
    files: BTreeMap<DataFile, String>,
}

impl DataSources {
    /// Reads every data file from a directory. A missing `rules.yaml` falls back to
    /// [`DEFAULT_RULES`].
    pub fn read_dir(dir: &Path) -> Result<Self, DataError> {
        let mut files = BTreeMap::new();
        for file in DataFile::ALL {
            let path = dir.join(file.file_name());
            match std::fs::read_to_string(&path) {
                Ok(content) => {
                    files.insert(file, content);
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound && file.optional() => {
                    tracing::info!(
                        file = file.file_name(),
                        "data file not found, using the built-in default"
                    );
                }
                Err(source) => return Err(DataError::Read { path, source }),
            }
        }
        Ok(Self { files })
    }

    /// Builds sources from in-memory contents.
    pub fn from_contents(contents: impl IntoIterator<Item = (DataFile, String)>) -> Self {
        Self {
            files: contents.into_iter().collect(),
        }
    }

    /// Replaces the contents of one file.
    pub fn set(&mut self, file: DataFile, content: impl Into<String>) {
        self.files.insert(file, content.into());
    }

    /// The contents of a file as read or set, `None` for an absent optional file.
    pub(crate) fn raw(&self, file: DataFile) -> Option<&str> {
        self.files.get(&file).map(String::as_str)
    }

    /// The contents of a file (the built-in default for an absent optional file).
    pub fn get(&self, file: DataFile) -> Result<&str, DataError> {
        match (self.files.get(&file), file) {
            (Some(content), _) => Ok(content),
            (None, DataFile::Rules) => Ok(DEFAULT_RULES),
            (None, file) => Err(DataError::invalid(file.file_name(), "file is missing")),
        }
    }
}
