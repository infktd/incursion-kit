//! `fits.dat`: the doctrine fits, as pasted from an in-game fitting link dump
//! (`<a href="fitting:DNA">NAME</a>`; everything else in the file is ignored).

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::LazyLock,
};

use regex::Regex;
use wl_eve_data::{EveData, TypeId};

use crate::{DataError, Fitting};

pub(crate) static FIT_LINK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"<a href="fitting:([0-9:;_]+)">([^<]+)</a>"#).expect("valid fit link pattern")
});

/// A named doctrine fit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctrineFit {
    pub name: String,
    pub fit: Fitting,
}

/// Doctrine fits grouped by hull, in file order within each hull.
#[derive(Debug, Clone, Default)]
pub struct DoctrineFits {
    by_hull: BTreeMap<TypeId, Vec<DoctrineFit>>,
}

impl DoctrineFits {
    pub(crate) fn load(content: &str, eve: &EveData) -> Result<Self, DataError> {
        let mut by_hull: BTreeMap<TypeId, Vec<DoctrineFit>> = BTreeMap::new();
        let mut problems = Vec::new();

        for link in FIT_LINK.captures_iter(content) {
            let (_, [dna, name]) = link.extract();
            let parsed = Fitting::from_dna(dna, eve).and_then(|fit| {
                fit.validate(eve)?;
                fit.to_dna(eve)?;
                Ok(fit)
            });
            match parsed {
                Ok(fit) => by_hull.entry(fit.hull).or_default().push(DoctrineFit {
                    name: name.to_string(),
                    fit,
                }),
                Err(error) => problems.push(format!("fit {name:?} ({dna}): {error}")),
            }
        }

        if problems.is_empty() {
            Ok(Self { by_hull })
        } else {
            Err(DataError::invalid("fits.dat", problems.join("; ")))
        }
    }

    /// Doctrine fits for a hull, in file order.
    pub fn for_hull(&self, hull: TypeId) -> &[DoctrineFit] {
        self.by_hull
            .get(&hull)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    /// The doctrine fit with this name on this hull.
    pub fn find(&self, hull: TypeId, name: &str) -> Option<&DoctrineFit> {
        self.for_hull(hull).iter().find(|fit| fit.name == name)
    }

    /// Every doctrine fit, by hull id then file order.
    pub fn iter(&self) -> impl Iterator<Item = &DoctrineFit> {
        self.by_hull.values().flatten()
    }

    /// Hulls that have doctrine fits.
    pub fn hulls(&self) -> impl Iterator<Item = TypeId> + '_ {
        self.by_hull.keys().copied()
    }

    /// Every hull, module and cargo type used by a doctrine fit.
    pub fn used_type_ids(&self) -> BTreeSet<TypeId> {
        self.iter().flat_map(|fit| fit.fit.type_ids()).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.by_hull.is_empty()
    }
}
