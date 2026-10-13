//! Shared configuration and types used across the waitlist crates.

pub mod config;
pub mod esi_error;
pub mod types;

pub use config::Config;
pub use esi_error::EsiError;
