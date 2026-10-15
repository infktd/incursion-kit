//! Fit notes (`fitnotes.yaml`): Markdown notes shown with a doctrine fit (by fit name) or a tier
//! of the fits page (by tier name).

use serde::Serialize;

use super::{Edited, Problems, Rejection, check_name, write_yaml};
use crate::{DataFile, DataSet, FittingNote, fittings};

const MAX_NAME: usize = 100;

/// What a note is shown with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum NoteTarget {
    /// A doctrine fit of that name exists.
    Fit,
    /// No doctrine fit has the name: a tier of the fits page (as named in its URL).
    Tier,
}

/// A fit note as the editor shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct FitNoteView {
    /// Fit name or tier name.
    pub name: String,
    /// Markdown.
    pub description: String,
    pub target: NoteTarget,
}

#[derive(Serialize)]
struct NoteFile<'a> {
    notes: &'a [FittingNote],
}

/// Every note of the current data set, in file order.
pub fn fit_notes(data: &DataSet) -> Vec<FitNoteView> {
    data.fit_notes
        .iter()
        .map(|note| FitNoteView {
            target: if data.fits.iter().any(|fit| fit.name == note.name) {
                NoteTarget::Fit
            } else {
                NoteTarget::Tier
            },
            name: note.name.clone(),
            description: note.description.clone(),
        })
        .collect()
}

fn check(note: &FittingNote, notes: &[FittingNote], except: Option<usize>) -> Problems {
    let mut problems = Problems::default();
    check_name(&mut problems, "name", &note.name, MAX_NAME);
    if notes
        .iter()
        .enumerate()
        .any(|(index, other)| Some(index) != except && other.name == note.name)
    {
        problems.error("name", format!("a note for {:?} already exists", note.name));
    }
    problems
}

fn save(content: &str, notes: &[FittingNote], problems: Problems) -> Edited {
    if problems.has_errors() {
        return Err(problems.into());
    }
    let yaml = write_yaml(DataFile::FitNotes, content, &NoteFile { notes })
        .map_err(|error| Rejection::invalid("", error.to_string()))?;
    problems.finish(yaml)
}

fn position(notes: &[FittingNote], name: &str) -> Result<usize, Rejection> {
    notes
        .iter()
        .position(|note| note.name == name)
        .ok_or_else(|| Rejection::NotFound("Fit note not found".to_string()))
}

fn current(content: &str) -> Result<Vec<FittingNote>, Rejection> {
    fittings::load_notes(content).map_err(|error| Rejection::invalid("", error.to_string()))
}

/// Appends a note to `content` (`fitnotes.yaml`).
pub fn create_note(content: &str, note: &FittingNote) -> Edited {
    let mut notes = current(content)?;
    let problems = check(note, &notes, None);
    notes.push(note.clone());
    save(content, &notes, problems)
}

/// Replaces (and possibly renames) the note `name`.
pub fn update_note(content: &str, name: &str, note: &FittingNote) -> Edited {
    let mut notes = current(content)?;
    let index = position(&notes, name)?;
    let problems = check(note, &notes, Some(index));
    notes[index] = note.clone();
    save(content, &notes, problems)
}

/// Removes the note `name`.
pub fn delete_note(content: &str, name: &str) -> Edited {
    let mut notes = current(content)?;
    let index = position(&notes, name)?;
    notes.remove(index);
    save(content, &notes, Problems::default())
}
