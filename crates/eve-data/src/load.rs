//! Loading [`EveData`] from the static data file.

use std::path::Path;

use crate::{EveData, EveDataError, sde};

impl EveData {
    /// Loads the static data file built by [`sde::build`] (or a legacy `sqlite-shrunk.sqlite`).
    pub async fn load(path: impl AsRef<Path>) -> Result<Self, EveDataError> {
        let path = path.as_ref();
        if !path.is_file() {
            return Err(EveDataError::Missing(path.to_path_buf()));
        }
        let (tables, map, info) =
            sde::read_tables(path)
                .await
                .map_err(|source| EveDataError::Database {
                    path: path.to_path_buf(),
                    source,
                })?;
        if tables.types.is_empty() {
            return Err(EveDataError::Empty(path.to_path_buf()));
        }
        let data = Self::from_tables(tables, info).with_universe(crate::Universe::from_tables(map));
        if data.universe().is_empty() {
            tracing::warn!(
                path = %path.display(),
                "the static data file has no star map (built before map support); incursion \
                 enrichment is limited until it is rebuilt with `waitlist sde`"
            );
        }
        match data.info() {
            Some(info) => tracing::info!(
                path = %path.display(),
                types = data.type_count(),
                sde_build = info.build_number,
                sde_release = info.release_date.as_deref().unwrap_or("unknown"),
                "loaded static data"
            ),
            None => tracing::info!(
                path = %path.display(),
                types = data.type_count(),
                "loaded static data (SDE build not recorded)"
            ),
        }
        Ok(data)
    }
}
