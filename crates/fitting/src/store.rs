//! The live data set: loaded from the data directory, swapped atomically on reload.

use std::{
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, PoisonError},
    time::{SystemTime, UNIX_EPOCH},
};

use arc_swap::ArcSwap;
use wl_eve_data::EveData;

use crate::{DEFAULT_RULES, DataError, DataFile, DataSet, DataSources};

/// Backups kept per data file when it is replaced.
const BACKUPS_KEPT: usize = 5;

/// The current [`DataSet`], loaded from the data directory.
///
/// Readers take a snapshot with [`DataStore::get`] and never block. Writers (reload, file
/// replacement) build a complete new data set first and only swap it in once it loaded without
/// errors, so a bad file never replaces a good data set.
pub struct DataStore {
    dir: PathBuf,
    eve: Arc<EveData>,
    current: ArcSwap<DataSet>,
    /// Serializes writers so reloads and file replacements do not interleave.
    writer: Mutex<()>,
}

/// Outcome of validating a data file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Validation {
    /// Problems that do not prevent saving (for example skill plans that fail to build).
    pub warnings: Vec<String>,
}

impl DataStore {
    /// Loads every data file from `dir`.
    pub fn open(eve: Arc<EveData>, dir: impl Into<PathBuf>) -> Result<Self, DataError> {
        let dir = dir.into();
        let data = DataSet::load(eve.clone(), &DataSources::read_dir(&dir)?)?;
        tracing::info!(
            dir = %dir.display(),
            doctrine_fits = data.fits.iter().count(),
            skill_plans = data.skill_plans.response.plans.len(),
            "loaded data files"
        );
        Ok(Self {
            dir,
            eve,
            current: ArcSwap::from_pointee(data),
            writer: Mutex::new(()),
        })
    }

    /// A snapshot of the current data set.
    pub fn get(&self) -> Arc<DataSet> {
        self.current.load_full()
    }

    /// The static data the data files are resolved against.
    pub fn eve(&self) -> &Arc<EveData> {
        &self.eve
    }

    /// The data directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Path of a data file.
    pub fn path(&self, file: DataFile) -> PathBuf {
        self.dir.join(file.file_name())
    }

    /// Contents of a data file (the built-in default for an absent `rules.yaml`).
    pub fn read_file(&self, file: DataFile) -> Result<String, DataError> {
        let path = self.path(file);
        match std::fs::read_to_string(&path) {
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound && file == DataFile::Rules =>
            {
                Ok(DEFAULT_RULES.to_string())
            }
            result => result.map_err(|source| DataError::Read { path, source }),
        }
    }

    /// Re-reads every data file. On error the current data set stays in place.
    pub fn reload(&self) -> Result<Arc<DataSet>, DataError> {
        let _writer = self.writer.lock().unwrap_or_else(PoisonError::into_inner);
        let data = Arc::new(DataSet::load(
            self.eve.clone(),
            &DataSources::read_dir(&self.dir)?,
        )?);
        self.current.store(data.clone());
        tracing::info!(dir = %self.dir.display(), "reloaded data files");
        Ok(data)
    }

    /// Checks that the data set would load with `content` as `file`, without writing anything.
    pub fn validate(&self, file: DataFile, content: &str) -> Result<Validation, DataError> {
        let mut sources = DataSources::read_dir(&self.dir)?;
        sources.set(file, content);
        let data = DataSet::load(self.eve.clone(), &sources)?;
        Ok(Validation {
            warnings: data.warnings,
        })
    }

    /// Validates `content` as `file`, then backs up the current file, writes the new one
    /// atomically and swaps in the new data set. Nothing is written if validation fails.
    pub fn replace_file(&self, file: DataFile, content: &str) -> Result<Validation, DataError> {
        let _writer = self.writer.lock().unwrap_or_else(PoisonError::into_inner);
        let mut sources = DataSources::read_dir(&self.dir)?;
        sources.set(file, content);
        let data = Arc::new(DataSet::load(self.eve.clone(), &sources)?);

        let path = self.path(file);
        backup(&self.dir, file)?;
        write_atomically(&path, content)?;
        self.current.store(data.clone());
        tracing::info!(file = file.file_name(), "data file replaced");
        Ok(Validation {
            warnings: data.warnings.clone(),
        })
    }
}

impl std::fmt::Debug for DataStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DataStore")
            .field("dir", &self.dir)
            .finish_non_exhaustive()
    }
}

fn write_atomically(path: &Path, content: &str) -> Result<(), DataError> {
    let write_error = |source| DataError::Write {
        path: path.to_path_buf(),
        source,
    };
    let mut temp_name = path.as_os_str().to_os_string();
    temp_name.push(".tmp");
    let temp = PathBuf::from(temp_name);
    let mut file = std::fs::File::create(&temp).map_err(write_error)?;
    file.write_all(content.as_bytes()).map_err(write_error)?;
    file.sync_all().map_err(write_error)?;
    std::fs::rename(&temp, path).map_err(write_error)
}

/// Copies the current file to `<name>.backup.<unix seconds>` and keeps the newest backups.
fn backup(dir: &Path, file: DataFile) -> Result<(), DataError> {
    let source = dir.join(file.file_name());
    if !source.exists() {
        return Ok(());
    }
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default();
    let prefix = format!("{}.backup.", file.file_name());
    let target = dir.join(format!("{prefix}{timestamp}"));
    std::fs::copy(&source, &target).map_err(|source| DataError::Write {
        path: target.clone(),
        source,
    })?;

    let entries = std::fs::read_dir(dir).map_err(|source| DataError::Read {
        path: dir.to_path_buf(),
        source,
    })?;
    let mut backups: Vec<(u64, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name();
            let stamp = name.to_str()?.strip_prefix(&prefix)?.parse().ok()?;
            Some((stamp, entry.path()))
        })
        .collect();
    backups.sort_by_key(|backup| std::cmp::Reverse(backup.0));
    for (_, old) in backups.into_iter().skip(BACKUPS_KEPT) {
        // Best effort: a stale backup left behind is harmless.
        let _ = std::fs::remove_file(old);
    }
    Ok(())
}
