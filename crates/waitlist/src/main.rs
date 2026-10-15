//! `waitlist`: the incursion waitlist server.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::Context;
use clap::{Parser, Subcommand};
use wl_api::AppState;
use wl_core::Config;
use wl_live::Hub;

#[derive(Parser)]
#[command(version, about = "Incursion waitlist server")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run migrations, start background workers and serve the API (default).
    Serve,
    /// Apply pending database migrations and exit.
    Migrate {
        /// Take over a database created by the legacy waitlist: record the baseline migration
        /// as applied (without running it), then apply newer migrations. Back up first.
        #[arg(long)]
        adopt: bool,
    },
    /// Download CCP's static data export and (re)build the static data file. `serve` builds it
    /// automatically when it is missing.
    Sde {
        /// Output path. Defaults to `data.sde` from the config.
        #[arg(long)]
        out: Option<std::path::PathBuf>,
        /// SDE build number to use instead of the latest.
        #[arg(long)]
        build: Option<u64>,
        /// Convert an already-downloaded SDE zip (`*-jsonl.zip`) instead of downloading.
        #[arg(long)]
        archive: Option<std::path::PathBuf>,
    },
    /// Write the bundled example data set (doctrine fits, skills, skill plans, categories...) to
    /// the data directory. `serve` does this automatically when the directory is empty.
    InitData {
        /// Data directory. Defaults to `data.dir` from the config.
        #[arg(long)]
        dir: Option<std::path::PathBuf>,
        /// Also write into a non-empty directory, overwriting example files of the same name
        /// (other files, such as `rules.yaml`, guides and locales, are kept).
        #[arg(long)]
        force: bool,
    },
    /// Print the OpenAPI 3.1 description of the HTTP API (JSON) and exit. Needs no config.
    Openapi,
    /// Check that the local server answers `GET /healthz` (exit code 0 = healthy). Meant for
    /// container health checks.
    Healthcheck {
        /// Port to probe. Defaults to the port of `server.bind`, or 5000.
        #[arg(long)]
        port: Option<u16>,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let command = cli.command.unwrap_or(Command::Serve);

    // Kept free of logging setup and the async runtime: it runs every few seconds.
    if let Command::Healthcheck { port } = command {
        return healthcheck(port);
    }
    if let Command::Openapi = command {
        return match std::io::stdout().write_all(wl_api::openapi::openapi_json().as_bytes()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("could not write the spec: {e}");
                ExitCode::FAILURE
            }
        };
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let result = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("could not start the async runtime")
        .and_then(|runtime| runtime.block_on(run(command)));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("{e:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run(command: Command) -> anyhow::Result<()> {
    if let Command::Sde {
        out,
        build,
        archive,
    } = command
    {
        return sde(out, build, archive).await;
    }
    if let Command::InitData { dir, force } = command {
        return init_data(dir, force);
    }
    let config = Config::load()?;
    match command {
        Command::Serve => serve(config).await,
        Command::Migrate { adopt } => migrate(config, adopt).await,
        Command::Sde { .. } | Command::InitData { .. } => {
            unreachable!("handled before loading the config")
        }
        Command::Healthcheck { .. } | Command::Openapi => {
            unreachable!("handled before the runtime starts")
        }
    }
}

async fn migrate(config: Config, adopt: bool) -> anyhow::Result<()> {
    let db = wl_db::connect(&config.database).await?;
    if adopt {
        if wl_db::adopt(&db).await? {
            tracing::info!("legacy database adopted; migrations are up to date");
        } else {
            tracing::info!("nothing to adopt; migrations are up to date");
        }
    } else {
        wl_db::migrate(&db).await?;
        tracing::info!("migrations are up to date");
    }
    db.close().await;
    Ok(())
}

/// Builds the static data file (`waitlist sde`). Needs no config when `--out` is given.
async fn sde(
    out: Option<std::path::PathBuf>,
    build: Option<u64>,
    archive: Option<std::path::PathBuf>,
) -> anyhow::Result<()> {
    let out = match out {
        Some(out) => out,
        None => Config::load()?.data.sde.into(),
    };
    let options = wl_eve_data::sde::BuildOptions {
        build_number: build,
        archive,
        ..Default::default()
    };
    let info = wl_eve_data::sde::build(&out, &options).await?;
    tracing::info!(path = %out.display(), sde_build = info.build_number, "static data file ready");
    Ok(())
}

/// Writes the example data set (`waitlist init-data`). Needs no config when `--dir` is given.
fn init_data(dir: Option<std::path::PathBuf>, force: bool) -> anyhow::Result<()> {
    let dir = match dir {
        Some(dir) => dir,
        None => Config::load()?.data.dir.into(),
    };
    match wl_fitting::example_data::seed(&dir, force)
        .with_context(|| format!("could not write the example data to {}", dir.display()))?
    {
        wl_fitting::example_data::SeedOutcome::Seeded => {
            tracing::info!(dir = %dir.display(), "example data written");
            Ok(())
        }
        wl_fitting::example_data::SeedOutcome::NotEmpty => anyhow::bail!(
            "{} is not empty; use --force to overwrite the example files in it",
            dir.display()
        ),
    }
}

/// First run: an empty data directory gets the example data set, so a fresh install starts.
fn seed_data_dir(dir: &str) -> anyhow::Result<()> {
    let seeded = wl_fitting::example_data::seed(std::path::Path::new(dir), false)
        .with_context(|| format!("could not write the example data to {dir} (is it writable?)"))?;
    if seeded == wl_fitting::example_data::SeedOutcome::Seeded {
        tracing::warn!(
            dir,
            "the data directory was empty: wrote the example data set; replace it with your \
             community's fits, skills and skill plans"
        );
    }
    Ok(())
}

/// Overridden ESI/SSO URLs are for development and tests (e.g. dev-esi); say so loudly.
fn warn_if_esi_overridden(esi: &wl_core::config::EsiConfig) {
    let overridden: Vec<String> = [
        ("esi_url", &esi.esi_url),
        ("sso_authorize_url", &esi.sso_authorize_url),
        ("sso_token_url", &esi.sso_token_url),
    ]
    .into_iter()
    .filter_map(|(key, value)| {
        let value = value.as_deref().map(str::trim).filter(|v| !v.is_empty())?;
        Some(format!("esi.{key} = {value}"))
    })
    .collect();
    if !overridden.is_empty() {
        tracing::warn!(
            "not using CCP's ESI/SSO ({}): fine for development, never in production",
            overridden.join(", ")
        );
    }
}

async fn serve(config: Config) -> anyhow::Result<()> {
    warn_if_esi_overridden(&config.esi);
    seed_data_dir(&config.data.dir)?;
    wl_eve_data::sde::ensure_sde(std::path::Path::new(&config.data.sde), &Default::default())
        .await
        .context("could not build the static data file")?;
    let db = wl_db::connect(&config.database).await?;
    wl_db::migrate(&db).await?;

    let eve_data = std::sync::Arc::new(
        wl_eve_data::EveData::load(&config.data.sde)
            .await
            .with_context(|| format!("could not load the static data file {}", config.data.sde))?,
    );
    let data_store = {
        let (eve, dir) = (eve_data.clone(), config.data.dir.clone());
        tokio::task::spawn_blocking(move || wl_fitting::DataStore::open(eve, &dir))
            .await
            .context("loading the data files panicked")?
            .with_context(|| format!("invalid data files in {}", config.data.dir))?
    };
    let esi = wl_esi::Esi::new(&config, db.clone()).context("could not set up ESI")?;
    let srp = wl_srp::Srp::new(&config, db.clone(), esi.clone()).with_eve_data(eve_data.clone());

    let bind = config.server.bind.clone();
    let live = Hub::new();
    let state = AppState::new(config, db.clone(), live.clone())?
        .with_eve_data(eve_data)
        .with_data_store(std::sync::Arc::new(data_store))
        .with_esi(esi)
        .with_srp(srp);

    let cancel_workers = wl_workers::CancellationToken::new();
    let workers = start_background_workers(&state, &cancel_workers);

    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .with_context(|| format!("could not listen on {bind}"))?;
    tracing::info!(%bind, "serving");

    axum::serve(listener, wl_api::router(state))
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            tracing::info!("shutting down");
            cancel_workers.cancel();
            // Open event streams never finish on their own; end them so shutdown can complete.
            live.disconnect_all();
        })
        .await
        .context("server error")?;

    // Workers stop at their next await point once cancelled; don't let one hold up shutdown.
    let stopped = join_workers(workers);
    if tokio::time::timeout(Duration::from_secs(10), stopped)
        .await
        .is_err()
    {
        tracing::warn!("background workers did not stop within 10 s");
    }
    db.close().await;
    Ok(())
}

/// Wait for every task (a panicked worker is only logged).
async fn join_workers(handles: Vec<tokio::task::JoinHandle<()>>) {
    for handle in handles {
        if let Err(e) = handle.await {
            tracing::error!("background worker failed: {e}");
        }
    }
}

/// Spawn the background workers enabled in the config (fleet, skill, war and incursion updaters
/// and the incursion tracker from `wl-workers`, the SRP updater from `wl-srp`). They share the API's database, ESI client,
/// live hub and war cache, and stop when `cancel` is cancelled.
fn start_background_workers(
    state: &AppState,
    cancel: &wl_workers::CancellationToken,
) -> Vec<tokio::task::JoinHandle<()>> {
    let esi = match state.esi() {
        Ok(esi) => esi.clone(),
        Err(e) => {
            tracing::error!("background workers not started: {e}");
            return Vec::new();
        }
    };
    let mut handles = wl_workers::start(
        wl_workers::Services {
            config: state.config().clone(),
            db: state.db().clone(),
            esi,
            live: state.live().clone(),
            war_cache: state.war_cache().clone(),
            eve_data: state.eve_data().ok().cloned(),
        },
        cancel,
    );
    if state.config().srp_updater.enable
        && let Ok(srp) = state.srp()
    {
        handles.push(tokio::spawn(wl_srp::updater::run(
            srp.clone(),
            cancel.child_token(),
        )));
    }
    tracing::info!(count = handles.len(), "background workers started");
    handles
}

/// Resolves on Ctrl-C or SIGTERM.
async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            tracing::error!("could not listen for Ctrl-C: {e}");
            std::future::pending::<()>().await;
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(e) => {
                tracing::error!("could not listen for SIGTERM: {e}");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
}

fn healthcheck(port: Option<u16>) -> ExitCode {
    let port = port.unwrap_or_else(|| {
        Config::load()
            .ok()
            .and_then(|c| c.server.bind.parse::<SocketAddr>().ok())
            .map_or(5000, |addr| addr.port())
    });
    match probe(port) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("unhealthy: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `GET /healthz` on localhost with a plain socket; healthy iff the status is 200.
fn probe(port: u16) -> Result<(), String> {
    let timeout = Duration::from_secs(5);
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut stream = TcpStream::connect_timeout(&addr, timeout).map_err(|e| e.to_string())?;
    stream
        .set_read_timeout(Some(timeout))
        .and_then(|()| stream.set_write_timeout(Some(timeout)))
        .map_err(|e| e.to_string())?;
    stream
        .write_all(b"GET /healthz HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
        .map_err(|e| e.to_string())?;

    let mut head = [0u8; 64];
    let mut read = 0;
    while read < 12 {
        match stream.read(&mut head[read..]) {
            Ok(0) => break,
            Ok(n) => read += n,
            Err(e) => return Err(e.to_string()),
        }
    }
    let status_line = String::from_utf8_lossy(&head[..read]);
    let status = status_line.split_whitespace().nth(1).unwrap_or("none");
    if status == "200" {
        Ok(())
    } else {
        Err(format!("/healthz answered status {status}"))
    }
}
