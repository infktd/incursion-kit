//! Type search for the editors' pickers.

use serde::{Deserialize, Serialize};
use wl_eve_data::{Category, EveData, Type, TypeId};

/// SDE category ids the editors distinguish beyond [`Category`].
const SKILL_CATEGORY: i32 = 16;
const SUBSYSTEM_CATEGORY: i32 = 32;

/// What kind of type a picker wants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TypeKind {
    Ship,
    /// Modules, including rigs.
    Module,
    Subsystem,
    Charge,
    Drone,
    /// Implants and boosters.
    Implant,
    Skill,
    Other,
}

impl TypeKind {
    pub fn of(ty: &Type) -> Self {
        match ty.category {
            Category::Ship => Self::Ship,
            Category::Module => Self::Module,
            Category::Charge => Self::Charge,
            Category::Drone => Self::Drone,
            Category::Implant => Self::Implant,
            Category::Other(SKILL_CATEGORY) => Self::Skill,
            Category::Other(SUBSYSTEM_CATEGORY) => Self::Subsystem,
            Category::Other(_) => Self::Other,
        }
    }
}

/// A type found by [`search_types`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct TypeSearchHit {
    #[schema(value_type = i32)]
    pub id: TypeId,
    /// Exact name, as data files use it.
    pub name: String,
    pub kind: TypeKind,
    /// `high`, `med`, `low`, `rig` or `drone` for fittable types.
    #[schema(value_type = Option<String>)]
    pub slot: Option<&'static str>,
    pub group_id: i32,
}

/// Published types whose name contains `query` (case-insensitively), of the given kinds (any
/// kind when empty): exact matches first, then names starting with the query, then names with a
/// word starting with it, then the rest; alphabetical within each. At most `limit`.
pub fn search_types(
    eve: &EveData,
    query: &str,
    kinds: &[TypeKind],
    limit: usize,
) -> Vec<TypeSearchHit> {
    let query = query.trim().to_lowercase();
    let mut hits: Vec<(u8, String, &Type)> = eve
        .types()
        .filter(|ty| ty.published && (kinds.is_empty() || kinds.contains(&TypeKind::of(ty))))
        .filter_map(|ty| {
            let name = ty.name.to_lowercase();
            let rank = if name == query {
                0
            } else if name.starts_with(&query) {
                1
            } else if name
                .split(|c: char| !c.is_alphanumeric())
                .any(|word| word.starts_with(&query))
            {
                2
            } else if name.contains(&query) {
                3
            } else {
                return None;
            };
            Some((rank, name, ty))
        })
        .collect();
    hits.sort_by(|a, b| (a.0, &a.1, a.2.id).cmp(&(b.0, &b.1, b.2.id)));
    hits.into_iter()
        .take(limit)
        .map(|(_, _, ty)| TypeSearchHit {
            id: ty.id,
            name: ty.name.clone(),
            kind: TypeKind::of(ty),
            slot: ty.slot().map(|slot| slot.as_str()),
            group_id: ty.group_id,
        })
        .collect()
}
