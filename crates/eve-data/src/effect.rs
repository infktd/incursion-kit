//! Dogma effect ids used by the waitlist.

/// A dogma effect id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EffectId(pub i32);

impl EffectId {
    pub const LOW_POWER: Self = Self(11);
    pub const HIGH_POWER: Self = Self(12);
    pub const MED_POWER: Self = Self(13);
    pub const RIG_SLOT: Self = Self(2663);

    /// Every effect kept in the shrunk static data file.
    pub const STORED: [Self; 4] = [
        Self::LOW_POWER,
        Self::HIGH_POWER,
        Self::MED_POWER,
        Self::RIG_SLOT,
    ];
}
