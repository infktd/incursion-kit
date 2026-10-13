//! The waitlist itself.
//!
//! - `GET /api/waitlist?waitlist_id=` (any logged-in account): `{open, waitlist, categories}`.
//!   `waitlist` is `null` when the list is closed or doesn't exist; `categories` are category
//!   names. What each entry shows depends on the viewer, exactly as in the legacy waitlist:
//!   - the entry's and fits' characters, hours in fleet, review comment, x-up message and all
//!     tags: own entry or `waitlist-view` (others see only the public tags);
//!   - DNA, implants and fit analysis: own entry or `waitlist-view` + `fit-view`;
//!   - `can_remove`: own entry or `waitlist-manage`;
//!   - `waitlist_note` (pinned pilot note): `waitlist-view`.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::extract::Query;
use axum::Json;
use axum::extract::State;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use wl_core::types::{Character, Hull, TypeId};

use crate::openapi::tags;
use crate::{ApiError, ApiResult, AppState, AuthenticatedAccount};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(list))
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
struct ListQuery {
    waitlist_id: i64,
}

#[derive(Debug, Serialize, ToSchema)]
struct WaitlistResponse {
    /// Whether the waitlist is open (`false` for an unknown waitlist).
    open: bool,
    /// The entries in join order; `null` when the waitlist is closed or doesn't exist.
    waitlist: Option<Vec<WaitlistEntry>>,
    /// Names of the waitlist categories, in configured order.
    categories: Vec<String>,
}

/// One account's x-up: its fits.
#[derive(Debug, Serialize, ToSchema)]
struct WaitlistEntry {
    /// Entry ID (for `POST /api/waitlist/remove_x`).
    id: i64,
    fits: Vec<WaitlistEntryFit>,
    /// The account's main (`corporation_id` is always `null`). Own entry or `waitlist-view`
    /// only.
    character: Option<Character>,
    /// When the account joined (Unix seconds).
    joined_at: i64,
    /// Own entry or `waitlist-manage`.
    can_remove: bool,
    /// The pinned pilot note on the account's main. `waitlist-view` only.
    waitlist_note: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
struct WaitlistEntryFit {
    /// Fit x-up ID (for approve, reject, invite and remove_fit).
    id: i64,
    approved: bool,
    /// Category name.
    category: String,
    hull: Hull,
    /// The pilot flying the fit (`corporation_id` is always `null`). Own entry or
    /// `waitlist-view` only.
    character: Option<Character>,
    /// All tags for the own entry or with `waitlist-view`; otherwise only the public tags.
    tags: Vec<String>,
    /// Whole hours in fleet when x'd up. Own entry or `waitlist-view` only.
    hours_in_fleet: Option<i64>,
    /// Own entry or `waitlist-view` only.
    review_comment: Option<String>,
    /// The fit as DNA. Own entry or `waitlist-view` + `fit-view` only.
    dna: Option<String>,
    /// The pilot's implant type IDs. Own entry or `waitlist-view` + `fit-view` only.
    implants: Option<Vec<TypeId>>,
    /// How the fit differs from the doctrine fit it matched (`null` when none was compared).
    /// Own entry or `waitlist-view` + `fit-view` only.
    #[schema(value_type = Option<wl_fitting::FitAnalysis>)]
    fit_analysis: Option<serde_json::Value>,
    is_alt: bool,
    /// The pilot's x-up message. Own entry or `waitlist-view` only.
    messagexup: Option<String>,
}

#[derive(sqlx::FromRow)]
struct Row {
    we_id: i64,
    we_joined_at: i64,
    we_account_id: i64,
    wef_id: i64,
    wef_approved: i8,
    wef_category: String,
    wef_cached_time_in_fleet: i64,
    wef_review_comment: Option<String>,
    wef_tags: String,
    wef_fit_analysis: Option<String>,
    wef_is_alt: i8,
    wef_messagexup: Option<String>,
    char_wef_id: i64,
    char_wef_name: String,
    char_we_id: i64,
    char_we_name: String,
    fitting_dna: String,
    fitting_hull: TypeId,
    implant_set_implants: String,
}

fn character(id: i64, name: String) -> Character {
    Character {
        id,
        name,
        corporation_id: None,
    }
}

/// The waitlist
///
/// A waitlist's entries with their fits. Any logged-in account may call it; what each entry
/// shows depends on the viewer, as in the legacy waitlist (see the field descriptions): the
/// own entry (or `waitlist-view`) shows the characters, hours in fleet, review comment, x-up
/// message and all tags; DNA, implants and fit analysis need the own entry or `waitlist-view`
/// and `fit-view`; `can_remove` is set for the own entry or with `waitlist-manage`; pinned
/// pilot notes need `waitlist-view`. A hull missing from the static data is a 400 `type error:
/// …`, as in the legacy waitlist.
#[utoipa::path(
    get,
    path = "/api/waitlist",
    operation_id = "get_waitlist",
    tag = tags::WAITLIST,
    security(("session" = [])),
    params(ListQuery),
    responses((status = 200, description = "The waitlist.", body = WaitlistResponse)),
)]
async fn list(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    Query(q): Query<ListQuery>,
) -> ApiResult<Json<WaitlistResponse>> {
    let data = state.data_store()?.get();
    let categories = &data.categories.categories;
    let category_names: Vec<String> = categories.iter().map(|c| c.name.clone()).collect();

    let is_open: Option<i8> = sqlx::query_scalar("SELECT is_open FROM waitlist WHERE id = ?")
        .bind(q.waitlist_id)
        .fetch_optional(state.db())
        .await?;
    if is_open.unwrap_or(0) == 0 {
        return Ok(Json(WaitlistResponse {
            open: false,
            waitlist: None,
            categories: category_names,
        }));
    }

    let rows: Vec<Row> = sqlx::query_as(
        "SELECT
            we.id we_id,
            we.joined_at we_joined_at,
            we.account_id we_account_id,
            wef.id wef_id,
            wef.approved wef_approved,
            wef.category wef_category,
            wef.cached_time_in_fleet wef_cached_time_in_fleet,
            wef.review_comment wef_review_comment,
            wef.tags wef_tags,
            wef.fit_analysis wef_fit_analysis,
            wef.is_alt wef_is_alt,
            wef.messagexup wef_messagexup,
            char_wef.id char_wef_id,
            char_wef.name char_wef_name,
            char_we.id char_we_id,
            char_we.name char_we_name,
            fitting.dna fitting_dna,
            fitting.hull fitting_hull,
            implant_set.implants implant_set_implants
        FROM waitlist_entry_fit wef
        JOIN waitlist_entry we ON wef.entry_id = we.id
        JOIN `character` char_wef ON wef.character_id = char_wef.id
        JOIN `character` char_we ON we.account_id = char_we.id
        JOIN fitting ON wef.fit_id = fitting.id
        JOIN implant_set ON wef.implant_set_id = implant_set.id
        WHERE we.waitlist_id = ?
        ORDER BY we.id ASC, wef.id ASC",
    )
    .bind(q.waitlist_id)
    .fetch_all(state.db())
    .await?;

    let eve = data.eve();
    let hulls: BTreeSet<TypeId> = rows.iter().map(|r| r.fitting_hull).collect();
    let mut hull_names = HashMap::new();
    for hull in hulls {
        hull_names.insert(hull, eve.require(hull)?.name.clone());
    }

    let can_view = account.has_access("waitlist-view");
    let can_view_fits = can_view && account.has_access("fit-view");
    let can_manage = account.has_access("waitlist-manage");

    let pinned_notes: HashMap<i64, String> = if can_view {
        sqlx::query_as::<_, (i64, String)>(
            "SELECT character_id, note FROM character_note WHERE show_on_waitlist = 1",
        )
        .fetch_all(state.db())
        .await?
        .into_iter()
        .collect()
    } else {
        HashMap::new()
    };

    let mut entries: BTreeMap<i64, WaitlistEntry> = BTreeMap::new();
    for row in rows {
        let ours = row.we_account_id == account.id;
        let entry = entries.entry(row.we_id).or_insert_with(|| WaitlistEntry {
            id: row.we_id,
            fits: Vec::new(),
            character: (ours || can_view)
                .then(|| character(row.char_we_id, row.char_we_name.clone())),
            joined_at: row.we_joined_at,
            can_remove: ours || can_manage,
            waitlist_note: pinned_notes.get(&row.we_account_id).cloned(),
        });

        // A category that is no longer in categories.yaml was a panic in the legacy waitlist.
        let category = data
            .categories
            .name_of(&row.wef_category)
            .ok_or_else(|| {
                ApiError::internal(format!("unknown waitlist category {:?}", row.wef_category))
            })?
            .to_string();
        let tags = row
            .wef_tags
            .split(',')
            .filter(|t| !t.is_empty())
            .map(str::to_string);

        let mut fit = WaitlistEntryFit {
            id: row.wef_id,
            approved: row.wef_approved > 0,
            category,
            hull: Hull {
                id: row.fitting_hull,
                name: hull_names[&row.fitting_hull].clone(),
            },
            character: None,
            tags: Vec::new(),
            hours_in_fleet: None,
            review_comment: None,
            dna: None,
            implants: None,
            fit_analysis: None,
            is_alt: row.wef_is_alt > 0,
            messagexup: None,
        };

        if ours || can_view {
            fit.character = Some(character(row.char_wef_id, row.char_wef_name));
            fit.hours_in_fleet = Some(row.wef_cached_time_in_fleet / 3600);
            fit.review_comment = row.wef_review_comment;
            fit.messagexup = row.wef_messagexup;
            fit.tags = tags.collect();
        } else {
            fit.tags = tags.filter(|t| data.public_tags.contains(t)).collect();
        }

        if ours || can_view_fits {
            fit.dna = Some(row.fitting_dna);
            fit.implants = Some(
                row.implant_set_implants
                    .split(':')
                    .filter(|s| !s.is_empty())
                    .map(str::parse)
                    .collect::<Result<_, _>>()
                    .map_err(ApiError::internal)?,
            );
            if let Some(analysis) = row.wef_fit_analysis.as_deref() {
                fit.fit_analysis = serde_json::from_str(analysis).map_err(ApiError::internal)?;
            }
        }

        entry.fits.push(fit);
    }

    Ok(Json(WaitlistResponse {
        open: true,
        waitlist: Some(entries.into_values().collect()),
        categories: category_names,
    }))
}
