//! Dogma attribute ids used by the waitlist.

/// A dogma attribute id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AttributeId(pub i32);

impl AttributeId {
    /// Training time multiplier of a skill (`skillTimeConstant`).
    pub const TRAINING_TIME_MULTIPLIER: Self = Self(275);
    /// Meta level of a module (`metaLevelOld`).
    pub const META_LEVEL: Self = Self(633);

    pub const EM_RESIST: Self = Self(984);
    pub const EXPLOSIVE_RESIST: Self = Self(985);
    pub const KINETIC_RESIST: Self = Self(986);
    pub const THERMAL_RESIST: Self = Self(987);

    pub const PRIMARY_SKILL: Self = Self(182);
    pub const PRIMARY_SKILL_LEVEL: Self = Self(277);
    pub const SECONDARY_SKILL: Self = Self(183);
    pub const SECONDARY_SKILL_LEVEL: Self = Self(278);
    pub const TERTIARY_SKILL: Self = Self(184);
    pub const TERTIARY_SKILL_LEVEL: Self = Self(279);
    pub const QUATERNARY_SKILL: Self = Self(1285);
    pub const QUATERNARY_SKILL_LEVEL: Self = Self(1286);
    pub const QUINARY_SKILL: Self = Self(1289);
    pub const QUINARY_SKILL_LEVEL: Self = Self(1287);
    pub const SENARY_SKILL: Self = Self(1290);
    pub const SENARY_SKILL_LEVEL: Self = Self(1288);

    /// `(required skill, required level)` attribute pairs, in the order the client lists them.
    pub const SKILL_REQUIREMENTS: [(Self, Self); 6] = [
        (Self::PRIMARY_SKILL, Self::PRIMARY_SKILL_LEVEL),
        (Self::SECONDARY_SKILL, Self::SECONDARY_SKILL_LEVEL),
        (Self::TERTIARY_SKILL, Self::TERTIARY_SKILL_LEVEL),
        (Self::QUATERNARY_SKILL, Self::QUATERNARY_SKILL_LEVEL),
        (Self::QUINARY_SKILL, Self::QUINARY_SKILL_LEVEL),
        (Self::SENARY_SKILL, Self::SENARY_SKILL_LEVEL),
    ];

    /// Every attribute kept in the shrunk static data file.
    pub const STORED: [Self; 18] = [
        Self::TRAINING_TIME_MULTIPLIER,
        Self::META_LEVEL,
        Self::EM_RESIST,
        Self::EXPLOSIVE_RESIST,
        Self::KINETIC_RESIST,
        Self::THERMAL_RESIST,
        Self::PRIMARY_SKILL,
        Self::SECONDARY_SKILL,
        Self::TERTIARY_SKILL,
        Self::QUATERNARY_SKILL,
        Self::QUINARY_SKILL,
        Self::SENARY_SKILL,
        Self::PRIMARY_SKILL_LEVEL,
        Self::SECONDARY_SKILL_LEVEL,
        Self::TERTIARY_SKILL_LEVEL,
        Self::QUATERNARY_SKILL_LEVEL,
        Self::QUINARY_SKILL_LEVEL,
        Self::SENARY_SKILL_LEVEL,
    ];
}
