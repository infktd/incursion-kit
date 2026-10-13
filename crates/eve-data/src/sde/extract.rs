//! Reading the parts of the SDE zip the waitlist needs.

use std::{
    collections::HashSet,
    fs::File,
    io::{BufRead, BufReader, Read},
    path::Path,
};

use serde::{Deserialize, de::DeserializeOwned};

use super::{SdeError, SdeInfo, download::VersionLine, io_error, map};
use crate::universe::MapTables;
use crate::{AttributeId, EffectId, EffectRow, MetaRow, TypeId};

const TYPES_FILE: &str = "types.jsonl";
const GROUPS_FILE: &str = "groups.jsonl";
const DOGMA_FILE: &str = "typeDogma.jsonl";
pub(super) const INFO_FILE: &str = "_sde.jsonl";

#[derive(Debug, Clone, PartialEq)]
pub(super) struct ExtractedType {
    pub id: TypeId,
    pub name: String,
    pub group_id: i32,
    pub published: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct ExtractedAttribute {
    pub type_id: TypeId,
    pub attribute_id: i32,
    /// Set when the value is a whole number that fits an `i32` (legacy `valueInt`).
    pub value_int: Option<i32>,
    pub value_float: f64,
}

/// The rows the static data file is made of.
#[derive(Debug, Default)]
pub(super) struct Extracted {
    pub info: Option<SdeInfo>,
    pub types: Vec<ExtractedType>,
    /// `(groupID, categoryID)`.
    pub groups: Vec<(i32, i32)>,
    pub metas: Vec<MetaRow>,
    pub attributes: Vec<ExtractedAttribute>,
    pub effects: Vec<EffectRow>,
    /// The star map; empty when the archive has no map files.
    pub map: MapTables,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TypeLine {
    #[serde(rename = "_key")]
    key: TypeId,
    #[serde(rename = "groupID")]
    group_id: i32,
    #[serde(default)]
    name: Option<LocalizedName>,
    #[serde(default)]
    published: bool,
    #[serde(default, rename = "metaGroupID")]
    meta_group_id: Option<i32>,
    #[serde(default, rename = "variationParentTypeID")]
    variation_parent_type_id: Option<TypeId>,
}

#[derive(Deserialize)]
struct LocalizedName {
    #[serde(default)]
    en: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GroupLine {
    #[serde(rename = "_key")]
    key: i32,
    #[serde(rename = "categoryID")]
    category_id: i32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DogmaLine {
    #[serde(rename = "_key")]
    key: TypeId,
    #[serde(default)]
    dogma_attributes: Vec<DogmaAttribute>,
    #[serde(default)]
    dogma_effects: Vec<DogmaEffect>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DogmaAttribute {
    #[serde(rename = "attributeID")]
    attribute_id: i32,
    value: serde_json::Number,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DogmaEffect {
    #[serde(rename = "effectID")]
    effect_id: i32,
}

/// Reads the waitlist's subset of the SDE from a `*-jsonl.zip` archive.
pub(super) fn extract(archive_path: &Path) -> Result<Extracted, SdeError> {
    let file = File::open(archive_path).map_err(io_error(archive_path))?;
    let mut archive = zip::ZipArchive::new(file)?;
    let mut out = Extracted::default();

    if entry_name(&archive, INFO_FILE).is_some() {
        for_each_line::<VersionLine>(&mut archive, INFO_FILE, |line, _| {
            out.info = Some(line.into());
            Ok(())
        })?;
    }

    let mut skipped_unnamed = 0usize;
    for_each_line::<TypeLine>(&mut archive, TYPES_FILE, |line, _| {
        let Some(name) = line.name.and_then(|name| name.en) else {
            skipped_unnamed += 1;
            return Ok(());
        };
        out.types.push(ExtractedType {
            id: line.key,
            name,
            group_id: line.group_id,
            published: line.published,
        });
        if line.meta_group_id.is_some() || line.variation_parent_type_id.is_some() {
            out.metas.push(MetaRow {
                type_id: line.key,
                parent_type_id: line.variation_parent_type_id,
                meta_group_id: line.meta_group_id,
            });
        }
        Ok(())
    })?;
    if skipped_unnamed > 0 {
        tracing::warn!(
            count = skipped_unnamed,
            "skipped SDE types without an English name"
        );
    }

    for_each_line::<GroupLine>(&mut archive, GROUPS_FILE, |line, _| {
        out.groups.push((line.key, line.category_id));
        Ok(())
    })?;

    let attributes: HashSet<i32> = AttributeId::STORED.iter().map(|a| a.0).collect();
    let effects: HashSet<i32> = EffectId::STORED.iter().map(|e| e.0).collect();
    for_each_line::<DogmaLine>(&mut archive, DOGMA_FILE, |line, number| {
        for attribute in line.dogma_attributes {
            if !attributes.contains(&attribute.attribute_id) {
                continue;
            }
            let (value_int, value_float) =
                attribute_value(&attribute.value).ok_or_else(|| SdeError::Invalid {
                    file: DOGMA_FILE,
                    line: number,
                    message: format!("attribute value {} is not a number", attribute.value),
                })?;
            out.attributes.push(ExtractedAttribute {
                type_id: line.key,
                attribute_id: attribute.attribute_id,
                value_int,
                value_float,
            });
        }
        for effect in line.dogma_effects {
            if effects.contains(&effect.effect_id) {
                out.effects.push(EffectRow {
                    type_id: line.key,
                    effect: EffectId(effect.effect_id),
                });
            }
        }
        Ok(())
    })?;

    out.map = map::extract(&mut archive)?;
    Ok(out)
}

/// Splits a JSON number into the legacy `(valueInt, valueFloat)` pair.
fn attribute_value(value: &serde_json::Number) -> Option<(Option<i32>, f64)> {
    let float = value.as_f64()?;
    let int = match value.as_i64() {
        Some(int) => i32::try_from(int).ok(),
        None if float.fract() == 0.0
            && float >= f64::from(i32::MIN)
            && float <= f64::from(i32::MAX) =>
        {
            Some(float as i32)
        }
        None => None,
    };
    Some((int, float))
}

/// Finds an archive entry by file name, whether at the root or inside a single top directory.
pub(super) fn entry_name<R: Read + std::io::Seek>(
    archive: &zip::ZipArchive<R>,
    file: &str,
) -> Option<String> {
    let suffix = format!("/{file}");
    archive
        .file_names()
        .filter_map(Result::ok)
        .find(|name| name == file || name.ends_with(&suffix))
        .map(|name| name.into_owned())
}

pub(super) fn for_each_line<T: DeserializeOwned>(
    archive: &mut zip::ZipArchive<File>,
    file: &'static str,
    mut handle: impl FnMut(T, usize) -> Result<(), SdeError>,
) -> Result<(), SdeError> {
    let name = entry_name(archive, file).ok_or(SdeError::MissingFile(file))?;
    let entry = archive.by_name(&name)?;
    for (index, line) in BufReader::new(entry).lines().enumerate() {
        let number = index + 1;
        let line = line.map_err(|e| SdeError::Invalid {
            file,
            line: number,
            message: e.to_string(),
        })?;
        if line.trim().is_empty() {
            continue;
        }
        let parsed: T = serde_json::from_str(&line).map_err(|e| SdeError::Invalid {
            file,
            line: number,
            message: e.to_string(),
        })?;
        handle(parsed, number)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::attribute_value;

    #[test]
    fn splits_attribute_values_like_the_legacy_converter() {
        let number = |raw: &str| serde_json::from_str::<serde_json::Number>(raw).unwrap();
        assert_eq!(
            attribute_value(&number("3386.0")),
            Some((Some(3386), 3386.0))
        );
        assert_eq!(attribute_value(&number("5")), Some((Some(5), 5.0)));
        assert_eq!(attribute_value(&number("0.425")), Some((None, 0.425)));
        assert_eq!(attribute_value(&number("1e12")), Some((None, 1e12)));
    }
}
