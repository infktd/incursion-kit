//! `dev-esi`: a local stand-in for EVE SSO and ESI. Development and tests only.

use std::path::PathBuf;

use anyhow::Context as _;
use clap::{Parser, Subcommand};
use dev_esi::{DevEsi, SeedSource, Signer, Universe};

#[derive(Parser)]
#[command(
    version,
    about = "A fake EVE SSO and ESI for developing against the waitlist without a CCP account. \
             It hands out tokens for any seeded character: never expose it or point a production \
             server at it."
)]
struct Cli {
    /// Seed file (YAML, or TOML by extension). Default: the seed shipped with dev-esi.
    #[arg(long, global = true, env = "DEV_ESI_SEED")]
    seed: Option<PathBuf>,
    #[command(flatten)]
    serve: ServeArgs,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Serve SSO, ESI and the control API (default).
    Serve,
    /// Print SQL that grants the seed's waitlist roles and bans its banned corporations, e.g.
    /// `dev-esi sql | mysql waitlist`.
    Sql,
    /// Print the default seed (a starting point for your own).
    Seed,
}

#[derive(clap::Args)]
struct ServeArgs {
    /// Address to listen on.
    #[arg(
        long,
        global = true,
        env = "DEV_ESI_BIND",
        default_value = "127.0.0.1:8090"
    )]
    bind: String,
    /// The waitlist's static data file, for type, system and constellation lookups. Skipped
    /// (with a warning) if it doesn't exist.
    #[arg(long, global = true, env = "DEV_ESI_SDE")]
    sde: Option<PathBuf>,
    /// Keep the token signing key and the refresh tokens in this directory (created if
    /// missing), so the tokens a waitlist stored stay valid across restarts. Default: nothing
    /// is kept; restarting logs everyone out.
    #[arg(long, global = true, env = "DEV_ESI_STATE_DIR")]
    state_dir: Option<PathBuf>,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let seed = match &cli.seed {
        Some(path) => SeedSource::from_path(path)?,
        None => SeedSource::default_seed(),
    };
    match cli.command {
        Some(Command::Sql) => {
            print!("{}", dev_esi::seed::waitlist_sql(&seed.parse()?));
            Ok(())
        }
        Some(Command::Seed) => {
            print!("{}", dev_esi::seed::DEFAULT_SEED);
            Ok(())
        }
        Some(Command::Serve) | None => serve(seed, cli.serve),
    }
}

fn serve(seed: SeedSource, args: ServeArgs) -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(async move {
            let universe = match &args.sde {
                Some(path) if path.exists() => {
                    let universe = Universe::load(path).await.with_context(|| {
                        format!("could not read the static data file {}", path.display())
                    })?;
                    tracing::info!(path = %path.display(), types = universe.types.len(), "static data loaded");
                    Some(universe)
                }
                Some(path) => {
                    tracing::warn!(path = %path.display(), "static data file not found: types, systems and constellations are unknown");
                    None
                }
                None => None,
            };
            let signer = match &args.state_dir {
                Some(dir) => Signer::load_or_create(&dir.join("signing-key.der"))?,
                None => Signer::generate()?,
            };
            let dev = DevEsi::new(seed, signer, universe)?;
            if let Some(dir) = &args.state_dir {
                dev.persist_grants(dir.join("refresh-tokens.json"))
                    .with_context(|| format!("could not read {}", dir.display()))?;
            }
            let listener = tokio::net::TcpListener::bind(&args.bind)
                .await
                .with_context(|| format!("could not listen on {}", args.bind))?;
            let addr = listener.local_addr()?;
            tracing::warn!(
                "dev-esi is a FAKE EVE SSO/ESI for development: anyone who reaches it can log in \
                 as any seeded character"
            );
            tracing::info!(
                "serving on http://{addr}: ESI at /, SSO at /v2/oauth/authorize and \
                 /v2/oauth/token, dashboard at /_dev"
            );
            let shutdown = async {
                let _ = tokio::signal::ctrl_c().await;
            };
            #[cfg(unix)]
            let terminate = async {
                if let Ok(mut signal) =
                    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                {
                    signal.recv().await;
                } else {
                    std::future::pending::<()>().await;
                }
            };
            #[cfg(not(unix))]
            let terminate = std::future::pending::<()>();
            axum::serve(listener, dev.router())
                .with_graceful_shutdown(async move {
                    tokio::select! {
                        () = shutdown => {},
                        () = terminate => {},
                    }
                })
                .await?;
            Ok(())
        })
}
