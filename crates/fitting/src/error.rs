//! Errors raised while loading, validating or writing data files.

use std::path::PathBuf;

use wl_eve_data::MissingTypes;

/// A data file could not be read, parsed or written, or refers to things that do not exist.
#[derive(Debug, thiserror::Error)]
pub enum DataError {
    #[error("{}: could not read: {source}", .path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{}: could not write: {source}", .path.display())]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{file}: invalid YAML: {message}")]
    Yaml { file: &'static str, message: String },
    #[error("{file}: {message}")]
    Invalid { file: &'static str, message: String },
    #[error("data files refer to {0}")]
    MissingTypes(#[from] MissingTypes),
}

impl DataError {
    pub(crate) fn invalid(file: &'static str, message: impl Into<String>) -> Self {
        Self::Invalid {
            file,
            message: message.into(),
        }
    }
}
