//! The in-memory static data set.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::{
    AttributeId, Category, EffectId, NameResolver, Type, TypeError, TypeId, sde::SdeInfo,
    types::skill_requirements, universe::Universe,
};

/// One row of `invTypes`, with the category of its group already resolved.
#[derive(Debug, Clone)]
pub struct TypeRow {
    pub id: TypeId,
    pub name: String,
    pub group_id: i32,
    /// `None` when the group is not in the data file.
    pub category_id: Option<i32>,
    pub published: bool,
}

/// One row of `dgmTypeAttributes`.
#[derive(Debug, Clone, Copy)]
pub struct AttributeRow {
    pub type_id: TypeId,
    pub attribute: AttributeId,
    pub value: f64,
}

/// One row of `dgmTypeEffects`.
#[derive(Debug, Clone, Copy)]
pub struct EffectRow {
    pub type_id: TypeId,
    pub effect: EffectId,
}

/// One row of `invMetaTypes`: a type's variation parent and meta group.
#[derive(Debug, Clone, Copy)]
pub struct MetaRow {
    pub type_id: TypeId,
    pub parent_type_id: Option<TypeId>,
    pub meta_group_id: Option<i32>,
}

/// The raw tables an [`EveData`] is built from.
#[derive(Debug, Clone, Default)]
pub struct Tables {
    pub types: Vec<TypeRow>,
    pub attributes: Vec<AttributeRow>,
    pub effects: Vec<EffectRow>,
    pub metas: Vec<MetaRow>,
}

/// Meta group ids with a fixed meta level when a variation has no meta level attribute.
const META_GROUP_TECH_1: i32 = 1;
const META_GROUP_TECH_2: i32 = 2;

/// Immutable EVE static data, shared behind an `Arc`.
#[derive(Debug)]
pub struct EveData {
    types: HashMap<TypeId, Type>,
    names: HashMap<String, TypeId>,
    /// `invMetaTypes` keyed by type id.
    metas: HashMap<TypeId, MetaRow>,
    /// Variation parent → types listing it as their parent.
    children: HashMap<TypeId, Vec<TypeId>>,
    /// Meta level attribute of every type that has one (including types missing from `types`).
    meta_levels: HashMap<TypeId, f64>,
    info: Option<SdeInfo>,
    /// The star map (empty for static data files without map tables).
    universe: Universe,
}

impl EveData {
    /// Builds the in-memory data set from raw table rows.
    pub fn from_tables(tables: Tables, info: Option<SdeInfo>) -> Self {
        let mut attributes: HashMap<TypeId, HashMap<AttributeId, f64>> = HashMap::new();
        let mut meta_levels = HashMap::new();
        for row in tables.attributes {
            if row.attribute == AttributeId::META_LEVEL {
                meta_levels.insert(row.type_id, row.value);
            }
            attributes
                .entry(row.type_id)
                .or_default()
                .insert(row.attribute, row.value);
        }

        let mut effects: HashMap<TypeId, HashSet<EffectId>> = HashMap::new();
        for row in tables.effects {
            effects.entry(row.type_id).or_default().insert(row.effect);
        }

        // When several types share a name, a published type wins over an unpublished one, then
        // the highest type id wins: the same choice the legacy waitlist's name lookup made.
        let mut type_rows = tables.types;
        type_rows.sort_by_key(|row| (row.published, row.id));

        let mut types = HashMap::with_capacity(type_rows.len());
        let mut names = HashMap::with_capacity(type_rows.len());
        for row in type_rows {
            let attributes = attributes.remove(&row.id).unwrap_or_default();
            let ty = Type {
                id: row.id,
                skill_requirements: skill_requirements(&attributes),
                attributes,
                effects: effects.remove(&row.id).unwrap_or_default(),
                name: row.name,
                group_id: row.group_id,
                category: row
                    .category_id
                    .map_or(Category::Other(0), Category::from_id),
                published: row.published,
            };
            names.insert(ty.name.clone(), ty.id);
            types.insert(ty.id, ty);
        }

        let mut metas = HashMap::with_capacity(tables.metas.len());
        let mut children: HashMap<TypeId, Vec<TypeId>> = HashMap::new();
        for row in tables.metas {
            if let Some(parent) = row.parent_type_id {
                children.entry(parent).or_default().push(row.type_id);
            }
            metas.entry(row.type_id).or_insert(row);
        }

        Self {
            types,
            names,
            metas,
            children,
            meta_levels,
            info,
            universe: Universe::default(),
        }
    }

    /// Attaches the star map.
    pub fn with_universe(mut self, universe: Universe) -> Self {
        self.universe = universe;
        self
    }

    /// The star map: systems, constellations, regions, stargates and NPC stations. Empty when
    /// the static data file predates the map tables (rebuild it with `waitlist sde`).
    pub fn universe(&self) -> &Universe {
        &self.universe
    }

    /// Build information of the static data export this was made from, when recorded.
    pub fn info(&self) -> Option<&SdeInfo> {
        self.info.as_ref()
    }

    /// Number of types loaded.
    pub fn type_count(&self) -> usize {
        self.types.len()
    }

    /// Every type, in no particular order.
    pub fn types(&self) -> impl Iterator<Item = &Type> {
        self.types.values()
    }

    /// Looks up a type by id.
    pub fn get(&self, id: TypeId) -> Option<&Type> {
        self.types.get(&id)
    }

    /// Looks up a type by id, failing with the legacy "no matches for item" error.
    pub fn require(&self, id: TypeId) -> Result<&Type, TypeError> {
        self.get(id).ok_or(TypeError::NothingMatched)
    }

    /// Looks up a type by its exact (case-sensitive) English name.
    pub fn by_name(&self, name: &str) -> Option<&Type> {
        self.id_of(name).and_then(|id| self.get(id))
    }

    /// The id of the type with this exact (case-sensitive) English name.
    pub fn id_of(&self, name: &str) -> Option<TypeId> {
        self.names.get(name).copied()
    }

    /// The name of a type.
    pub fn name_of(&self, id: TypeId) -> Option<&str> {
        self.get(id).map(|ty| ty.name.as_str())
    }

    /// Names of the given types; unknown ids are left out.
    pub fn names_of(&self, ids: impl IntoIterator<Item = TypeId>) -> BTreeMap<TypeId, String> {
        ids.into_iter()
            .filter_map(|id| self.name_of(id).map(|name| (id, name.to_string())))
            .collect()
    }

    /// Starts resolving names from a data file; see [`NameResolver`].
    pub fn resolver(&self) -> NameResolver<'_> {
        NameResolver::new(self)
    }

    /// All variations of a type (the type itself, its variation parent and the parent's other
    /// variations) with their meta level.
    ///
    /// The variation parent always has meta level 0. A variation without a meta level attribute
    /// counts as 1 when it is tech 1 and 2 when it is tech 2, and is left out otherwise.
    pub fn type_variations(&self, id: TypeId) -> BTreeMap<TypeId, i64> {
        let parent = self
            .metas
            .get(&id)
            .and_then(|meta| meta.parent_type_id)
            .unwrap_or(id);

        let mut variations = BTreeMap::new();
        variations.insert(parent, 0);

        for &child in self.children.get(&parent).into_iter().flatten() {
            let meta_level = self.meta_levels.get(&child).copied();
            let meta_group = self.metas.get(&child).and_then(|meta| meta.meta_group_id);
            let level = match (meta_level, meta_group) {
                // The legacy waitlist read attribute values as 32-bit floats; mirror its rounding.
                (Some(level), _) => Some((level as f32) as i64),
                (None, Some(META_GROUP_TECH_1)) => Some(1),
                (None, Some(META_GROUP_TECH_2)) => Some(2),
                (None, _) => None,
            };
            if let Some(level) = level {
                variations.insert(child, level);
            }
        }

        variations
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: TypeId, name: &str, category_id: i32, published: bool) -> TypeRow {
        TypeRow {
            id,
            name: name.to_string(),
            group_id: 1,
            category_id: Some(category_id),
            published,
        }
    }

    fn sample() -> EveData {
        let tables = Tables {
            types: vec![
                row(10, "Widget I", 7, true),
                row(11, "Widget II", 7, true),
                row(12, "Fancy Widget", 7, true),
                row(13, "Old Widget", 7, true),
                row(20, "Duplicate", 7, true),
                row(21, "Duplicate", 7, false),
                row(22, "Duplicate", 7, true),
            ],
            attributes: vec![
                AttributeRow {
                    type_id: 12,
                    attribute: AttributeId::META_LEVEL,
                    value: 8.0,
                },
                AttributeRow {
                    type_id: 11,
                    attribute: AttributeId::PRIMARY_SKILL,
                    value: 3300.0,
                },
                AttributeRow {
                    type_id: 11,
                    attribute: AttributeId::PRIMARY_SKILL_LEVEL,
                    value: 4.0,
                },
            ],
            effects: vec![EffectRow {
                type_id: 11,
                effect: EffectId::MED_POWER,
            }],
            metas: vec![
                MetaRow {
                    type_id: 11,
                    parent_type_id: Some(10),
                    meta_group_id: Some(2),
                },
                MetaRow {
                    type_id: 12,
                    parent_type_id: Some(10),
                    meta_group_id: Some(4),
                },
                MetaRow {
                    type_id: 13,
                    parent_type_id: Some(10),
                    meta_group_id: Some(3),
                },
            ],
        };
        EveData::from_tables(tables, None)
    }

    #[test]
    fn duplicate_names_prefer_published_then_highest_id() {
        assert_eq!(sample().id_of("Duplicate"), Some(22));
    }

    #[test]
    fn derives_skill_requirements_and_slots() {
        let data = sample();
        let widget = data.require(11).unwrap();
        assert_eq!(widget.skill_requirements.get(&3300), Some(&4));
        assert_eq!(widget.slot(), Some(crate::Slot::Med));
        assert_eq!(data.require(99).unwrap_err(), TypeError::NothingMatched);
    }

    #[test]
    fn variations_follow_parent_and_meta_rules() {
        let data = sample();
        let expected: BTreeMap<TypeId, i64> = [(10, 0), (11, 2), (12, 8)].into_iter().collect();
        // Looking up from a child or from the parent gives the same family; the meta group 3
        // variation without a meta level is left out.
        assert_eq!(data.type_variations(12), expected);
        assert_eq!(data.type_variations(10), expected);
        // A type without variations is its own family.
        assert_eq!(
            data.type_variations(20),
            [(20, 0)].into_iter().collect::<BTreeMap<_, _>>()
        );
    }
}
