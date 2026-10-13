//! Ship replacement program (SRP). The domain logic lives in `wl-srp`.
//!
//! - `GET /api/srp/status?character_id=`: a pilot's coverage. A caller who may not see the
//!   character gets **400 `Unauthorized`** (legacy quirk, kept).
//! - `/api/admin/srp/*` (`commanders-manage:admin`): service account setup and wallet checks,
//!   reconciliation, config, coverage overview, reports, appraisal, approve/deny, in-game windows.
//! - `/api/fc/srp/*` (`fleet-view`): report list, submit, edit.
//!
//! `GET /api/pilot/srp-reports/{character_id}` existed in the legacy code but was never mounted
//! (it answered 404); it stays unmounted (`Srp::reports_for_pilot` holds its query).
//!
//! `/api/srp/status` and `/api/admin/srp/all-statuses` also return `srp_tiers` (the configured
//! prices, see `[srp]` in the config) so clients need not hardcode them.

use std::collections::HashMap;

use crate::extract::{Path, Query};
use axum::Json;
use axum::extract::State;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use wl_esi::LoginKind;
use wl_esi::killmail::KillmailData;
use wl_srp::coverage::CoverageAtTime;
use wl_srp::focus::{self, IncursionFocus};
use wl_srp::killmail::EnrichedKillmail;
use wl_srp::reports::killmail_timestamp;
use wl_srp::tiers::TierTable;
use wl_srp::{
    CorporationWallet, ServiceAccount, SrpError, SrpPayment, SrpReport, WalletJournalEntry,
    payments, service_account,
};

use crate::auth::sso::start_login;
use crate::openapi::tags;
use crate::{ApiError, ApiResult, AppState, AuthenticatedAccount, JsonBody};

const ADMIN: &str = "commanders-manage:admin";

/// The character the legacy "test window" endpoint opens (CCP Falcon).
const TEST_WINDOW_TARGET: i64 = 2112625428;

impl From<SrpError> for ApiError {
    fn from(err: SrpError) -> Self {
        match err {
            SrpError::Database(e) => ApiError::Database(e),
            SrpError::Esi(e) => ApiError::Esi(e),
            SrpError::BadRequest(msg) => ApiError::BadRequest(msg),
            SrpError::NotFound(msg) => ApiError::NotFound(msg),
            SrpError::Forbidden(msg) => ApiError::Forbidden(msg),
        }
    }
}

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(setup))
        .routes(routes!(get_service_account))
        .routes(routes!(test_connection))
        .routes(routes!(list_wallets))
        .routes(routes!(fetch_journal))
        .routes(routes!(remove_service_account))
        .routes(routes!(process))
        .routes(routes!(status))
        .routes(routes!(get_config, update_config))
        .routes(routes!(all_statuses))
        .routes(routes!(incursion_focus))
        .routes(routes!(focus_end_timestamp))
        .routes(routes!(admin_reports))
        .routes(routes!(fc_reports))
        .routes(routes!(admin_report))
        .routes(routes!(report_killmail))
        .routes(routes!(report_killmail_enriched))
        .routes(routes!(fleet_validation))
        .routes(routes!(srp_validation))
        .routes(routes!(appraisal))
        .routes(routes!(approve))
        .routes(routes!(deny))
        .routes(routes!(test_character_window))
        .routes(routes!(open_victim_window))
        .routes(routes!(submit))
        .routes(routes!(update))
        .routes(routes!(report_for_edit))
}

// --- service account and wallet -----------------------------------------------------------

#[derive(Serialize, ToSchema)]
struct SetupResponse {
    /// The EVE SSO URL that sets up the service account.
    login_url: String,
    /// Whether an active service account already exists.
    has_service_account: bool,
}

/// SRP service account setup
///
/// Starts the SSO login that sets up the SRP service account: the character whose token reads the
/// corporation wallet. The login requests the wallet-read scope and its callback
/// (`GET /api/auth/cb` with `state=srp_setup:<nonce>`) stores the character as the service account.
/// Requires `commanders-manage:admin`.
#[utoipa::path(
    get,
    path = "/api/admin/srp/setup",
    tag = tags::SRP,
    operation_id = "srp_setup",
    security(("session" = ["commanders-manage:admin"])),
    responses((
        status = 200,
        description = "The login URL and whether a service account exists.",
        body = SetupResponse,
        headers(("set-cookie" = String, description = "`wlSsoNonce`: the login's nonce (HttpOnly, 15 minutes).")),
    )),
)]
async fn setup(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
) -> ApiResult<impl axum::response::IntoResponse> {
    account.require_access(ADMIN)?;
    let has_service_account = service_account::active(state.db()).await?.is_some();
    let (login_url, cookie) = start_login(&state, LoginKind::SrpSetup, false)?;
    Ok((
        cookie,
        Json(SetupResponse {
            login_url,
            has_service_account,
        }),
    ))
}

#[derive(Serialize, ToSchema)]
struct ServiceAccountResponse {
    /// The active service account, `null` if none is configured.
    service_account: Option<ServiceAccount>,
}

/// SRP service account
///
/// The active service account (tokens are never returned). Requires `commanders-manage:admin`.
#[utoipa::path(
    get,
    path = "/api/admin/srp/service-account",
    tag = tags::SRP,
    operation_id = "get_srp_service_account",
    security(("session" = ["commanders-manage:admin"])),
    responses((status = 200, description = "The service account, or `null`.", body = ServiceAccountResponse)),
)]
async fn get_service_account(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
) -> ApiResult<Json<ServiceAccountResponse>> {
    account.require_access(ADMIN)?;
    Ok(Json(ServiceAccountResponse {
        service_account: service_account::active(state.db()).await?,
    }))
}

const NO_SERVICE_ACCOUNT: &str = "No service account configured";

#[derive(Serialize, ToSchema)]
struct TestResponse {
    success: bool,
    /// What happened, for display.
    message: String,
    /// Journal entries on page 1 (when `success`).
    entries_found: Option<i64>,
}

/// Test the SRP wallet connection
///
/// Reads page 1 of the service account's configured wallet division's journal. Failures are not
/// error responses: `success` is `false` and `message` says why (`No service account configured`,
/// or `Failed to connect to wallet <division>: <reason> (Character: <id>, Corp: <id>)`). Requires
/// `commanders-manage:admin`.
#[utoipa::path(
    post,
    path = "/api/admin/srp/test",
    tag = tags::SRP,
    operation_id = "test_srp_wallet_connection",
    security(("session" = ["commanders-manage:admin"])),
    responses((status = 200, description = "The result of the test.", body = TestResponse)),
)]
async fn test_connection(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
) -> ApiResult<Json<TestResponse>> {
    account.require_access(ADMIN)?;
    let Some(sa) = service_account::active(state.db()).await? else {
        return Ok(Json(TestResponse {
            success: false,
            message: NO_SERVICE_ACCOUNT.into(),
            entries_found: None,
        }));
    };
    Ok(Json(match state.srp()?.journal(&sa, sa.wallet_id).await {
        Ok(entries) => TestResponse {
            success: true,
            message: format!(
                "Successfully connected to corporation wallet {}. Found {} entries.",
                sa.wallet_id,
                entries.len()
            ),
            entries_found: Some(entries.len() as i64),
        },
        Err(e) => TestResponse {
            success: false,
            message: format!(
                "Failed to connect to wallet {}: {e} (Character: {}, Corp: {})",
                sa.wallet_id, sa.character_id, sa.corporation_id
            ),
            entries_found: None,
        },
    }))
}

#[derive(Serialize, ToSchema)]
struct WalletsResponse {
    success: bool,
    /// What happened, for display.
    message: String,
    /// The corporation's wallet divisions (when `success`).
    wallets: Option<Vec<CorporationWallet>>,
}

/// SRP corporation wallets
///
/// The balances of the service account's corporation wallet divisions. Failures are not error
/// responses: `success` is `false` and `message` says why (`No service account configured`, or
/// `Failed to fetch wallets: <reason> (Character: <id>, Corp: <id>)`). Requires
/// `commanders-manage:admin`.
#[utoipa::path(
    post,
    path = "/api/admin/srp/wallets",
    tag = tags::SRP,
    operation_id = "list_srp_wallets",
    security(("session" = ["commanders-manage:admin"])),
    responses((status = 200, description = "The wallets, or why they could not be read.", body = WalletsResponse)),
)]
async fn list_wallets(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
) -> ApiResult<Json<WalletsResponse>> {
    account.require_access(ADMIN)?;
    let Some(sa) = service_account::active(state.db()).await? else {
        return Ok(Json(WalletsResponse {
            success: false,
            message: NO_SERVICE_ACCOUNT.into(),
            wallets: None,
        }));
    };
    Ok(Json(match state.srp()?.wallets(&sa).await {
        Ok(wallets) => WalletsResponse {
            success: true,
            message: format!("Found {} available corporation wallets", wallets.len()),
            wallets: Some(wallets),
        },
        Err(e) => WalletsResponse {
            success: false,
            message: format!(
                "Failed to fetch wallets: {e} (Character: {}, Corp: {})",
                sa.character_id, sa.corporation_id
            ),
            wallets: None,
        },
    }))
}

#[derive(Serialize, ToSchema)]
struct JournalResponse {
    success: bool,
    /// What happened, for display.
    message: String,
    /// Page 1 of the journal, newest first (when `success`).
    entries: Option<Vec<WalletJournalEntry>>,
    /// The wallet division read (the first one ESI lists).
    wallet_id: Option<i32>,
}

/// SRP wallet journal
///
/// Page 1 of the journal of the first wallet division ESI lists for the service account's
/// corporation. Without a service account, or when the corporation has no wallets, `success` is
/// `false` (`No service account configured`, `No wallets found`); a failure reading the journal is
/// also `success: false` (`Failed to fetch journal for wallet <division>: <reason>`). A failure
/// *listing the wallets* is an error response, as in the legacy waitlist: ESI's status with
/// `ESI error: <ESI's JSON error>`, or 401 `ESI error: no ESI token found` / `ESI error: missing ESI
/// scope` when the service account's token is unusable. Requires `commanders-manage:admin`.
#[utoipa::path(
    post,
    path = "/api/admin/srp/journal",
    tag = tags::SRP,
    operation_id = "fetch_srp_journal",
    security(("session" = ["commanders-manage:admin"])),
    responses((status = 200, description = "The journal page, or why it could not be read.", body = JournalResponse)),
)]
async fn fetch_journal(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
) -> ApiResult<Json<JournalResponse>> {
    account.require_access(ADMIN)?;
    let Some(sa) = service_account::active(state.db()).await? else {
        return Ok(Json(JournalResponse {
            success: false,
            message: NO_SERVICE_ACCOUNT.into(),
            entries: None,
            wallet_id: None,
        }));
    };
    let srp = state.srp()?;
    // A failure listing the wallets is an error response (legacy behaviour).
    let wallets = srp.wallets(&sa).await?;
    let Some(first) = wallets.first() else {
        return Ok(Json(JournalResponse {
            success: false,
            message: "No wallets found".into(),
            entries: None,
            wallet_id: None,
        }));
    };
    let wallet_id = first.division;
    Ok(Json(match srp.journal(&sa, wallet_id).await {
        Ok(entries) => JournalResponse {
            success: true,
            message: format!(
                "Successfully fetched journal for wallet {wallet_id}. Found {} entries.",
                entries.len()
            ),
            entries: Some(entries),
            wallet_id: Some(wallet_id),
        },
        Err(e) => JournalResponse {
            success: false,
            message: format!("Failed to fetch journal for wallet {wallet_id}: {e}"),
            entries: None,
            wallet_id: Some(wallet_id),
        },
    }))
}

#[derive(Serialize, ToSchema)]
struct SrpResponse {
    /// Always `true` (failures are error responses).
    success: bool,
    /// What happened, for display.
    message: String,
}

fn ok(message: impl Into<String>) -> Json<SrpResponse> {
    Json(SrpResponse {
        success: true,
        message: message.into(),
    })
}

/// Remove the SRP service account
///
/// Deletes every service account, so a new one can be set up. Requires `commanders-manage:admin`.
#[utoipa::path(
    post,
    path = "/api/admin/srp/remove",
    tag = tags::SRP,
    operation_id = "remove_srp_service_account",
    security(("session" = ["commanders-manage:admin"])),
    responses((status = 200, description = "Removed.", body = SrpResponse)),
)]
async fn remove_service_account(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
) -> ApiResult<Json<SrpResponse>> {
    account.require_access(ADMIN)?;
    service_account::remove_all(state.db()).await?;
    Ok(ok(
        "Service account removed. You can now set up a new service account.",
    ))
}

/// Reconcile SRP payments
///
/// Runs a wallet reconciliation now (the background updater also runs it periodically): reads the
/// service account's journal back to `srp.journal_days` (8) days, **wipes and rebuilds** the
/// payment list from the deposits that match a tier price, marks `approved` reports whose payout
/// appears in the journal as `paid` (posting their Discord notices), then voids per-focus coverage
/// if the incursion focus has ended. Requires `commanders-manage:admin`.
#[utoipa::path(
    post,
    path = "/api/admin/srp/process",
    tag = tags::SRP,
    operation_id = "process_srp_payments",
    security(("session" = ["commanders-manage:admin"])),
    responses(
        (status = 200, description = "Processed.", body = String, content_type = "text/plain", example = "SRP payments processed successfully"),
        (status = 400, description = "`No SRP service account configured`, `ESI request failed: <reason>` (reading the journal), or `Invalid date format` (a journal entry with an unparsable date).", body = String, content_type = "text/plain"),
    ),
)]
async fn process(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
) -> ApiResult<&'static str> {
    account.require_access(ADMIN)?;
    state.srp()?.process_and_validate().await?;
    Ok("SRP payments processed successfully")
}

// --- coverage ------------------------------------------------------------------------------

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
struct StatusQuery {
    /// The character to check: one of the caller's own characters (main or alt), or any
    /// character with `waitlist-manage`.
    character_id: i64,
}

#[derive(Serialize, ToSchema)]
struct StatusResponse {
    /// `Unpaid`, `Paid until end of focus` or `Paid until YYYY-MM-DD HH:MM UTC`.
    status: Option<String>,
    /// ISK paid for the covering payment.
    payment_amount: Option<f64>,
    /// `daily` or `per_focus` (when covered).
    coverage_type: Option<String>,
    /// The configured `srp_updater.fc_srp_price_factor`: FCs may pay the tier prices times this.
    fc_srp_price_factor: f64,
    /// The configured tier prices.
    srp_tiers: TierTable,
}

/// SRP coverage of a pilot
///
/// Whether a character is currently covered, by its own payment, its main's or one of its
/// account's alts' (coverage is bought per character name). An unknown character is `Unpaid`. The
/// caller may check their own characters (main or alt), or any character with `waitlist-manage`;
/// anyone else gets **400 `Unauthorized`** (legacy quirk, kept). Also returns the FC price factor
/// and the configured tier prices so clients need not hardcode them.
#[utoipa::path(
    get,
    path = "/api/srp/status",
    tag = tags::SRP,
    operation_id = "srp_status",
    security(("session" = [])),
    params(StatusQuery),
    responses(
        (status = 200, description = "The character's coverage.", body = StatusResponse),
        (status = 400, description = "`Unauthorized`: the caller may not see this character.", body = String, content_type = "text/plain"),
    ),
)]
async fn status(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    Query(q): Query<StatusQuery>,
) -> ApiResult<Json<StatusResponse>> {
    let db = state.db();
    let character_id = q.character_id;
    let allowed = character_id == account.id
        || account.has_access("waitlist-manage")
        || sqlx::query_scalar::<_, i64>(
            "SELECT account_id FROM alt_character WHERE (account_id = ? AND alt_id = ?) \
             OR (account_id = ? AND alt_id = ?) LIMIT 1",
        )
        .bind(account.id)
        .bind(character_id)
        .bind(character_id)
        .bind(account.id)
        .fetch_optional(db)
        .await?
        .is_some();
    if !allowed {
        return Err(ApiError::bad_request("Unauthorized"));
    }
    let status = state.srp()?.pilot_status(character_id).await?;
    Ok(Json(StatusResponse {
        status: status.status,
        payment_amount: status.payment_amount,
        coverage_type: status.coverage_type,
        fc_srp_price_factor: state.config().srp_updater.fc_srp_price_factor,
        srp_tiers: TierTable::from_config(&state.config().srp),
    }))
}

#[derive(Serialize, ToSchema)]
struct AllStatusesResponse {
    /// Every detected payment, latest expiry first.
    statuses: Vec<SrpPayment>,
    /// The configured `srp_updater.fc_srp_price_factor`.
    fc_srp_price_factor: f64,
    /// The configured tier prices.
    srp_tiers: TierTable,
}

/// All SRP payments
///
/// Every detected subscription payment (latest expiry first), with the FC price factor and the
/// configured tier prices. The list only holds payments from the last `srp.journal_days` (8) days:
/// each reconciliation rebuilds it. Requires `commanders-manage:admin`.
#[utoipa::path(
    get,
    path = "/api/admin/srp/all-statuses",
    tag = tags::SRP,
    operation_id = "list_srp_payments",
    security(("session" = ["commanders-manage:admin"])),
    responses((status = 200, description = "The payments.", body = AllStatusesResponse)),
)]
async fn all_statuses(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
) -> ApiResult<Json<AllStatusesResponse>> {
    account.require_access(ADMIN)?;
    Ok(Json(AllStatusesResponse {
        statuses: state.srp()?.all_payments().await?,
        fc_srp_price_factor: state.config().srp_updater.fc_srp_price_factor,
        srp_tiers: TierTable::from_config(&state.config().srp),
    }))
}

// --- config and focus ----------------------------------------------------------------------

#[derive(Serialize, ToSchema)]
struct ConfigResponse {
    /// Every `srp_config` entry (e.g. `show_fc_killmail_links`).
    config: HashMap<String, String>,
}

/// SRP settings
///
/// The `srp_config` key/value settings. Requires `commanders-manage:admin`.
#[utoipa::path(
    get,
    path = "/api/admin/srp/config",
    tag = tags::SRP,
    operation_id = "get_srp_config",
    security(("session" = ["commanders-manage:admin"])),
    responses((status = 200, description = "The settings.", body = ConfigResponse)),
)]
async fn get_config(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
) -> ApiResult<Json<ConfigResponse>> {
    account.require_access(ADMIN)?;
    Ok(Json(ConfigResponse {
        config: payments::config_map(state.db()).await?,
    }))
}

#[derive(Deserialize, ToSchema)]
struct UpdateConfigRequest {
    /// Any key; `show_fc_killmail_links` (`true`/`1` to show links to FCs) is the one the
    /// server reads.
    key: String,
    value: String,
}

/// Set an SRP setting
///
/// Creates or replaces one `srp_config` entry (any key is accepted). Requires
/// `commanders-manage:admin`.
#[utoipa::path(
    post,
    path = "/api/admin/srp/config",
    tag = tags::SRP,
    operation_id = "update_srp_config",
    security(("session" = ["commanders-manage:admin"])),
    request_body = UpdateConfigRequest,
    responses((status = 200, description = "Saved.", body = String, content_type = "text/plain", example = "Configuration updated successfully")),
)]
async fn update_config(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    JsonBody(input): JsonBody<UpdateConfigRequest>,
) -> ApiResult<&'static str> {
    account.require_access(ADMIN)?;
    payments::set_config(state.db(), &input.key, &input.value, account.id).await?;
    Ok("Configuration updated successfully")
}

#[derive(Serialize, ToSchema)]
struct FocusResponse {
    /// The latest focus status, `null` if none was recorded yet.
    focus_status: Option<IncursionFocus>,
}

/// Incursion focus status
///
/// The latest incursion focus record (written by the incursion updater); the end of a focus voids
/// per-focus coverage. Requires `commanders-manage:admin`.
#[utoipa::path(
    get,
    path = "/api/admin/srp/incursion-focus",
    tag = tags::SRP,
    security(("session" = ["commanders-manage:admin"])),
    responses((status = 200, description = "The focus status, or `null`.", body = FocusResponse)),
)]
async fn incursion_focus(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
) -> ApiResult<Json<FocusResponse>> {
    account.require_access(ADMIN)?;
    Ok(Json(FocusResponse {
        focus_status: focus::current_focus(state.db()).await?,
    }))
}

#[derive(Serialize, ToSchema)]
struct FocusEndResponse {
    /// When the last focus ended (Unix seconds).
    focus_end_timestamp: Option<i64>,
    /// The same time as `YYYY-MM-DD HH:MM:SS UTC`.
    formatted_date: Option<String>,
}

/// Incursion focus end
///
/// When the last incursion focus ended, as Unix seconds and formatted (both `null` if unknown).
/// Requires `commanders-manage:admin`.
#[utoipa::path(
    get,
    path = "/api/admin/srp/focus-end-timestamp",
    tag = tags::SRP,
    security(("session" = ["commanders-manage:admin"])),
    responses((status = 200, description = "The focus end time.", body = FocusEndResponse)),
)]
async fn focus_end_timestamp(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
) -> ApiResult<Json<FocusEndResponse>> {
    account.require_access(ADMIN)?;
    let (focus_end_timestamp, formatted_date) = focus::focus_end(state.db()).await?;
    Ok(Json(FocusEndResponse {
        focus_end_timestamp,
        formatted_date,
    }))
}

// --- reports: reading ----------------------------------------------------------------------

#[derive(Serialize, ToSchema)]
struct ReportsResponse {
    /// Newest first.
    reports: Vec<SrpReport>,
}

/// SRP reports
///
/// Every loss report, newest first. Requires `commanders-manage:admin`.
#[utoipa::path(
    get,
    path = "/api/admin/srp/reports",
    tag = tags::SRP,
    operation_id = "list_srp_reports",
    security(("session" = ["commanders-manage:admin"])),
    responses((status = 200, description = "The reports.", body = ReportsResponse)),
)]
async fn admin_reports(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
) -> ApiResult<Json<ReportsResponse>> {
    account.require_access(ADMIN)?;
    Ok(Json(ReportsResponse {
        reports: state.srp()?.all_reports().await?,
    }))
}

/// SRP reports for FCs
///
/// Every loss report, newest first. `killmail_link` is `""` unless the `show_fc_killmail_links`
/// setting is `true` or `1`. Requires `fleet-view`.
#[utoipa::path(
    get,
    path = "/api/fc/srp/reports",
    tag = tags::SRP,
    operation_id = "list_srp_reports_fc",
    security(("session" = ["fleet-view"])),
    responses((status = 200, description = "The reports.", body = ReportsResponse)),
)]
async fn fc_reports(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
) -> ApiResult<Json<ReportsResponse>> {
    account.require_access("fleet-view")?;
    let mut reports = state.srp()?.all_reports().await?;
    let show_links = payments::config_map(state.db())
        .await?
        .get("show_fc_killmail_links")
        .is_some_and(|v| v == "true" || v == "1");
    if !show_links {
        for report in &mut reports {
            report.killmail_link.clear();
        }
    }
    Ok(Json(ReportsResponse { reports }))
}

#[derive(Serialize, ToSchema)]
struct ReportResponse {
    report: SrpReport,
}

/// SRP report
///
/// One loss report. Requires `commanders-manage:admin`.
#[utoipa::path(
    get,
    path = "/api/admin/srp/reports/{killmail_id}",
    tag = tags::SRP,
    operation_id = "get_srp_report",
    security(("session" = ["commanders-manage:admin"])),
    params(("killmail_id" = i64, Path, description = "The report's killmail id.")),
    responses(
        (status = 200, description = "The report.", body = ReportResponse),
        (status = 404, description = "`SRP report not found`", body = String, content_type = "text/plain"),
    ),
)]
async fn admin_report(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    Path(killmail_id): Path<i64>,
) -> ApiResult<Json<ReportResponse>> {
    account.require_access(ADMIN)?;
    Ok(Json(ReportResponse {
        report: state.srp()?.require_report(killmail_id).await?,
    }))
}

/// SRP report killmail
///
/// The report's killmail, fetched from ESI. Requires `commanders-manage:admin`.
#[utoipa::path(
    get,
    path = "/api/admin/srp/reports/{killmail_id}/killmail",
    tag = tags::SRP,
    operation_id = "get_srp_report_killmail",
    security(("session" = ["commanders-manage:admin"])),
    params(("killmail_id" = i64, Path, description = "The report's killmail id.")),
    responses(
        (status = 200, description = "The killmail.", body = KillmailData),
        (status = 400, description = "The stored killmail link is malformed (`Invalid killmail URL format`, `Invalid killmail ID`) or ESI did not return the killmail (`Failed to fetch killmail: <reason>`).", body = String, content_type = "text/plain"),
        (status = 404, description = "`SRP report not found`", body = String, content_type = "text/plain"),
    ),
)]
async fn report_killmail(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    Path(killmail_id): Path<i64>,
) -> ApiResult<Json<KillmailData>> {
    account.require_access(ADMIN)?;
    let srp = state.srp()?;
    let report = srp.require_report(killmail_id).await?;
    Ok(Json(srp.killmail(&report.killmail_link).await?))
}

/// SRP report killmail with names
///
/// The report's killmail with every id resolved to a name (one ESI lookup with a 10 s timeout,
/// falling back to the static data for type names). Names that cannot be resolved are `null`, or
/// `Unknown` for the victim's ship and items. Requires `commanders-manage:admin`.
#[utoipa::path(
    get,
    path = "/api/admin/srp/reports/{killmail_id}/killmail/enriched",
    tag = tags::SRP,
    operation_id = "get_srp_report_killmail_enriched",
    security(("session" = ["commanders-manage:admin"])),
    params(("killmail_id" = i64, Path, description = "The report's killmail id.")),
    responses(
        (status = 200, description = "The enriched killmail.", body = EnrichedKillmail),
        (status = 400, description = "The stored killmail link is malformed (`Invalid killmail URL format`, `Invalid killmail ID`) or ESI did not return the killmail (`Failed to fetch killmail: <reason>`).", body = String, content_type = "text/plain"),
        (status = 404, description = "`SRP report not found`", body = String, content_type = "text/plain"),
    ),
)]
async fn report_killmail_enriched(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    Path(killmail_id): Path<i64>,
) -> ApiResult<Json<EnrichedKillmail>> {
    account.require_access(ADMIN)?;
    let srp = state.srp()?;
    let report = srp.require_report(killmail_id).await?;
    Ok(Json(srp.enriched_killmail(&report.killmail_link).await?))
}

#[derive(Serialize, ToSchema)]
struct FleetValidationResponse {
    /// Whether the victim was in a tracked fleet at the killmail's time (`false` for an NPC
    /// victim).
    was_in_fleet: bool,
}

/// SRP report fleet check
///
/// Whether the victim was in a tracked fleet when the ship was lost. Requires
/// `commanders-manage:admin`.
#[utoipa::path(
    get,
    path = "/api/admin/srp/reports/{killmail_id}/fleet-validation",
    tag = tags::SRP,
    operation_id = "validate_srp_report_fleet",
    security(("session" = ["commanders-manage:admin"])),
    params(("killmail_id" = i64, Path, description = "The report's killmail id.")),
    responses(
        (status = 200, description = "The check's result.", body = FleetValidationResponse),
        (status = 400, description = "`Failed to fetch killmail data: <reason>` (malformed stored link or ESI failure), or `Invalid killmail time format`.", body = String, content_type = "text/plain"),
        (status = 404, description = "`SRP report not found`", body = String, content_type = "text/plain"),
    ),
)]
async fn fleet_validation(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    Path(killmail_id): Path<i64>,
) -> ApiResult<Json<FleetValidationResponse>> {
    account.require_access(ADMIN)?;
    let srp = state.srp()?;
    let report = srp.require_report(killmail_id).await?;
    let km = srp
        .killmail(&report.killmail_link)
        .await
        .map_err(|e| ApiError::BadRequest(format!("Failed to fetch killmail data: {e}")))?;
    let timestamp = killmail_timestamp(&km.killmail_time)?;
    let was_in_fleet = match km.victim.character_id {
        Some(character_id) => srp.was_in_fleet(character_id, timestamp).await?,
        None => false,
    };
    Ok(Json(FleetValidationResponse { was_in_fleet }))
}

/// SRP report coverage check
///
/// Whether the victim had SRP coverage at the killmail's time (its own, its main's or an alt's),
/// recomputed now. Unlike the snapshot stored at submission (`srp_paid`), it ignores the voiding of
/// per-focus coverage. `alt_needs_linking` is `true` when the victim is unknown to the waitlist.
/// An NPC victim has no coverage. Requires `commanders-manage:admin`.
#[utoipa::path(
    get,
    path = "/api/admin/srp/reports/{killmail_id}/srp-validation",
    tag = tags::SRP,
    operation_id = "validate_srp_report_coverage",
    security(("session" = ["commanders-manage:admin"])),
    params(("killmail_id" = i64, Path, description = "The report's killmail id.")),
    responses(
        (status = 200, description = "The victim's coverage at the time.", body = CoverageAtTime),
        (status = 400, description = "The stored killmail link is malformed (`Invalid killmail URL format`, `Invalid killmail ID`), ESI did not return the killmail (`Failed to fetch killmail: <reason>`), or its time is unparsable (`Invalid killmail time format`).", body = String, content_type = "text/plain"),
        (status = 404, description = "`SRP report not found`", body = String, content_type = "text/plain"),
    ),
)]
async fn srp_validation(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    Path(killmail_id): Path<i64>,
) -> ApiResult<Json<CoverageAtTime>> {
    account.require_access(ADMIN)?;
    let srp = state.srp()?;
    let report = srp.require_report(killmail_id).await?;
    let km = srp.killmail(&report.killmail_link).await?;
    let timestamp = killmail_timestamp(&km.killmail_time)?;
    Ok(Json(
        srp.coverage_at(km.victim.character_id, timestamp).await?,
    ))
}

// --- appraisal and decisions ---------------------------------------------------------------

#[derive(Deserialize, ToSchema)]
struct AppraisalRequest {
    /// One line per item, as Janice parses them (e.g. `Large Shield Extender II 2`).
    destroyed_items: Vec<String>,
    /// The lost hull: `srp_modified` rules scoped to hulls only apply when it matches.
    #[serde(default)]
    hull_type_id: Option<i64>,
}

#[derive(Serialize, ToSchema)]
struct AppraisalResponse {
    /// Sum of the items' (adjusted) `effectivePrices.sellPriceTotal`, in ISK.
    total_value: f64,
    /// The number of lines in `destroyed_items`.
    item_count: usize,
    /// Janice's appraisal items, with `effectivePrices.sellPrice`/`sellPriceTotal` rewritten by
    /// the `srp_modified` rules and an `srp_override` object (`market_percent` or `flat`, plus
    /// Janice's original `janice_sell_price`/`janice_sell_price_total`) where a rule applied.
    #[schema(value_type = Vec<Object>)]
    items: Vec<serde_json::Value>,
}

/// Appraise a loss
///
/// Prices the destroyed items with Janice (Jita sell), then applies the `srp_modified` price rules
/// of `modules.yaml` (a percentage of the market price or a flat price, optionally scoped to hulls).
/// An empty item list is worth 0 without calling Janice. Requires `commanders-manage:admin`.
#[utoipa::path(
    post,
    path = "/api/admin/srp/appraisal",
    tag = tags::SRP,
    operation_id = "appraise_srp_loss",
    security(("session" = ["commanders-manage:admin"])),
    request_body = AppraisalRequest,
    responses(
        (status = 200, description = "The appraisal.", body = AppraisalResponse),
        (status = 400, description = "Janice failed: `Failed to call Janice API: <reason>`, `Failed to read Janice API response: <reason>`, `Janice API returned error: <status> - Body: <body>` or `Failed to parse Janice API response: <reason>`.", body = String, content_type = "text/plain"),
    ),
)]
async fn appraisal(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    JsonBody(input): JsonBody<AppraisalRequest>,
) -> ApiResult<Json<AppraisalResponse>> {
    account.require_access(ADMIN)?;
    let data = state.data_store()?.get();
    let (total_value, items) = state
        .srp()?
        .appraise(
            &input.destroyed_items,
            input.hull_type_id,
            &data.modules.srp_modified,
            Some(data.eve()),
        )
        .await?;
    Ok(Json(AppraisalResponse {
        total_value,
        item_count: input.destroyed_items.len(),
        items,
    }))
}

#[derive(Deserialize, ToSchema)]
struct ApproveRequest {
    /// ISK to pay out (in game, by hand).
    payout_amount: f64,
}

/// Approve an SRP report
///
/// Sets the report to `approved` with the payout amount (unless it is already `paid`), and updates
/// its Discord notice. No ISK moves: the payout is made in game, and the next reconciliation marks
/// the report `paid` when it finds a matching wallet transfer. Requires `commanders-manage:admin`.
#[utoipa::path(
    post,
    path = "/api/admin/srp/reports/{killmail_id}/approve",
    tag = tags::SRP,
    operation_id = "approve_srp_report",
    security(("session" = ["commanders-manage:admin"])),
    params(("killmail_id" = i64, Path, description = "The report's killmail id.")),
    request_body = ApproveRequest,
    responses(
        (status = 200, description = "Approved.", body = SrpResponse),
        (status = 400, description = "`SRP report is already paid`", body = String, content_type = "text/plain"),
        (status = 404, description = "`SRP report not found`", body = String, content_type = "text/plain"),
    ),
)]
async fn approve(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    Path(killmail_id): Path<i64>,
    JsonBody(input): JsonBody<ApproveRequest>,
) -> ApiResult<Json<SrpResponse>> {
    account.require_access(ADMIN)?;
    state
        .srp()?
        .approve_report(killmail_id, input.payout_amount)
        .await?;
    Ok(ok("SRP report approved successfully"))
}

#[derive(Deserialize, ToSchema)]
struct DenyRequest {
    /// Shown to the pilot.
    reason: String,
}

/// Deny an SRP report
///
/// Sets the report to `rejected` with the reason (unless it is already `paid`), and updates its
/// Discord notice. Requires `commanders-manage:admin`.
#[utoipa::path(
    post,
    path = "/api/admin/srp/reports/{killmail_id}/deny",
    tag = tags::SRP,
    operation_id = "deny_srp_report",
    security(("session" = ["commanders-manage:admin"])),
    params(("killmail_id" = i64, Path, description = "The report's killmail id.")),
    request_body = DenyRequest,
    responses(
        (status = 200, description = "Denied.", body = SrpResponse),
        (status = 400, description = "`SRP report is already paid`", body = String, content_type = "text/plain"),
        (status = 404, description = "`SRP report not found`", body = String, content_type = "text/plain"),
    ),
)]
async fn deny(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    Path(killmail_id): Path<i64>,
    JsonBody(input): JsonBody<DenyRequest>,
) -> ApiResult<Json<SrpResponse>> {
    account.require_access(ADMIN)?;
    state.srp()?.deny_report(killmail_id, &input.reason).await?;
    Ok(ok("SRP report denied successfully"))
}

/// The character whose token opens in-game windows: the SRP-admin re-login, else the main.
fn window_character(account: &AuthenticatedAccount) -> i64 {
    account.window_character_id.unwrap_or(account.id)
}

fn window_error(e: wl_core::EsiError) -> ApiError {
    ApiError::BadRequest(format!("Failed to open character window: {e}"))
}

/// Test the in-game window
///
/// Opens the show-info window of a fixed character (CCP Falcon, 2112625428) in the game client of
/// the caller's window character: the character of the session's SRP administrator login
/// (`GET /api/auth/login_url?srp_admin=true`), else the account's main. Requires
/// `commanders-manage:admin`.
#[utoipa::path(
    post,
    path = "/api/admin/srp/test-character-window",
    tag = tags::SRP,
    security(("session" = ["commanders-manage:admin"])),
    responses(
        (status = 200, description = "The window was opened.", body = SrpResponse),
        (status = 400, description = "`Failed to open character window: <reason>`, e.g. when the window character has no token with the `esi-ui.open_window.v1` scope.", body = String, content_type = "text/plain"),
    ),
)]
async fn test_character_window(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
) -> ApiResult<Json<SrpResponse>> {
    account.require_access(ADMIN)?;
    let esi_character = window_character(&account);
    state
        .esi()?
        .open_information_window(esi_character, TEST_WINDOW_TARGET)
        .await
        .map_err(window_error)?;
    Ok(ok(format!(
        "Character window opened for character ID: {TEST_WINDOW_TARGET} using ESI character: {esi_character}"
    )))
}

/// Open the victim's info window
///
/// Opens the show-info window of the report's victim in the game client of the caller's window
/// character (see `POST /api/admin/srp/test-character-window`). For an NPC victim nothing is
/// opened and the message says so. Requires `commanders-manage:admin`.
#[utoipa::path(
    post,
    path = "/api/admin/srp/reports/{killmail_id}/open-victim-window",
    tag = tags::SRP,
    security(("session" = ["commanders-manage:admin"])),
    params(("killmail_id" = i64, Path, description = "The report's killmail id.")),
    responses(
        (status = 200, description = "The window was opened (or the victim is an NPC).", body = SrpResponse),
        (status = 400, description = "The killmail could not be fetched (`Invalid killmail URL format`, `Invalid killmail ID`, `Failed to fetch killmail: <reason>`), or `Failed to open character window: <reason>`.", body = String, content_type = "text/plain"),
        (status = 404, description = "`SRP report not found`", body = String, content_type = "text/plain"),
    ),
)]
async fn open_victim_window(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    Path(killmail_id): Path<i64>,
) -> ApiResult<Json<SrpResponse>> {
    account.require_access(ADMIN)?;
    let srp = state.srp()?;
    let report = srp.require_report(killmail_id).await?;
    let km = srp.killmail(&report.killmail_link).await?;
    let Some(victim) = km.victim.character_id else {
        return Ok(ok("Victim is an NPC, no character window to open"));
    };
    state
        .esi()?
        .open_information_window(window_character(&account), victim)
        .await
        .map_err(window_error)?;
    Ok(ok(format!(
        "Character window opened for victim ID: {victim}"
    )))
}

// --- FC submission and editing -------------------------------------------------------------

#[derive(Deserialize, ToSchema)]
struct SubmitRequest {
    /// The ESI killmail URL, `https://esi.evetech.net/latest/killmails/<id>/<hash>/`.
    killmail_link: String,
    description: String,
    /// Whether the loot was returned to the victim.
    loot_returned: bool,
}

/// Submit an SRP report
///
/// Files a loss report for a killmail as `pending`. The server fetches the killmail from ESI,
/// records the victim's name and ship, snapshots the victim's coverage at the killmail's time
/// (`srp_paid`) and the composition of the tracked fleet, then posts a Discord notice when
/// configured. Requires `fleet-view`.
#[utoipa::path(
    post,
    path = "/api/fc/srp/submit",
    tag = tags::SRP,
    operation_id = "submit_srp_report",
    security(("session" = ["fleet-view"])),
    request_body = SubmitRequest,
    responses(
        (status = 200, description = "Submitted.", body = SrpResponse),
        (status = 400, description = "`Invalid killmail link: <reason>`, `Failed to fetch killmail: <reason>`, `Invalid killmail time format`, or `An SRP report for this killmail already exists. Use the update button to modify existing reports.`", body = String, content_type = "text/plain"),
    ),
)]
async fn submit(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    JsonBody(input): JsonBody<SubmitRequest>,
) -> ApiResult<Json<SrpResponse>> {
    account.require_access("fleet-view")?;
    state
        .srp()?
        .submit_report(
            &input.killmail_link,
            &input.description,
            input.loot_returned,
            account.id,
        )
        .await?;
    Ok(ok("SRP report submitted successfully"))
}

/// The submitter or an admin may edit a report.
async fn authorize_edit(
    state: &AppState,
    account: &AuthenticatedAccount,
    killmail_id: i64,
    denied: &str,
) -> ApiResult<()> {
    let submitter = state.srp()?.report_submitter(killmail_id).await?;
    if submitter != account.id && !account.has_access(ADMIN) {
        return Err(ApiError::forbidden(denied));
    }
    Ok(())
}

#[derive(Deserialize, ToSchema)]
struct UpdateRequest {
    description: String,
    /// Whether the loot was returned to the victim.
    loot_returned: bool,
}

/// Edit an SRP report
///
/// Changes a report's description and loot flag and recomputes its fleet composition (refetching
/// the killmail). The status is unchanged and no notice is sent. Requires `fleet-view`; only the
/// FC who submitted the report, or a `commanders-manage:admin`, may edit it.
#[utoipa::path(
    post,
    path = "/api/fc/srp/update/{killmail_id}",
    tag = tags::SRP,
    operation_id = "update_srp_report",
    security(("session" = ["fleet-view"])),
    params(("killmail_id" = i64, Path, description = "The report's killmail id.")),
    request_body = UpdateRequest,
    responses(
        (status = 200, description = "Updated.", body = SrpResponse),
        (status = 400, description = "The stored killmail link is malformed (`Invalid killmail URL format`, `Invalid killmail ID`), ESI did not return the killmail (`Failed to fetch killmail: <reason>`), or its time is unparsable (`Invalid killmail time format`).", body = String, content_type = "text/plain"),
        (status = 403, description = "`You can only update SRP reports that you submitted`", body = String, content_type = "text/plain"),
        (status = 404, description = "`SRP report not found`", body = String, content_type = "text/plain"),
    ),
)]
async fn update(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    Path(killmail_id): Path<i64>,
    JsonBody(input): JsonBody<UpdateRequest>,
) -> ApiResult<Json<SrpResponse>> {
    account.require_access("fleet-view")?;
    authorize_edit(
        &state,
        &account,
        killmail_id,
        "You can only update SRP reports that you submitted",
    )
    .await?;
    state
        .srp()?
        .update_report(killmail_id, &input.description, input.loot_returned)
        .await?;
    Ok(ok("SRP report updated successfully"))
}

#[derive(Serialize, ToSchema)]
struct EditReport {
    killmail_id: i64,
    killmail_link: String,
    description: Option<String>,
    loot_returned: bool,
}

#[derive(Serialize, ToSchema)]
struct EditReportResponse {
    /// Always `true`.
    success: bool,
    report: EditReport,
}

/// SRP report for editing
///
/// The editable fields of a report. Requires `fleet-view`; only the FC who submitted the report, or
/// a `commanders-manage:admin`, may read it here.
#[utoipa::path(
    get,
    path = "/api/fc/srp/report/{killmail_id}",
    tag = tags::SRP,
    operation_id = "get_srp_report_for_edit",
    security(("session" = ["fleet-view"])),
    params(("killmail_id" = i64, Path, description = "The report's killmail id.")),
    responses(
        (status = 200, description = "The report.", body = EditReportResponse),
        (status = 403, description = "`You can only edit SRP reports that you submitted`", body = String, content_type = "text/plain"),
        (status = 404, description = "`SRP report not found`", body = String, content_type = "text/plain"),
    ),
)]
async fn report_for_edit(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    Path(killmail_id): Path<i64>,
) -> ApiResult<Json<EditReportResponse>> {
    account.require_access("fleet-view")?;
    authorize_edit(
        &state,
        &account,
        killmail_id,
        "You can only edit SRP reports that you submitted",
    )
    .await?;
    let report = state.srp()?.require_report(killmail_id).await?;
    Ok(Json(EditReportResponse {
        success: true,
        report: EditReport {
            killmail_id: report.killmail_id,
            killmail_link: report.killmail_link,
            description: report.description,
            loot_returned: report.loot_returned,
        },
    }))
}
