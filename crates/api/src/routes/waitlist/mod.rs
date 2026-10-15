//! Waitlist endpoints (`/api/waitlist*`, and `/api/waitlists` + `/api/admin/waitlists*` to
//! administer the waitlists themselves).

mod approve;
mod empty;
mod gates;
mod invite;
mod list;
pub mod notify;
mod open;
mod remove;
mod waitlists;
mod xup;

pub(crate) use waitlists::{require_active, waitlist_rows};

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
        .merge(gates::routes())
        .merge(waitlists::routes())
}
