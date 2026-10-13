//! The public incursion tracker: spawns seen on ESI, their system roles, influence history,
//! state changes, lifetimes and spawn windows.
//!
//! - [`sync`]: applies an ESI `/incursions` snapshot to the database (the worker in
//!   `wl-workers` polls ESI and calls it).
//! - [`store`]: the tables (`0002_incursion_tracker.sql`) and their queries.
//! - [`roles`]: staging / vanguard / assault / headquarters per system.
//! - [`rules`]: maximum remaining time and spawn windows.
//! - [`view`]: the public API's JSON shapes, enriched from the star map.
//! - [`discord`]: optional webhook notices.
//!
//! See `docs/incursions-api.md` for the API and the rules with their sources.

pub mod discord;
pub mod roles;
pub mod rules;
pub mod store;
pub mod sync;
pub mod view;
