//! Implant sets (`implants.yaml`): the implant guide of the fits page.

use std::collections::BTreeSet;

use serde::Serialize;
use wl_eve_data::{Category, EveData};

use super::{Edited, Problems, Rejection, check_name, write_yaml};
use crate::{DataFile, ImplantEntry, ImplantSet, fittings};

const MAX_NAME: usize = 100;

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
            problems.warn(format!("{path}.name: {:?} is not an implant", entry.name));
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

fn check(eve: &EveData, set: &ImplantSet, sets: &[ImplantSet], except: Option<usize>) -> Problems {
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
    problems
}

fn current(content: &str) -> Result<Vec<ImplantSet>, Rejection> {
    fittings::load_implants(content).map_err(|error| Rejection::invalid("", error.to_string()))
}

fn save(content: &str, sets: &[ImplantSet], problems: Problems) -> Edited {
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
pub fn create_implant_set(content: &str, eve: &EveData, set: &ImplantSet) -> Edited {
    let mut sets = current(content)?;
    let problems = check(eve, set, &sets, None);
    sets.push(set.clone());
    save(content, &sets, problems)
}

/// Replaces (and possibly renames) the implant set `name`.
pub fn update_implant_set(content: &str, eve: &EveData, name: &str, set: &ImplantSet) -> Edited {
    let mut sets = current(content)?;
    let index = position(&sets, name)?;
    let problems = check(eve, set, &sets, Some(index));
    sets[index] = set.clone();
    save(content, &sets, problems)
}

/// Removes the implant set `name`.
pub fn delete_implant_set(content: &str, name: &str) -> Edited {
    let mut sets = current(content)?;
    let index = position(&sets, name)?;
    sets.remove(index);
    save(content, &sets, Problems::default())
}
