//! The public fittings page: doctrine fits as DNA, `fitnotes.yaml`, `implants.yaml`, and
//! `tags.yaml` (tags every pilot may see).

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use wl_eve_data::{EveData, TypeId};

use crate::{Categories, DataError, DataFile, DoctrineFits, FitError, rules::FittingsRules, yaml};

/// A note shown with a doctrine fit (matched by fit name) or a fits page tab.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, utoipa::ToSchema)]
pub struct FittingNote {
    pub name: String,
    /// Markdown.
    pub description: String,
}

#[derive(Deserialize)]
struct NoteFile {
    notes: Vec<FittingNote>,
}

/// One implant in the implant guide.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, utoipa::ToSchema)]
pub struct ImplantEntry {
    /// The implant as shown (and copied) on the fits page: its type name or a short form such
    /// as `WS-618`.
    pub name: String,
    /// Free-text remark shown with the implant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Label grouping alternatives together.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
}

/// Recommended implants for one slot.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, utoipa::ToSchema)]
pub struct ImplantSlot {
    /// Implant slot number (1-10).
    pub slot: u8,
    /// The recommended implants.
    #[serde(default)]
    pub default: Vec<ImplantEntry>,
    /// Acceptable alternatives.
    #[serde(default)]
    pub alternative: Vec<ImplantEntry>,
}

/// A named set of implant recommendations.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, utoipa::ToSchema)]
pub struct ImplantSet {
    pub name: String,
    pub slots: Vec<ImplantSlot>,
}

#[derive(Deserialize)]
struct ImplantFile {
    sets: Vec<ImplantSet>,
}

#[derive(Deserialize)]
struct TagFile {
    public_tags: Vec<String>,
}

pub(crate) fn load_notes(content: &str) -> Result<Vec<FittingNote>, DataError> {
    Ok(yaml::parse::<NoteFile>(DataFile::FitNotes, content)?.notes)
}

pub(crate) fn load_implants(content: &str) -> Result<Vec<ImplantSet>, DataError> {
    Ok(yaml::parse::<ImplantFile>(DataFile::Implants, content)?.sets)
}

pub(crate) fn load_public_tags(content: &str) -> Result<BTreeSet<String>, DataError> {
    Ok(yaml::parse::<TagFile>(DataFile::Tags, content)?
        .public_tags
        .into_iter()
        .collect())
}

/// A doctrine fit as DNA.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct DnaFitting {
    /// Fit name, as in `fits.dat`.
    pub name: String,
    /// The fit in EVE ship DNA format (`hull:module;count:...::`).
    pub dna: String,
}

/// `GET /api/fittings`. Every field is always present; they are optional only on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct FittingsResponse {
    /// The doctrine fits, without hidden alternatives.
    pub fittingdata: Option<Vec<DnaFitting>>,
    /// `fitnotes.yaml`.
    pub notes: Option<Vec<FittingNote>>,
    /// The implant guide (`implants.yaml`).
    pub implants: Option<Vec<ImplantSet>>,
    /// Type ids of the items that put a fit in the configured category (logistics by default).
    #[schema(value_type = Option<Vec<i32>>)]
    pub rules: Option<Vec<TypeId>>,
}

pub(crate) fn fittings_response(
    eve: &EveData,
    fits: &DoctrineFits,
    categories: &Categories,
    notes: &[FittingNote],
    implants: &[ImplantSet],
    rules: &FittingsRules,
) -> Result<FittingsResponse, FitError> {
    let fittingdata = fits
        .iter()
        .filter(|fit| !fit.name.contains(&rules.hidden_fit_marker))
        .map(|fit| {
            Ok(DnaFitting {
                name: fit.name.clone(),
                dna: fit.fit.to_dna(eve)?,
            })
        })
        .collect::<Result<_, FitError>>()?;
    Ok(FittingsResponse {
        fittingdata: Some(fittingdata),
        notes: Some(notes.to_vec()),
        implants: Some(implants.to_vec()),
        rules: Some(categories.rule_items(&rules.rules_category)),
    })
}
