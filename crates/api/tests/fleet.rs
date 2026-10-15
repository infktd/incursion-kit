//! Fleet routes against a mock ESI: status, info, register, close, members, fleetcomp and
//! update-motd.

mod common;
mod support;

use std::collections::HashSet;

use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use support::Env;
use wiremock::matchers::{body_string_contains, header, method, path};
use wiremock::{Mock, ResponseTemplate};

const FLEET: i64 = 7_000_001;
const FC: i64 = 1001;

fn bearer(character_id: i64) -> String {
    format!("Bearer access-{character_id}")
}

async fn mock_current_fleet(env: &Env, character_id: i64, fleet_id: Option<i64>) {
    let response = match fleet_id {
        Some(fleet_id) => ResponseTemplate::new(200).set_body_json(json!({
            "fleet_id": fleet_id, "fleet_boss_id": FC, "role": "fleet_commander",
            "squad_id": -1, "wing_id": -1,
        })),
        None => ResponseTemplate::new(404)
            .set_body_json(json!({"error": "Character is not in a fleet"})),
    };
    Mock::given(method("GET"))
        .and(path(format!("/characters/{character_id}/fleet")))
        .respond_with(response)
        .mount(&env.esi)
        .await;
}

/// Wings: 1 ("Main", squads 11 "Logi" and 12 "DPS") and 2 ("Alts", squad 21 "Alts").
async fn mock_wings(env: &Env, boss_id: i64) {
    Mock::given(method("GET"))
        .and(path(format!("/fleets/{FLEET}/wings")))
        .and(header("authorization", bearer(boss_id).as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"id": 1, "name": "Main", "squads": [{"id": 11, "name": "Logi"}, {"id": 12, "name": "DPS"}]},
            {"id": 2, "name": "Alts", "squads": [{"id": 21, "name": "Alts"}]},
        ])))
        .mount(&env.esi)
        .await;
}

fn member(character_id: i64, ship_type_id: i32, role: &str, wing_id: i64, squad_id: i64) -> Value {
    json!({
        "character_id": character_id, "join_time": "2024-05-01T12:00:00Z", "role": role,
        "role_name": role, "ship_type_id": ship_type_id, "solar_system_id": 30000142,
        "squad_id": squad_id, "takes_fleet_warp": true, "wing_id": wing_id,
    })
}

async fn mock_members(env: &Env, boss_id: i64, members: Value) {
    Mock::given(method("GET"))
        .and(path(format!("/fleets/{FLEET}/members")))
        .and(header("authorization", bearer(boss_id).as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(members))
        .mount(&env.esi)
        .await;
}

async fn mock_motd(env: &Env, motd: &str) {
    Mock::given(method("GET"))
        .and(path(format!("/fleets/{FLEET}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "is_free_move": false, "is_registered": false, "is_voice_enabled": false, "motd": motd,
        })))
        .mount(&env.esi)
        .await;
}

async fn register(env: &Env) {
    sqlx::query("INSERT INTO fleet (id, boss_id) VALUES (?, ?)")
        .bind(FLEET)
        .bind(FC)
        .execute(env.pool())
        .await
        .unwrap();
    for (category, wing, squad) in [("logi", 1, 11), ("bastion", 1, 12), ("other", 1, 12)] {
        sqlx::query(
            "INSERT INTO fleet_squad (fleet_id, category, wing_id, squad_id) VALUES (?, ?, ?, ?)",
        )
        .bind(FLEET)
        .bind(category)
        .bind(wing)
        .bind(squad)
        .execute(env.pool())
        .await
        .unwrap();
    }
}

fn link(id: i64, name: &str) -> String {
    format!("<a href=\"showinfo:1377//{id}\">{name}</a>")
}

#[tokio::test]
async fn status_lists_registered_fleets_for_fleet_viewers() {
    let Some(env) = Env::new().await else { return };
    let fc = env.pilot(FC, Some("trainee")).await;
    let user = env.pilot(1002, None).await;
    register(&env).await;

    assert_eq!(env.get("/api/fleet/status", None).await.status, 401);
    let res = env.get("/api/fleet/status", Some(&user)).await;
    assert_eq!((res.status.as_u16(), res.text()), (401, "access denied"));
    let res = env.get("/api/fleet/status", Some(&fc)).await;
    assert_eq!(res.status, 200);
    assert_eq!(
        res.json::<Value>(),
        json!({"fleets": [{"id": FLEET, "boss": {"id": FC, "name": "Pilot 1001"}, "waitlist_id": null}]})
    );
}

#[tokio::test]
async fn info_reports_the_fleet_and_its_wings() {
    let Some(env) = Env::new().await else { return };
    let fc = env.pilot(FC, Some("fc")).await;
    env.alt(FC, 1003).await;
    env.pilot(1004, None).await;
    mock_current_fleet(&env, FC, Some(FLEET)).await;
    mock_current_fleet(&env, 1003, Some(FLEET)).await;
    mock_current_fleet(&env, 1005, None).await;
    mock_wings(&env, FC).await;
    Mock::given(method("GET"))
        .and(path(format!("/fleets/{FLEET}/wings")))
        .and(header("authorization", bearer(1003).as_str()))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({"error": "Not boss"})))
        .mount(&env.esi)
        .await;

    let res = env
        .get(&format!("/api/fleet/info?character_id={FC}"), Some(&fc))
        .await;
    assert_eq!(res.status, 200, "{}", res.text());
    let body: Value = res.json();
    assert_eq!(body["fleet_id"], FLEET);
    assert_eq!(
        body["wings"][0],
        json!({"id": 1, "name": "Main", "squads": [{"id": 11, "name": "Logi"}, {"id": 12, "name": "DPS"}]})
    );

    // An alt that isn't the boss.
    let res = env
        .get("/api/fleet/info?character_id=1003", Some(&fc))
        .await;
    assert_eq!(
        (res.status.as_u16(), res.text()),
        (404, "You are not the fleet boss")
    );
    // Someone else's character.
    let res = env
        .get("/api/fleet/info?character_id=1004", Some(&fc))
        .await;
    assert_eq!((res.status.as_u16(), res.text()), (401, "access denied"));
    // Not in a fleet.
    env.alt(FC, 1005).await;
    let res = env
        .get("/api/fleet/info?character_id=1005", Some(&fc))
        .await;
    assert_eq!(
        (res.status.as_u16(), res.text()),
        (404, "You are not in a fleet")
    );
}

#[tokio::test]
async fn info_without_the_fleet_scope_is_a_401() {
    let Some(env) = Env::new().await else { return };
    let fc = env.pilot(FC, Some("fc")).await;
    env.grant_tokens(FC, &["publicData"]).await;
    let res = env
        .get(&format!("/api/fleet/info?character_id={FC}"), Some(&fc))
        .await;
    assert_eq!(
        (res.status.as_u16(), res.text()),
        (401, "ESI error: missing ESI scope")
    );
}

#[tokio::test]
async fn register_needs_every_category_and_replaces_squads() {
    let Some(env) = Env::new().await else { return };
    let fc = env.pilot(FC, Some("trainee")).await;
    let user = env.pilot(1002, None).await;
    let body = |assignments: Value| json!({"character_id": FC, "fleet_id": FLEET, "assignments": assignments});

    let res = env
        .post_json(
            "/api/fleet/register",
            Some(&user),
            &json!({"character_id": 1002, "fleet_id": FLEET, "assignments": {}}),
        )
        .await;
    assert_eq!((res.status.as_u16(), res.text()), (401, "access denied"));

    let res = env
        .post_json(
            "/api/fleet/register",
            Some(&fc),
            &body(json!({"logi": [1, 11], "other": [1, 12]})),
        )
        .await;
    assert_eq!(
        (res.status.as_u16(), res.text()),
        (400, "Missing assignment for Bastion")
    );
    let fleets: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM fleet")
        .fetch_one(env.pool())
        .await
        .unwrap();
    assert_eq!(fleets, 0, "rolled back");

    let res = env
        .post_json(
            "/api/fleet/register",
            Some(&fc),
            &body(
                json!({"logi": [1, 11], "bastion": [1, 12], "other": [2, 21], "unknown": [9, 9]}),
            ),
        )
        .await;
    assert_eq!((res.status.as_u16(), res.text()), (200, "OK"));
    let res = env
        .post_json(
            "/api/fleet/register",
            Some(&fc),
            &body(json!({"logi": [1, 11], "bastion": [1, 12], "other": [1, 12]})),
        )
        .await;
    assert_eq!(res.status, 200);
    let squads: Vec<(String, i64, i64)> = sqlx::query_as(
        "SELECT category, wing_id, squad_id FROM fleet_squad WHERE fleet_id = ? ORDER BY category",
    )
    .bind(FLEET)
    .fetch_all(env.pool())
    .await
    .unwrap();
    assert_eq!(
        squads,
        [
            ("bastion".into(), 1, 12),
            ("logi".into(), 1, 11),
            ("other".into(), 1, 12)
        ]
    );
    let boss: i64 = sqlx::query_scalar("SELECT boss_id FROM fleet WHERE id = ?")
        .bind(FLEET)
        .fetch_one(env.pool())
        .await
        .unwrap();
    assert_eq!(boss, FC);
}

#[tokio::test]
async fn close_kicks_everyone_else() {
    let Some(env) = Env::new().await else { return };
    let fc = env.pilot(FC, Some("trainee")).await;
    mock_current_fleet(&env, FC, Some(FLEET)).await;
    mock_members(
        &env,
        FC,
        json!([
            member(FC, 17740, "fleet_commander", -1, -1),
            member(2001, 17740, "squad_member", 1, 12),
            member(2002, 17740, "squad_member", 1, 12),
        ]),
    )
    .await;
    Mock::given(method("DELETE"))
        .and(path(format!("/fleets/{FLEET}/members/2001")))
        .and(header("authorization", bearer(FC).as_str()))
        .respond_with(ResponseTemplate::new(204))
        .expect(2)
        .mount(&env.esi)
        .await;
    // 2002 already left.
    let gone = Mock::given(method("DELETE"))
        .and(path(format!("/fleets/{FLEET}/members/2002")))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({"error": "Not found"})))
        .mount_as_scoped(&env.esi)
        .await;

    let res = env
        .post_json("/api/fleet/close", Some(&fc), &json!({"character_id": FC}))
        .await;
    assert_eq!(
        (res.status.as_u16(), res.text()),
        (200, "Removed 1 of 2 fleet members.")
    );
    drop(gone);

    Mock::given(method("DELETE"))
        .and(path(format!("/fleets/{FLEET}/members/2002")))
        .respond_with(ResponseTemplate::new(204))
        .mount(&env.esi)
        .await;
    let res = env
        .post_json("/api/fleet/close", Some(&fc), &json!({"character_id": FC}))
        .await;
    assert_eq!(
        (res.status.as_u16(), res.text()),
        (200, "All fleet members kicked.")
    );

    let user = env.pilot(1002, None).await;
    let res = env
        .post_json(
            "/api/fleet/close",
            Some(&user),
            &json!({"character_id": 1002}),
        )
        .await;
    assert_eq!((res.status.as_u16(), res.text()), (401, "access denied"));
}

#[tokio::test]
async fn members_need_a_registered_fleet_and_report_categories() {
    let Some(env) = Env::new().await else { return };
    let fc = env.pilot(FC, Some("trainee")).await;
    env.pilot(2001, None).await;
    mock_current_fleet(&env, FC, Some(FLEET)).await;
    mock_wings(&env, FC).await;
    let vindicator = env.type_id("Vindicator");
    let nestor = env.type_id("Nestor");
    mock_members(
        &env,
        FC,
        json!([
            member(FC, vindicator, "fleet_commander", -1, -1),
            member(2001, nestor, "squad_member", 1, 11),
            member(2999, vindicator, "squad_member", 2, 21),
        ]),
    )
    .await;

    let url = format!("/api/fleet/members?character_id={FC}");
    let res = env.get(&url, Some(&fc)).await;
    assert_eq!(
        (res.status.as_u16(), res.text()),
        (404, "Fleet not configured")
    );

    register(&env).await;
    let res = env.get(&url, Some(&fc)).await;
    assert_eq!(res.status, 200, "{}", res.text());
    assert_eq!(
        res.json::<Value>(),
        json!({"members": [
            {"id": FC, "name": "Pilot 1001", "ship": {"id": vindicator, "name": "Vindicator"},
             "wl_category": null, "category": null, "role": "fleet_commander"},
            {"id": 2001, "name": "Pilot 2001", "ship": {"id": nestor, "name": "Nestor"},
             "wl_category": "Logistics", "category": "Logi", "role": "squad_member"},
            {"id": 2999, "name": null, "ship": {"id": vindicator, "name": "Vindicator"},
             "wl_category": null, "category": "Alts", "role": "squad_member"},
        ]})
    );
}

#[tokio::test]
async fn fleetcomp_groups_members_and_reads_motd_roles() {
    let Some(env) = Env::new().await else { return };
    let fc = env.pilot(FC, Some("trainee")).await;
    env.pilot(2001, None).await;
    env.pilot(2002, None).await;
    register(&env).await;
    mock_current_fleet(&env, FC, Some(FLEET)).await;
    mock_wings(&env, FC).await;
    let vindicator = env.type_id("Vindicator");
    let kronos = env.type_id("Kronos");
    mock_members(
        &env,
        FC,
        json!([
            member(FC, vindicator, "fleet_commander", -1, -1),
            member(2001, kronos, "wing_commander", 1, -1),
            member(2002, vindicator, "squad_member", 1, 12),
        ]),
    )
    .await;
    mock_motd(
        &env,
        &format!(
            "Welcome<br>DDD: {}<br>LR: {}, {}",
            link(2002, "Pilot 2002"),
            link(2001, "pilot 2001"),
            link(3000, "Somebody Else")
        ),
    )
    .await;
    // 2002's corporation is at war.
    sqlx::query("INSERT INTO corporation (id, name, alliance_id, updated_at) VALUES (55, 'War Corp', NULL, 0)")
        .execute(env.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE `character` SET corporation_id = 55 WHERE id = 2002")
        .execute(env.pool())
        .await
        .unwrap();
    env.state
        .war_cache()
        .replace(HashSet::from([55]), HashSet::new());

    let res = env
        .get(
            &format!("/api/fleet/fleetcomp?character_id={FC}"),
            Some(&fc),
        )
        .await;
    assert_eq!(res.status, 200, "{}", res.text());
    let body: Value = res.json();
    let ship = |id: i32, name: &str| json!({"id": id, "name": name});
    assert_eq!(body["id"], FLEET);
    assert_eq!(
        body["member"],
        json!({"id": FC, "name": "Pilot 1001", "ship": ship(vindicator, "Vindicator"), "role": "fleet_commander", "at_war": false})
    );
    assert_eq!(
        body["wings"][0],
        json!({
            "id": 1, "name": "Main",
            "member": {"id": 2001, "name": "Pilot 2001", "ship": ship(kronos, "Kronos"), "role": "wing_commander", "at_war": false},
            "squads": [
                {"id": 11, "name": "Logi", "members": []},
                {"id": 12, "name": "DPS", "members": [
                    {"id": 2002, "name": "Pilot 2002", "ship": ship(vindicator, "Vindicator"), "role": "squad_member", "at_war": true},
                ]},
            ],
        })
    );
    assert_eq!(body["wings"][1]["member"], Value::Null);
    assert_eq!(
        body["role_assignments"],
        json!({
            "DDD": [{"name": "Pilot 2002", "in_fleet": true, "correct_hull": true, "hull_id": vindicator}],
            "LR": [
                {"name": "pilot 2001", "in_fleet": true, "correct_hull": true, "hull_id": kronos},
                {"name": "Somebody Else", "in_fleet": false, "correct_hull": null, "hull_id": null},
            ],
        })
    );
}

#[tokio::test]
async fn fleetcomp_without_a_readable_motd_has_no_role_assignments() {
    let Some(env) = Env::new().await else { return };
    let fc = env.pilot(FC, Some("trainee")).await;
    register(&env).await;
    mock_current_fleet(&env, FC, Some(FLEET)).await;
    mock_wings(&env, FC).await;
    mock_members(
        &env,
        FC,
        json!([member(
            FC,
            env.type_id("Vindicator"),
            "fleet_commander",
            -1,
            -1
        )]),
    )
    .await;
    Mock::given(method("GET"))
        .and(path(format!("/fleets/{FLEET}")))
        .respond_with(ResponseTemplate::new(502).set_body_json(json!({"error": "bad gateway"})))
        .mount(&env.esi)
        .await;

    let res = env
        .get(
            &format!("/api/fleet/fleetcomp?character_id={FC}"),
            Some(&fc),
        )
        .await;
    assert_eq!(res.status, 200, "{}", res.text());
    let body: Value = res.json();
    assert_eq!(body["role_assignments"], Value::Null);
    assert_eq!(body["wings"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn update_motd_assigns_a_role_and_drops_pilots_who_left() {
    let Some(env) = Env::new().await else { return };
    let fc = env.pilot(FC, Some("trainee")).await;
    env.pilot(2001, None).await;
    env.pilot(2002, None).await;
    register(&env).await;
    mock_current_fleet(&env, FC, Some(FLEET)).await;
    let vindicator = env.type_id("Vindicator");
    mock_members(
        &env,
        FC,
        json!([
            member(FC, vindicator, "fleet_commander", -1, -1),
            member(2001, vindicator, "squad_member", 1, 12),
            member(2002, env.type_id("Kronos"), "squad_member", 1, 12),
        ]),
    )
    .await;
    mock_motd(
        &env,
        &format!(
            "Fleet MOTD<br>DDD: {}, {}<br>LR: TBD<br>",
            link(2001, "Pilot 2001"),
            link(3000, "Left Fleet")
        ),
    )
    .await;

    // Adds the Kronos pilot as DDD (wrong hull), drops the pilot who left.
    let expected = format!(
        "Fleet MOTD<br>DDD: {}, {}<br>LR: TBD<br>",
        link(2001, "Pilot 2001"),
        link(2002, "Pilot 2002")
    );
    let put = Mock::given(method("PUT"))
        .and(path(format!("/fleets/{FLEET}")))
        .and(header("authorization", bearer(FC).as_str()))
        .and(body_string_contains(
            serde_json::to_string(&expected).unwrap().trim_matches('"'),
        ))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount_as_scoped(&env.esi)
        .await;
    let res = env
        .post_json(
            "/api/fleet/update-motd",
            Some(&fc),
            &json!({"character_id": FC, "role": "ddd", "character_name": "Pilot 2002"}),
        )
        .await;
    assert_eq!(res.status, 200, "{}", res.text());
    assert_eq!(
        res.json::<Value>(),
        json!({"success": true, "message": "MOTD updated successfully", "is_duplicate": false, "has_incorrect_hull": true})
    );
    drop(put);

    // Re-adding a listed pilot is a duplicate on the right hull.
    Mock::given(method("PUT"))
        .and(path(format!("/fleets/{FLEET}")))
        .respond_with(ResponseTemplate::new(204))
        .mount(&env.esi)
        .await;
    let res = env
        .post_json(
            "/api/fleet/update-motd",
            Some(&fc),
            &json!({"character_id": FC, "role": "DDD", "character_name": "pilot 2001"}),
        )
        .await;
    assert_eq!(
        res.json::<Value>(),
        json!({"success": true, "message": "MOTD updated successfully", "is_duplicate": true, "has_incorrect_hull": false})
    );

    // Needs fleet-configure and a registered fleet.
    let user = env.pilot(1002, None).await;
    let res = env
        .post_json(
            "/api/fleet/update-motd",
            Some(&user),
            &json!({"character_id": 1002, "role": "DDD", "character_name": "x"}),
        )
        .await;
    assert_eq!((res.status.as_u16(), res.text()), (401, "access denied"));
    sqlx::query("DELETE FROM fleet_squad")
        .execute(env.pool())
        .await
        .unwrap();
    sqlx::query("DELETE FROM fleet")
        .execute(env.pool())
        .await
        .unwrap();
    let res = env
        .post_json(
            "/api/fleet/update-motd",
            Some(&fc),
            &json!({"character_id": FC, "role": "DDD", "character_name": "x"}),
        )
        .await;
    assert_eq!(
        (res.status.as_u16(), res.text()),
        (404, "Fleet not configured")
    );
}
