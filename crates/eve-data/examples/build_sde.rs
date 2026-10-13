//! Builds the static data file without the rest of the server:
//!
//! ```sh
//! cargo run -p wl-eve-data --example build_sde -- deploy/sde/sde.sqlite [--build N] [--archive PATH]
//! ```
//!
//! `waitlist sde` does the same from the server binary.

use std::path::PathBuf;

use wl_eve_data::sde::{self, BuildOptions};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let mut args = std::env::args().skip(1);
    let mut out = None;
    let mut options = BuildOptions::default();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--build" => {
                options.build_number = Some(args.next().ok_or("--build needs a value")?.parse()?)
            }
            "--archive" => {
                options.archive = Some(args.next().ok_or("--archive needs a value")?.into())
            }
            path => out = Some(PathBuf::from(path)),
        }
    }
    let out = out.unwrap_or_else(|| PathBuf::from("deploy/sde/sde.sqlite"));
    let info = sde::build(&out, &options).await?;
    println!(
        "built {} from SDE build {}",
        out.display(),
        info.build_number
    );
    Ok(())
}
