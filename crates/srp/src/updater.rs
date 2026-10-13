//! The SRP updater worker: reconciles the wallet every `srp_updater.interval_seconds` (one
//! minute after a failure). Started by the binary when `srp_updater.enable` is set.

use std::time::Duration;

use tokio_util::sync::CancellationToken;

use crate::{Srp, SrpResult, service_account};

/// One updater cycle: nothing without a service account; otherwise reconcile payments, then
/// void per-focus coverage if the focus ended.
pub async fn run_once(srp: &Srp) -> SrpResult<()> {
    if service_account::active(srp.db()).await?.is_none() {
        tracing::debug!("no SRP service account configured, skipping the SRP update");
        return Ok(());
    }
    let outcome = srp.process_and_validate().await?;
    tracing::info!(
        payments = outcome.payments,
        paid_reports = outcome.paid_reports.len(),
        "SRP payments processed"
    );
    Ok(())
}

/// Run until `cancel` fires.
pub async fn run(srp: Srp, cancel: CancellationToken) {
    let interval = srp.settings().interval_seconds.max(1);
    loop {
        let wait = tokio::select! {
            () = cancel.cancelled() => return,
            result = run_once(&srp) => match result {
                Ok(()) => interval,
                Err(e) => {
                    tracing::error!(error = %e, details = ?e, "SRP updater failed");
                    60
                }
            },
        };
        tokio::select! {
            () = cancel.cancelled() => return,
            () = tokio::time::sleep(Duration::from_secs(wait)) => {}
        }
    }
}
