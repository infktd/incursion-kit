//! Shared test helpers: the real static data file and the example data set.
//!
//! Tests that need the static data skip (with a message) when `deploy/sde/sde.sqlite` (or
//! `WL_SDE_PATH`) is absent; build it with `cargo run -p wl-eve-data --example build_sde`.

#![allow(dead_code)]

use std::{
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
};

use wl_eve_data::{EveData, TypeId};
use wl_fitting::{DataStore, Fitting};

/// The real static data, loaded once per test binary.
pub fn eve() -> Option<Arc<EveData>> {
    static EVE: OnceLock<Option<Arc<EveData>>> = OnceLock::new();
    EVE.get_or_init(|| {
        let path = std::env::var_os("WL_SDE_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../deploy/sde/sde.sqlite")
            });
        if !path.exists() {
            return None;
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("tokio runtime");
        Some(Arc::new(
            runtime
                .block_on(EveData::load(&path))
                .expect("static data file loads"),
        ))
    })
    .clone()
    .or_else(|| {
        eprintln!(
            "SKIPPED: static data file not found; build it with \
             `cargo run -p wl-eve-data --example build_sde`"
        );
        None
    })
}

/// The bundled example data set (`example-data/`) in a fresh directory.
pub fn example_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp dir");
    wl_fitting::example_data::seed(dir.path(), true).expect("write example data");
    dir
}

/// A data store over a copy of the example data set.
pub fn example_store() -> Option<(tempfile::TempDir, DataStore)> {
    let eve = eve()?;
    let dir = example_dir();
    let store = DataStore::open(eve, dir.path()).expect("example data loads");
    Some((dir, store))
}

pub fn id(eve: &EveData, name: &str) -> TypeId {
    eve.id_of(name)
        .unwrap_or_else(|| panic!("{name:?} not in the static data"))
}

pub fn eft(eve: &EveData, text: &str) -> Fitting {
    let mut fits = Fitting::from_eft(text, eve).expect("EFT parses");
    assert_eq!(fits.len(), 1);
    fits.remove(0)
}

pub fn read(dir: &Path, name: &str) -> String {
    std::fs::read_to_string(dir.join(name)).expect("read data file")
}
