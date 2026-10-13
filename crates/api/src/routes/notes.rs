//! Character notes (DB table `character_note`).
//!
//! - `GET /api/notes?character_id=` (`notes-view`): the character's notes, oldest first.
//! - `POST /api/notes/add` `{character_id, note}` (`notes-add`): note of 10 to 5000 bytes, else
//!   400 `Invalid note`; text `OK`.
//! - `POST /api/notes/toggle_waitlist` `{note_id, show_on_waitlist}` (`notes-add`): pin a note on
//!   the waitlist (at most one per character) or unpin it; text `OK`. Publishes
//!   `waitlist_update` `{waitlist_id}` on `waitlist` for each open waitlist the character is on.
//!   Like the legacy waitlist, that lookup matches the noted character against the entry's
//!   account (main) id, so notes on alts don't trigger a refresh.

use crate::extract::Query;
use axum::Json;
use axum::extract::State;
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use wl_core::types::Character;
use wl_live::{events, topics};

use crate::openapi::tags;
use crate::{ApiError, ApiResult, AppState, AuthenticatedAccount, JsonBody};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_notes))
        .routes(routes!(add_note))
        .routes(routes!(toggle_waitlist_note))
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
struct ListQuery {
    /// The character the notes are about.
    character_id: i64,
}

/// A note about a pilot.
#[derive(Serialize, ToSchema)]
struct NotesListNote {
    id: i64,
    /// Who wrote the note (`corporation_id` is always null).
    author: Character,
    /// When the note was written (Unix seconds).
    logged_at: i64,
    note: String,
    /// Whether the note is pinned on the character's waitlist entry.
    show_on_waitlist: bool,
}

#[derive(Serialize, ToSchema)]
struct NotesList {
    /// Oldest first.
    notes: Vec<NotesListNote>,
}

#[derive(sqlx::FromRow)]
struct NoteRow {
    id: i64,
    author_id: i64,
    author_name: String,
    note: String,
    logged_at: i64,
    show_on_waitlist: i8,
}

/// Notes about a pilot
///
/// Every note written about the character, oldest first. Requires `notes-view`.
#[utoipa::path(
    get,
    path = "/api/notes",
    tag = tags::PILOT,
    security(("session" = ["notes-view"])),
    params(ListQuery),
    responses((status = 200, description = "The notes.", body = NotesList)),
)]
async fn list_notes(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    Query(query): Query<ListQuery>,
) -> ApiResult<Json<NotesList>> {
    account.require_access("notes-view")?;
    let rows: Vec<NoteRow> = sqlx::query_as(
        "SELECT cn.id, cn.author_id, author.name AS author_name, cn.note, cn.logged_at,
                cn.show_on_waitlist
         FROM character_note cn
         JOIN `character` author ON author.id = cn.author_id
         WHERE cn.character_id = ?
         ORDER BY cn.logged_at ASC, cn.id ASC",
    )
    .bind(query.character_id)
    .fetch_all(state.db())
    .await?;
    let notes = rows
        .into_iter()
        .map(|row| NotesListNote {
            id: row.id,
            author: Character {
                id: row.author_id,
                name: row.author_name,
                corporation_id: None,
            },
            logged_at: row.logged_at,
            note: row.note,
            show_on_waitlist: row.show_on_waitlist > 0,
        })
        .collect();
    Ok(Json(NotesList { notes }))
}

#[derive(Deserialize, ToSchema)]
struct AddNoteInput {
    /// The character the note is about.
    character_id: i64,
    /// The note text: 10 to 5000 bytes (UTF-8).
    note: String,
}

/// Add a note
///
/// Writes a note about a character, authored by the caller's main. The note must be 10 to 5000
/// bytes long (a byte count, not characters). The character id is not checked. Requires
/// `notes-add`.
#[utoipa::path(
    post,
    path = "/api/notes/add",
    tag = tags::PILOT,
    security(("session" = ["notes-add"])),
    request_body = AddNoteInput,
    responses(
        (
            status = 200,
            description = "Note added.",
            body = String,
            content_type = "text/plain",
            example = "OK",
        ),
        (
            status = 400,
            description = "`Invalid note` (shorter than 10 or longer than 5000 bytes).",
            body = String,
            content_type = "text/plain",
        ),
    ),
)]
async fn add_note(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    JsonBody(input): JsonBody<AddNoteInput>,
) -> ApiResult<&'static str> {
    account.require_access("notes-add")?;
    // Byte length, as before.
    if input.note.len() < 10 || input.note.len() > 5000 {
        return Err(ApiError::bad_request("Invalid note"));
    }
    sqlx::query(
        "INSERT INTO character_note (author_id, character_id, note, logged_at) VALUES (?, ?, ?, ?)",
    )
    .bind(account.id)
    .bind(input.character_id)
    .bind(&input.note)
    .bind(chrono::Utc::now().timestamp())
    .execute(state.db())
    .await?;
    Ok("OK")
}

#[derive(Deserialize, ToSchema)]
struct ToggleWaitlistNoteInput {
    note_id: i64,
    /// `true` pins the note (unpinning the character's other notes), `false` unpins it.
    show_on_waitlist: bool,
}

/// Pin a note on the waitlist
///
/// Pins a note on the noted character's waitlist entry (at most one pinned note per character:
/// pinning unpins the others) or unpins it. Requires `notes-add`.
///
/// Publishes `waitlist_update` `{waitlist_id}` on `waitlist` for each open waitlist the
/// character is on. Like the legacy waitlist, that lookup matches the noted character against
/// the entry's account (main) id, so pinning a note on an alt doesn't trigger a refresh.
#[utoipa::path(
    post,
    path = "/api/notes/toggle_waitlist",
    tag = tags::PILOT,
    security(("session" = ["notes-add"])),
    request_body = ToggleWaitlistNoteInput,
    responses(
        (
            status = 200,
            description = "Note updated.",
            body = String,
            content_type = "text/plain",
            example = "OK",
        ),
        (
            status = 400,
            description = "`Note not found`.",
            body = String,
            content_type = "text/plain",
        ),
    ),
)]
async fn toggle_waitlist_note(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    JsonBody(input): JsonBody<ToggleWaitlistNoteInput>,
) -> ApiResult<&'static str> {
    account.require_access("notes-add")?;
    let character_id: i64 =
        sqlx::query_scalar("SELECT character_id FROM character_note WHERE id = ?")
            .bind(input.note_id)
            .fetch_optional(state.db())
            .await?
            .ok_or_else(|| ApiError::bad_request("Note not found"))?;

    let mut tx = state.db().begin().await?;
    if input.show_on_waitlist {
        sqlx::query("UPDATE character_note SET show_on_waitlist = 0 WHERE character_id = ?")
            .bind(character_id)
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query("UPDATE character_note SET show_on_waitlist = ? WHERE id = ?")
        .bind(i8::from(input.show_on_waitlist))
        .bind(input.note_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;

    let waitlists: Vec<i64> = sqlx::query_scalar(
        "SELECT we.waitlist_id
         FROM waitlist_entry we
         JOIN waitlist w ON w.id = we.waitlist_id AND w.is_open = 1
         WHERE we.account_id = ?",
    )
    .bind(character_id)
    .fetch_all(state.db())
    .await?;
    for waitlist_id in waitlists {
        state
            .live()
            .publish_json(
                topics::WAITLIST,
                events::WAITLIST_UPDATE,
                &json!({ "waitlist_id": waitlist_id }),
            )
            .map_err(ApiError::internal)?;
    }
    Ok("OK")
}
