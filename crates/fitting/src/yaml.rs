//! YAML parsing shared by the data files.
//!
//! Data files use YAML merge keys (`<<: [*generic, *armor]` in `skills.yaml`); `serde-saphyr`
//! expands them with standard precedence (keys written in the mapping win, then earlier merged
//! mappings win over later ones), which is what the legacy waitlist's YAML stack did. Duplicate
//! keys keep the last value, as before.

use serde::{Serialize, de::DeserializeOwned};
use serde_saphyr::{DuplicateKeyPolicy, MergeKeyPolicy};

use crate::{DataError, DataFile};

pub(crate) fn parse<T: DeserializeOwned>(file: DataFile, content: &str) -> Result<T, DataError> {
    let options = serde_saphyr::options! {
        duplicate_keys: DuplicateKeyPolicy::LastWins,
        merge_keys: MergeKeyPolicy::Merge,
    };
    serde_saphyr::from_str_with_options(content, options).map_err(|error| DataError::Yaml {
        file: file.file_name(),
        message: error.to_string(),
    })
}

pub(crate) fn to_string<T: Serialize>(file: DataFile, value: &T) -> Result<String, DataError> {
    serde_saphyr::to_string(value).map_err(|error| DataError::Yaml {
        file: file.file_name(),
        message: error.to_string(),
    })
}
