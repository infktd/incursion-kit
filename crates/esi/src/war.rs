//! Wars (public). The war checker stores ETags, so these take and return them.

use serde::{Deserialize, Serialize};

use crate::error::{ErrorStyle, EsiResult};
use crate::esi::{Esi, EsiResponse, quote_etag, send, send_with_etag};
use crate::util::esi_time;

/// One side of a war.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WarEntity {
    pub corporation_id: Option<i64>,
    pub alliance_id: Option<i64>,
    /// Absent for allies.
    pub isk_destroyed: Option<f64>,
    /// Absent for allies.
    pub ships_killed: Option<i64>,
}

/// `GET /wars/{id}`, shaped as the legacy waitlist stored it (timestamps as ESI formats them).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WarDetail {
    pub id: i64,
    pub aggressor: WarEntity,
    pub defender: WarEntity,
    pub allies: Vec<WarEntity>,
    pub finished: Option<String>,
    pub declared: Option<String>,
    pub started: Option<String>,
    pub retracted: Option<String>,
    pub mutual: bool,
    pub open_for_allies: bool,
}

impl Esi {
    /// The newest war IDs (ESI's first page). With the ETag from a previous call, `data` is
    /// `None` when nothing changed. Failures are `WithMessage(status, body)`.
    pub async fn wars(&self, etag: Option<&str>) -> EsiResult<EsiResponse<Option<Vec<i64>>>> {
        let mut request = self.client().get_wars();
        if let Some(etag) = etag {
            request = request.if_none_match(quote_etag(etag));
        }
        let response = send_with_etag(request.send(), ErrorStyle::Body).await?;
        Ok(EsiResponse {
            data: response.data.map(|ids| ids.0),
            etag: response.etag,
        })
    }

    /// War IDs below `max_war_id` (the next page back).
    pub async fn wars_page(&self, max_war_id: i64) -> EsiResult<Vec<i64>> {
        let ids = send(
            self.client().get_wars().max_war_id(max_war_id).send(),
            ErrorStyle::Status,
        )
        .await?;
        Ok(ids.0)
    }

    /// One war. With a previous ETag, `data` is `None` when unchanged. A war ESI no longer
    /// knows is `WithMessage(404, ..)`.
    pub async fn war(
        &self,
        war_id: i64,
        etag: Option<&str>,
    ) -> EsiResult<EsiResponse<Option<WarDetail>>> {
        let mut request = self.client().get_wars_war_id().war_id(war_id);
        if let Some(etag) = etag {
            request = request.if_none_match(quote_etag(etag));
        }
        let response = send_with_etag(request.send(), ErrorStyle::Body).await?;
        Ok(EsiResponse {
            etag: response.etag,
            data: response.data.map(|w| WarDetail {
                id: w.id,
                aggressor: WarEntity {
                    corporation_id: w.aggressor.corporation_id,
                    alliance_id: w.aggressor.alliance_id,
                    isk_destroyed: Some(w.aggressor.isk_destroyed),
                    ships_killed: Some(w.aggressor.ships_killed),
                },
                defender: WarEntity {
                    corporation_id: w.defender.corporation_id,
                    alliance_id: w.defender.alliance_id,
                    isk_destroyed: Some(w.defender.isk_destroyed),
                    ships_killed: Some(w.defender.ships_killed),
                },
                allies: w
                    .allies
                    .into_iter()
                    .map(|a| WarEntity {
                        corporation_id: a.corporation_id,
                        alliance_id: a.alliance_id,
                        isk_destroyed: None,
                        ships_killed: None,
                    })
                    .collect(),
                finished: w.finished.as_ref().map(esi_time),
                declared: Some(esi_time(&w.declared)),
                started: w.started.as_ref().map(esi_time),
                retracted: w.retracted.as_ref().map(esi_time),
                mutual: w.mutual,
                open_for_allies: w.open_for_allies,
            }),
        })
    }
}
