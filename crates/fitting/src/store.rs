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

/// Called after the data set changed: `Some(file)` when one file was written, `None` after a
/// reload of every file.
type Listener = Arc<dyn Fn(Option<DataFile>) + Send + Sync>;

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
    /// Told about every change (see [`DataStore::on_change`]).
    listener: Mutex<Option<Listener>>,
}

/// A structured edit failed: the edit itself rejected the change, or the data set does not load
/// with the edited file (or a file could not be read or written).
#[derive(Debug)]
pub enum EditError<E> {
    Edit(E),
    Data(DataError),
}

impl<E> From<DataError> for EditError<E> {
    fn from(error: DataError) -> Self {
        Self::Data(error)
    }
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
            listener: Mutex::new(None),
        })
    }

    /// Calls `listener` after every change of the data set: with the file after a file was
    /// written ([`DataStore::replace_file`], [`DataStore::edit`]), with `None` after
    /// [`DataStore::reload`]. Replaces an earlier listener. The listener runs on the writing
    /// thread and must not block.
    pub fn on_change(&self, listener: impl Fn(Option<DataFile>) + Send + Sync + 'static) {
        *self.listener.lock().unwrap_or_else(PoisonError::into_inner) = Some(Arc::new(listener));
    }

    fn changed(&self, file: Option<DataFile>) {
        let listener = self
            .listener
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        if let Some(listener) = listener {
            listener(file);
        }
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
        drop(_writer);
        self.changed(None);
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
        self.edit(file, false, |_, _| {
            Ok::<_, DataError>((content.to_string(), ()))
        })
        .map(|(validation, ())| validation)
        .map_err(|error| match error {
            EditError::Edit(error) | EditError::Data(error) => error,
        })
    }

    /// A read-modify-write of one data file, serialized with every other writer.
    ///
    /// `edit` gets the file's current contents (from disk; the built-in default for an absent
    /// `rules.yaml`) and the current data set, and returns the new contents plus a value passed
    /// back to the caller. The data set must load with the new contents; then, unless `dry_run`,
    /// the current file is backed up, the new one written atomically, the new data set swapped in
    /// and the change listener told. With `dry_run` nothing is written.
    pub fn edit<T, E>(
        &self,
        file: DataFile,
        dry_run: bool,
        edit: impl FnOnce(&str, &DataSet) -> Result<(String, T), E>,
    ) -> Result<(Validation, T), EditError<E>> {
        self.commit(dry_run, Some(file), |sources, data| {
            let (content, value) = edit(sources.get(file)?, data).map_err(EditError::Edit)?;
            sources.set(file, content);
            Ok(value)
        })
    }

    /// A read-modify-write of several data files at once (a doctrine fit and its implant set
    /// link), serialized with every other writer.
    ///
    /// `edit` gets every file's current contents (from disk) to change in place, and the current
    /// data set. The data set must load with every change; then, unless `dry_run`, each file
    /// whose contents changed is backed up and written atomically (one after the other), the new
    /// data set swapped in and the change listener told once per written file.
    pub fn edit_files<T, E>(
        &self,
        dry_run: bool,
        edit: impl FnOnce(&mut DataSources, &DataSet) -> Result<T, EditError<E>>,
    ) -> Result<(Validation, T), EditError<E>> {
        self.commit(dry_run, None, edit)
    }

    /// Runs `edit` on the sources, loads the data set and writes the changed files (and
    /// `always`, changed or not).
    fn commit<T, E>(
        &self,
        dry_run: bool,
        always: Option<DataFile>,
        edit: impl FnOnce(&mut DataSources, &DataSet) -> Result<T, EditError<E>>,
    ) -> Result<(Validation, T), EditError<E>> {
        let writer = self.writer.lock().unwrap_or_else(PoisonError::into_inner);
        let original = DataSources::read_dir(&self.dir)?;
        let mut sources = original.clone();
        let current = self.get();
        let value = edit(&mut sources, &current)?;
        let data = Arc::new(DataSet::load(self.eve.clone(), &sources)?);
        let validation = Validation {
            warnings: data.warnings.clone(),
        };
        if dry_run {
            return Ok((validation, value));
        }

        let written: Vec<DataFile> = DataFile::ALL
            .into_iter()
            .filter(|&file| Some(file) == always || sources.raw(file) != original.raw(file))
            .collect();
        for &file in &written {
            backup(&self.dir, file)?;
            write_atomically(&self.path(file), sources.get(file)?)?;
            tracing::info!(file = file.file_name(), "data file replaced");
        }
        self.current.store(data);
        drop(writer);
        for file in written {
            self.changed(Some(file));
        }
        Ok((validation, value))
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
