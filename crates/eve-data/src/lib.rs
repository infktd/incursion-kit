//! EVE static data: types, groups, categories, dogma attributes and effects, skill requirements,
//! module variations and the star map ([`universe`]).
//!
//! The data is read at startup from a small SQLite file built from CCP's static data export
//! (see [`sde`]), held in memory as an immutable [`EveData`] and shared behind an `Arc`.
//! Nothing here is resolved at compile time: type names used by community data files are
//! resolved when those files are loaded, through a [`NameResolver`] that reports every missing
//! name at once.

mod attribute;
mod category;
mod data;
mod effect;
mod error;
mod load;
mod module_info;
mod resolve;
pub mod sde;
mod types;
pub mod universe;

pub use attribute::AttributeId;
pub use category::Category;
pub use data::{AttributeRow, EffectRow, EveData, MetaRow, Tables, TypeRow};
pub use effect::EffectId;
pub use error::{EveDataError, TypeError};
pub use module_info::ModuleInfo;
pub use resolve::{MissingType, MissingTypes, NameResolver};
pub use types::{Slot, Type};
pub use universe::{SecurityBand, Universe};

/// EVE type id (ships, modules, skills, implants...).
pub use wl_core::types::TypeId;

/// A skill level, `0..=5`.
pub type SkillLevel = i8;

/// EVE inventory group id.
pub type GroupId = i32;
