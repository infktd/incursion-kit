//! Waitlist endpoints (`/api/waitlist*`).

mod approve;
mod empty;
mod invite;
mod list;
pub mod notify;
mod open;
mod remove;
mod xup;

use utoipa_axum::router::OpenApiRouter;

use crate::AppState;

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .merge(list::routes())
        .merge(approve::routes())
        .merge(open::routes())
        .merge(empty::routes())
        .merge(remove::routes())
        .merge(invite::routes())
        .merge(xup::routes())
}
