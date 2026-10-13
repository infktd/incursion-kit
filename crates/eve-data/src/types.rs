//! A single inventory type and its fitting-relevant properties.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::{AttributeId, Category, EffectId, GroupId, SkillLevel, TypeId};

/// The fitting slot a module goes into.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Slot {
    High,
    Med,
    Low,
    Rig,
    Drone,
}

impl Slot {
    /// The name used on the wire (`/api/module/info`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::High => "high",
            Self::Med => "med",
            Self::Low => "low",
            Self::Rig => "rig",
            Self::Drone => "drone",
        }
    }
}

/// An inventory type: ship, module, charge, skill, implant...
#[derive(Debug, Clone)]
pub struct Type {
    pub id: TypeId,
    pub name: String,
    pub group_id: GroupId,
    pub category: Category,
    pub published: bool,
    /// The dogma attributes kept in the static data file (see [`AttributeId::STORED`]).
    pub attributes: HashMap<AttributeId, f64>,
    /// The dogma effects kept in the static data file (see [`EffectId::STORED`]).
    pub effects: HashSet<EffectId>,
    /// Skills (and levels) needed to use this type.
    pub skill_requirements: BTreeMap<TypeId, SkillLevel>,
}

impl Type {
    /// The slot this type is fitted to, if it is fittable at all.
    pub fn slot(&self) -> Option<Slot> {
        if self.effects.contains(&EffectId::LOW_POWER) {
            Some(Slot::Low)
        } else if self.effects.contains(&EffectId::MED_POWER) {
            Some(Slot::Med)
        } else if self.effects.contains(&EffectId::HIGH_POWER) {
            Some(Slot::High)
        } else if self.effects.contains(&EffectId::RIG_SLOT) {
            Some(Slot::Rig)
        } else if self.category == Category::Drone {
            Some(Slot::Drone)
        } else {
            None
        }
    }

    /// Charges and implants always go into the cargo hold, never into a slot.
    pub fn is_always_cargo(&self) -> bool {
        matches!(self.category, Category::Charge | Category::Implant)
    }

    /// The value of a stored dogma attribute.
    pub fn attribute(&self, attribute: AttributeId) -> Option<f64> {
        self.attributes.get(&attribute).copied()
    }
}

/// Derives skill requirements from the required-skill attribute pairs. A skill attribute without
/// its level attribute is ignored.
pub(crate) fn skill_requirements(
    attributes: &HashMap<AttributeId, f64>,
) -> BTreeMap<TypeId, SkillLevel> {
    let mut requirements = BTreeMap::new();
    for (skill, level) in AttributeId::SKILL_REQUIREMENTS {
        if let (Some(&skill_id), Some(&level)) = (attributes.get(&skill), attributes.get(&level)) {
            // Attribute values are whole numbers stored as floats.
            requirements.insert(skill_id as TypeId, level as SkillLevel);
        }
    }
    requirements
}
