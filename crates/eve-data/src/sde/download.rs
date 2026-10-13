//! Downloading the SDE from CCP.

use std::{io::Write, path::Path};

use serde::Deserialize;

use super::{SdeError, SdeInfo, io_error};

/// `latest.jsonl`: `{"_key": "sde", "buildNumber": 3586130, "releaseDate": "..."}`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct VersionLine {
    pub build_number: u64,
    #[serde(default)]
    pub release_date: Option<String>,
}

impl From<VersionLine> for SdeInfo {
    fn from(line: VersionLine) -> Self {
        Self {
            build_number: line.build_number,
            release_date: line.release_date,
        }
    }
}

pub(super) async fn latest(client: &reqwest::Client, base_url: &str) -> Result<SdeInfo, SdeError> {
    let url = format!("{}/latest.jsonl", base_url.trim_end_matches('/'));
    let body = client
        .get(&url)
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    let line = body
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("");
    let version: VersionLine =
        serde_json::from_str(line).map_err(|_| SdeError::BadVersion(body.clone()))?;
    Ok(version.into())
}

/// Downloads the zip of one SDE build to `dest`.
pub(super) async fn archive(
    client: &reqwest::Client,
    base_url: &str,
    build_number: u64,
    dest: &Path,
) -> Result<(), SdeError> {
    let url = format!(
        "{}/eve-online-static-data-{build_number}-jsonl.zip",
        base_url.trim_end_matches('/')
    );
    tracing::info!(%url, "downloading SDE");
    let mut response = client.get(&url).send().await?.error_for_status()?;
    let total = response.content_length();

    let mut file = std::fs::File::create(dest).map_err(io_error(dest))?;
    let mut written: u64 = 0;
    let mut next_report: u64 = 0;
    while let Some(chunk) = response.chunk().await? {
        file.write_all(&chunk).map_err(io_error(dest))?;
        written += chunk.len() as u64;
        if written >= next_report {
            tracing::info!(
                mib = written >> 20,
                total_mib = total.map(|t| t >> 20),
                "downloading SDE"
            );
            next_report += 16 << 20;
        }
    }
    file.sync_all().map_err(io_error(dest))?;
    tracing::info!(mib = written >> 20, "SDE downloaded");
    Ok(())
}
