//! History endpoints (`/api/history/*`): fleet activity and fleet composition (`activity`),
//! skill changes (`skills`) and x-ups (`xup`).

mod activity;
mod skills;
mod xup;

use utoipa_axum::router::OpenApiRouter;

use crate::AppState;

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .merge(activity::routes())
        .merge(skills::routes())
        .merge(xup::routes())
}
