//! Rebuild when a migration is added or changed: `sqlx::migrate!()` embeds the files at compile
//! time, and Cargo doesn't otherwise notice new files in the directory.
fn main() {
    println!("cargo:rerun-if-changed=migrations");
}
