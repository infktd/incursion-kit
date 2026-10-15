//! Structured editing of the community data files, for guided editors (forms instead of YAML).
//!
//! Each data file has a typed model that is read from the current file and written back in the
//! file's own format, so files written here stay readable by older servers. Every edit runs
//! through [`crate::DataStore::edit`]: the edit is checked here first (with field paths into the
//! request), then the whole data set must load with the new file before anything is written.
//!
//! YAML files are re-serialized from the model: comments other than the file's leading comment
//! block are lost (the block is kept), keys are written in the file format's order, and YAML
//! anchors and merge keys are written out in full. `fits.dat` is edited in place: only the
//! changed fitting link is touched, the rest of the file (headers, colours) stays as it was.

use serde::{Deserialize, Serialize};
use wl_eve_data::{EveData, TypeId};

use crate::{DataError, DataFile, yaml};

mod categories;
mod fits;
mod implants;
mod notes;
mod rules;
mod tags;
mod types;

pub use categories::{CategoriesDocument, CategoryRuleEntry};
pub use fits::{
    DoctrineFitInput, DoctrineFitView, EftParse, FitItem, FitLayout, FitSelector, FitWrite,
    create_fit, delete_fit, doctrine_fits, parse_eft, render_eft, update_fit, write_fit,
};
pub use implants::{ImplantSetInput, create_implant_set, delete_implant_set, update_implant_set};
pub use notes::{FitNoteView, NoteTarget, create_note, delete_note, fit_notes, update_note};
pub use rules::{
    BadgeApprovalEntry, FitCheckSection, FleetSection, ImplantModuleRuleEntry, MotdRoleEntry,
    RulesDocument, SkillPlansSection, TankLevelEntry, TimeInFleetLimitEntry, read_rules,
    write_rules,
};
pub use tags::{TagsDocument, read_tags, write_tags};
pub use types::{TypeKind, TypeSearchHit, search_types};

pub use categories::{read_categories, write_categories};

/// A problem with one field of a structured edit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct FieldError {
    /// Where the problem is, as a path into the request body (`name`,
    /// `slots[2].default[0].name`, `rules[3].item`); empty when it concerns the request as a
    /// whole or the data set it would produce.
    pub path: String,
    pub message: String,
    /// 1-based line, for problems inside multi-line text (EFT).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
}

impl FieldError {
    /// A problem that concerns no particular field (`path` empty), e.g. from loading the data
    /// set.
    pub fn general(message: impl Into<String>) -> Self {
        Self {
            path: String::new(),
            message: message.into(),
            line: None,
        }
    }

    /// The problem as one line of text: `line N: message` for a problem with a line,
    /// `path: message` for one with a path, else the message.
    pub fn text(&self) -> String {
        match (self.line, self.path.as_str()) {
            (Some(line), _) => format!("line {line}: {}", self.message),
            (None, "") => self.message.clone(),
            (None, path) => format!("{path}: {}", self.message),
        }
    }
}

/// Errors and warnings collected while checking an edit.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Problems {
    pub errors: Vec<FieldError>,
    /// Problems that do not prevent saving, with their field path (and line in EFT text).
    pub warnings: Vec<FieldError>,
}

impl Problems {
    pub fn error(&mut self, path: impl Into<String>, message: impl Into<String>) {
        self.errors.push(FieldError {
            path: path.into(),
            message: message.into(),
            line: None,
        });
    }

    pub fn line_error(&mut self, path: &str, line: usize, message: impl Into<String>) {
        self.errors.push(FieldError {
            path: path.to_string(),
            message: message.into(),
            line: Some(line),
        });
    }

    pub fn warn(&mut self, path: impl Into<String>, message: impl Into<String>) {
        self.warnings.push(FieldError {
            path: path.into(),
            message: message.into(),
            line: None,
        });
    }

    pub fn line_warn(&mut self, path: &str, line: usize, message: impl Into<String>) {
        self.warnings.push(FieldError {
            path: path.to_string(),
            message: message.into(),
            line: Some(line),
        });
    }

    pub fn has_errors(&self) -> bool {
        !self.errors.is_empty()
    }

    /// `value` with the warnings, or the problems when there are errors.
    pub(crate) fn finish<T>(self, value: T) -> Result<(T, Vec<FieldError>), Rejection> {
        if self.has_errors() {
            Err(Rejection::Invalid(self))
        } else {
            Ok((value, self.warnings))
        }
    }
}

/// Why an edit was refused before the data set was loaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rejection {
    /// The item to change does not exist.
    NotFound(String),
    /// The request has errors.
    Invalid(Problems),
}

impl From<Problems> for Rejection {
    fn from(problems: Problems) -> Self {
        Self::Invalid(problems)
    }
}

impl Rejection {
    fn invalid(path: impl Into<String>, message: impl Into<String>) -> Self {
        let mut problems = Problems::default();
        problems.error(path, message);
        Self::Invalid(problems)
    }
}

/// The result of an edit: the new file contents and warnings about the request.
pub type Edited = Result<(String, Vec<FieldError>), Rejection>;

/// The errors a data set that does not load reports, as field errors without a path.
pub fn data_error_fields(error: &DataError) -> Vec<FieldError> {
    let field = FieldError::general;
    match error {
        DataError::MissingTypes(missing) => missing
            .0
            .iter()
            .map(|missing| {
                field(format!(
                    "unknown type name {:?} (used in {})",
                    missing.name,
                    missing.used_in.join("; ")
                ))
            })
            .collect(),
        other => vec![field(other.to_string())],
    }
}

/// `Some(None)` for an explicit `null`, `None` when the field is absent.
fn double_option<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

/// A type with its name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct TypeRef {
    #[schema(value_type = i32)]
    pub id: TypeId,
    pub name: String,
}

impl TypeRef {
    pub(crate) fn of(eve: &EveData, id: TypeId) -> Self {
        Self {
            id,
            name: eve.name_of(id).unwrap_or_default().to_string(),
        }
    }
}

/// Renders `value` as YAML for `file`, keeping the leading comment block of `original` (the
/// comments and blank lines before the first YAML content).
pub(crate) fn write_yaml<T: Serialize>(
    file: DataFile,
    original: &str,
    value: &T,
) -> Result<String, DataError> {
    let body = yaml::to_string_indented(file, value)?;
    let header = leading_comments(original);
    if header.is_empty() {
        Ok(body)
    } else {
        Ok(format!("{header}\n{body}"))
    }
}

/// The comment lines (and blank lines between them) at the top of a YAML file, without trailing
/// blank lines; empty when the file does not start with a comment.
fn leading_comments(content: &str) -> String {
    let mut lines = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') || trimmed.is_empty() {
            lines.push(line.trim_end());
        } else {
            break;
        }
    }
    while lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    if lines.is_empty() {
        String::new()
    } else {
        lines.join("\n") + "\n"
    }
}

/// Checks a name used as an identifier in a data file: not empty, no surrounding whitespace, no
/// line breaks, at most `max` characters.
pub(crate) fn check_name(problems: &mut Problems, path: &str, name: &str, max: usize) -> bool {
    let message = if name.trim().is_empty() {
        "must not be empty".to_string()
    } else if name.trim() != name {
        "must not start or end with whitespace".to_string()
    } else if name.chars().any(char::is_control) {
        "must not contain line breaks or control characters".to_string()
    } else if name.chars().count() > max {
        format!("must be at most {max} characters")
    } else {
        return true;
    };
    problems.error(path, message);
    false
}

/// Resolves a type name for a field, reporting unknown names.
pub(crate) fn resolve(
    eve: &EveData,
    problems: &mut Problems,
    path: &str,
    name: &str,
) -> Option<TypeId> {
    if name.trim().is_empty() {
        problems.error(path, "must not be empty");
        return None;
    }
    let id = eve.id_of(name);
    if id.is_none() {
        problems.error(path, format!("unknown type {name:?}"));
    }
    id
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_leading_comment_block() {
        assert_eq!(
            leading_comments("# a\n#b\n\n# c\n\nkey: 1\n# not this\n"),
            "# a\n#b\n\n# c\n"
        );
        assert_eq!(leading_comments("---\n# x\nkey: 1\n"), "");
        assert_eq!(leading_comments("key: 1\n"), "");
    }

    #[test]
    fn names_are_checked() {
        let mut problems = Problems::default();
        assert!(check_name(&mut problems, "a", "ok", 10));
        assert!(!check_name(&mut problems, "b", " x", 10));
        assert!(!check_name(&mut problems, "c", "", 10));
        assert!(!check_name(&mut problems, "d", "x\ny", 10));
        assert!(!check_name(&mut problems, "e", "abcdefghijk", 10));
        let paths: Vec<&str> = problems.errors.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, ["b", "c", "d", "e"]);
    }
}
