//! Route modules, one per legacy route file. Each exposes `routes() -> OpenApiRouter<AppState>` with
//! full literal paths (there is no mount prefix).

mod admin;
mod announcements;
mod auth;
mod badges;
mod bans;
mod categories;
mod commanders;
mod fittings;
mod fleet;
mod guides;
mod healthcheck;
mod history;
mod implants;
mod incursions;
mod locales;
mod modules;
mod notes;
mod pilot;
mod search;
mod skillplans;
mod skills;
mod srp;
pub mod sse;
mod statistics;
mod waitlist;
mod window;

use utoipa_axum::router::OpenApiRouter;

use crate::{ApiError, ApiResult, AppState};

/// Run blocking work (file I/O, image processing) off the async runtime.
pub(crate) async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> ApiResult<T> + Send + 'static,
) -> ApiResult<T> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(ApiError::internal)?
}

/// All API routes, without middleware or fallbacks (see [`crate::router`]).
pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .merge(admin::routes())
        .merge(announcements::routes())
        .merge(auth::routes())
        .merge(sse::routes())
        .merge(skills::routes())
        .merge(pilot::routes())
        .merge(history::routes())
        .merge(window::routes())
        .merge(badges::routes())
        .merge(bans::routes())
        .merge(commanders::routes())
        .merge(modules::routes())
        .merge(search::routes())
        .merge(categories::routes())
        .merge(fleet::routes())
        .merge(waitlist::routes())
        .merge(statistics::routes())
        .merge(healthcheck::routes())
        .merge(locales::routes())
        .merge(guides::routes())
        .merge(implants::routes())
        .merge(notes::routes())
        .merge(skillplans::routes())
        .merge(srp::routes())
        .merge(fittings::routes())
        .merge(incursions::routes())
}
