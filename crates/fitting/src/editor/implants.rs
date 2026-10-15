//! Implant sets (`implants.yaml`): the implant guide of the fits page.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use wl_eve_data::{Category, EveData};

use super::{Edited, Problems, Rejection, check_name, write_yaml};
use crate::{DataFile, DataSet, ImplantEntry, ImplantSet, ImplantSlot, fittings};

const MAX_NAME: usize = 100;
/// Doctrine fit names are at most this long (see the fits editor).
const MAX_FIT_NAME: usize = 100;

/// An implant set to create, or the new state of one to update.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, utoipa::ToSchema)]
pub struct ImplantSetInput {
    pub name: String,
    pub slots: Vec<ImplantSlot>,
    /// The doctrine fits (by name) that use the set. Absent (or `null`): a new set has none and
    /// an updated one keeps its fits, the way a fit write without `implant_set` keeps the fit's
    /// set; `[]` takes every fit out of the set.
    #[serde(default)]
    pub fits: Option<Vec<String>>,
}

impl ImplantSetInput {
    /// The set to store: `fits` when given, else `keep`.
    fn into_set(self, keep: &[String]) -> ImplantSet {
        ImplantSet {
            name: self.name,
            slots: self.slots,
            fits: self.fits.unwrap_or_else(|| keep.to_vec()),
        }
    }
}

impl From<ImplantSet> for ImplantSetInput {
    fn from(set: ImplantSet) -> Self {
        Self {
            name: set.name,
            slots: set.slots,
            fits: Some(set.fits),
        }
    }
}

#[derive(Serialize)]
struct ImplantFile<'a> {
    sets: &'a [ImplantSet],
}

/// Entry names are display text (the implant's name or a short form such as `WS-618`), so they
/// are not resolved; an exact type name that is not an implant gets a warning.
fn check_entries(eve: &EveData, problems: &mut Problems, path: &str, entries: &[ImplantEntry]) {
    for (index, entry) in entries.iter().enumerate() {
        let path = format!("{path}[{index}]");
        if check_name(problems, &format!("{path}.name"), &entry.name, MAX_NAME)
            && eve
                .by_name(&entry.name)
                .is_some_and(|ty| ty.category != Category::Implant)
        {
            problems.warn(
                format!("{path}.name"),
                format!("{:?} is not an implant", entry.name),
            );
        }
        for (field, value) in [("note", &entry.note), ("group", &entry.group)] {
            if value
                .as_deref()
                .is_some_and(|v| v.chars().any(|c| c == '\n' || c == '\r'))
            {
                problems.error(format!("{path}.{field}"), "must be a single line");
            }
        }
    }
}

fn check(data: &DataSet, set: &ImplantSet, sets: &[ImplantSet], except: Option<usize>) -> Problems {
    let eve = data.eve();
    let mut problems = Problems::default();
    check_name(&mut problems, "name", &set.name, MAX_NAME);
    if sets
        .iter()
        .enumerate()
        .any(|(index, other)| Some(index) != except && other.name == set.name)
    {
        problems.error(
            "name",
            format!("an implant set named {:?} already exists", set.name),
        );
    }
    let mut seen = BTreeSet::new();
    for (index, slot) in set.slots.iter().enumerate() {
        let path = format!("slots[{index}]");
        if !(1..=10).contains(&slot.slot) {
            problems.error(format!("{path}.slot"), "must be between 1 and 10");
        } else if !seen.insert(slot.slot) {
            problems.error(
                format!("{path}.slot"),
                format!("slot {} is listed twice", slot.slot),
            );
        }
        check_entries(
            eve,
            &mut problems,
            &format!("{path}.default"),
            &slot.default,
        );
        check_entries(
            eve,
            &mut problems,
            &format!("{path}.alternative"),
            &slot.alternative,
        );
    }
    let mut listed = BTreeSet::new();
    for (index, fit) in set.fits.iter().enumerate() {
        let path = format!("fits[{index}]");
        if !check_name(&mut problems, &path, fit, MAX_FIT_NAME) {
            continue;
        }
        if !listed.insert(fit.as_str()) {
            problems.error(path, format!("{fit:?} is listed twice"));
            continue;
        }
        if let Some(other) = sets
            .iter()
            .enumerate()
            .find(|(i, other)| Some(*i) != except && other.fits.contains(fit))
            .map(|(_, other)| other)
        {
            problems.error(
                path,
                format!(
                    "{fit:?} already uses the implant set {:?}: take it out of that set first",
                    other.name
                ),
            );
        } else if data.fits.iter().all(|doctrine| doctrine.name != *fit) {
            problems.warn(path, format!("no doctrine fit is named {fit:?}"));
        }
    }
    problems
}

pub(super) fn current(content: &str) -> Result<Vec<ImplantSet>, Rejection> {
    fittings::load_implants(content).map_err(|error| Rejection::invalid("", error.to_string()))
}

pub(super) fn save(content: &str, sets: &[ImplantSet], problems: Problems) -> Edited {
    if problems.has_errors() {
        return Err(problems.into());
    }
    let yaml = write_yaml(DataFile::Implants, content, &ImplantFile { sets })
        .map_err(|error| Rejection::invalid("", error.to_string()))?;
    problems.finish(yaml)
}

fn position(sets: &[ImplantSet], name: &str) -> Result<usize, Rejection> {
    sets.iter()
        .position(|set| set.name == name)
        .ok_or_else(|| Rejection::NotFound("Implant set not found".to_string()))
}

/// Appends an implant set to `content` (`implants.yaml`).
pub fn create_implant_set(content: &str, data: &DataSet, input: &ImplantSetInput) -> Edited {
    let mut sets = current(content)?;
    let set = input.clone().into_set(&[]);
    let problems = check(data, &set, &sets, None);
    sets.push(set);
    save(content, &sets, problems)
}

/// Replaces (and possibly renames) the implant set `name`. Without `fits` the set keeps its
/// fits.
pub fn update_implant_set(
    content: &str,
    data: &DataSet,
    name: &str,
    input: &ImplantSetInput,
) -> Edited {
    let mut sets = current(content)?;
    let index = position(&sets, name)?;
    let set = input.clone().into_set(&sets[index].fits);
    let problems = check(data, &set, &sets, Some(index));
    sets[index] = set;
    save(content, &sets, problems)
}

/// Removes the implant set `name`.
pub fn delete_implant_set(content: &str, name: &str) -> Edited {
    let mut sets = current(content)?;
    let index = position(&sets, name)?;
    sets.remove(index);
    save(content, &sets, Problems::default())
}
