//! Inventory categories the waitlist distinguishes.

/// The inventory category of a type. Categories the waitlist does not care about are kept as
/// [`Category::Other`] with their raw id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Category {
    Ship,
    Module,
    Charge,
    Drone,
    Implant,
    Other(i32),
}

impl Category {
    /// Maps an SDE `categoryID`.
    pub fn from_id(id: i32) -> Self {
        match id {
            6 => Self::Ship,
            7 => Self::Module,
            8 => Self::Charge,
            18 => Self::Drone,
            20 => Self::Implant,
            other => Self::Other(other),
        }
    }

    /// The name used on the wire (`/api/module/info`).
    pub fn category_name(self) -> &'static str {
        match self {
            Self::Ship => "ship",
            Self::Module => "module",
            Self::Charge => "charge",
            Self::Drone => "drone",
            Self::Implant => "implant",
            Self::Other(_) => "_other",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Category;

    #[test]
    fn maps_known_ids_and_keeps_others() {
        assert_eq!(Category::from_id(6), Category::Ship);
        assert_eq!(Category::from_id(20).category_name(), "implant");
        assert_eq!(Category::from_id(4), Category::Other(4));
        assert_eq!(Category::from_id(4).category_name(), "_other");
    }
}
