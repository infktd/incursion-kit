//! Pilot info, skills, implants, open-window, module info, fittings and skill plans.

mod common;
mod support;

use axum::http::{Method, StatusCode};
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use support::Env;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, ResponseTemplate};

#[tokio::test]
async fn open_window_uses_the_characters_token() {
    let Some(env) = Env::new().await else { return };
    let me = env.pilot(1001, None).await;
    env.pilot(1002, None).await;
    Mock::given(method("POST"))
        .and(path("/ui/openwindow/information"))
        .and(query_param("target_id", "1002"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&env.esi)
        .await;

    let res = env
        .post_json(
            "/api/open_window",
            Some(&me),
            &json!({"character_id": 1001, "target_id": 1002}),
        )
        .await;
    assert_eq!((res.status, res.text()), (StatusCode::OK, ""));
    let res = env
        .post_json(
            "/api/open_window",
            Some(&me),
            &json!({"character_id": 1002, "target_id": 1}),
        )
        .await;
    assert_eq!(
        (res.status, res.text()),
        (StatusCode::UNAUTHORIZED, "access denied")
    );
    env.grant_tokens(1001, &["publicData"]).await;
    let res = env
        .post_json(
            "/api/open_window",
            Some(&me),
            &json!({"character_id": 1001, "target_id": 1002}),
        )
        .await;
    assert_eq!(
        (res.status, res.text()),
        (StatusCode::UNAUTHORIZED, "ESI error: missing ESI scope")
    );
}

#[tokio::test]
async fn pilot_info_and_alts() {
    let Some(env) = Env::new().await else { return };
    let me = env.pilot(1001, Some("fc")).await;
    env.alt(1001, 1002).await;
    let user = env.pilot(2001, None).await;
    let trainee = env.pilot(2002, Some("trainee")).await;
    sqlx::query("INSERT INTO badge_assignment (characterId, badgeId, grantedAt) SELECT 1001, id, 0 FROM badge WHERE name = 'LOGI'")
        .execute(env.pool())
        .await
        .unwrap();

    let res = env
        .get("/api/pilot/info?character_id=1001", Some(&me))
        .await;
    assert_eq!(
        res.json::<Value>(),
        json!({"id": 1001, "name": "Pilot 1001", "tags": ["HQ-FC", "LOGI"], "active_bans": null})
    );
    let res = env
        .get("/api/pilot/info?character_id=1001", Some(&user))
        .await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED);
    let res = env
        .get("/api/pilot/info?character_id=2002", Some(&trainee))
        .await;
    assert_eq!(res.json::<Value>()["tags"], json!(["TRAINEE"]));
    let res = env
        .get("/api/pilot/info?character_id=1001", Some(&trainee))
        .await;
    assert_eq!(res.status, StatusCode::OK);

    let res = env
        .get("/api/pilot/alts?character_id=1002", Some(&me))
        .await;
    assert_eq!(
        res.json::<Value>(),
        json!([{"id": 1001, "name": "Pilot 1001", "corporation_id": support::CORPORATION_ID}])
    );
    let res = env
        .get("/api/pilot/alts?character_id=1001", Some(&trainee))
        .await;
    assert_eq!(
        (res.status, res.text()),
        (StatusCode::UNAUTHORIZED, "access denied")
    );
}

#[tokio::test]
async fn skills_are_loaded_and_recorded() {
    let Some(env) = Env::new().await else { return };
    let me = env.pilot(1001, None).await;
    let other = env.pilot(1002, None).await;
    let reviewer = env.pilot(1003, Some("trainee-advanced")).await;

    let res = env.get("/api/skills?character_id=1001", Some(&me)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let body: Value = res.json();
    let data = env.store.get();
    let current = body["current"].as_object().unwrap();
    assert_eq!(current.len(), data.skills.relevant_skills.len());
    assert!(current.values().all(|v| v.as_i64().unwrap() <= 5));
    assert_eq!(
        body["ids"].as_object().unwrap().len(),
        data.skills.name_lookup.len()
    );
    assert!(body["categories"].is_object() && body["requirements"].is_object());
    let stored: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM skill_current WHERE character_id = 1001")
            .fetch_one(env.pool())
            .await
            .unwrap();
    assert_eq!(stored as usize, env.all_doctrine_skills().len());

    let res = env.get("/api/skills?character_id=1001", Some(&other)).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED);
    let res = env
        .get("/api/skills?character_id=1001", Some(&reviewer))
        .await;
    assert_eq!(res.status, StatusCode::OK);
}

#[tokio::test]
async fn implants_are_own_characters_only() {
    let Some(env) = Env::new().await else { return };
    let me = env.pilot(1001, Some("fc")).await;
    env.pilot(1002, None).await;
    env.esi.reset().await;
    env.mock_implants(1001, &[13_231, 10_228]).await;

    let res = env.get("/api/implants?character_id=1001", Some(&me)).await;
    assert_eq!(res.json::<Value>(), json!({"implants": [13231, 10228]}));
    let res = env.get("/api/implants?character_id=1002", Some(&me)).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED);
    sqlx::query("DELETE FROM access_token WHERE character_id = 1001")
        .execute(env.pool())
        .await
        .unwrap();
    sqlx::query("DELETE FROM refresh_token WHERE character_id = 1001")
        .execute(env.pool())
        .await
        .unwrap();
    let res = env.get("/api/implants?character_id=1001", Some(&me)).await;
    assert_eq!(
        (res.status, res.text()),
        (StatusCode::UNAUTHORIZED, "ESI error: no ESI token found")
    );
}

#[tokio::test]
async fn module_info_and_preload() {
    let Some(env) = Env::new().await else { return };
    let me = env.pilot(1001, None).await;
    let paladin = env.type_id("Paladin");

    let res = env
        .get(&format!("/api/module/info?ids={paladin},1"), Some(&me))
        .await;
    assert_eq!(
        res.json::<Value>(),
        json!({paladin.to_string(): {"name": "Paladin", "category": "ship", "slot": null}})
    );
    let res = env
        .get(&format!("/api/module/info?ids={paladin}"), None)
        .await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED);
    for bad in ["", "1,x", "1,,2"] {
        let res = env
            .get(&format!("/api/module/info?ids={bad}"), Some(&me))
            .await;
        assert_eq!(
            (res.status, res.text()),
            (StatusCode::BAD_REQUEST, "Invalid type ID given"),
            "{bad}"
        );
    }
    let many = (1..=201)
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let res = env
        .get(&format!("/api/module/info?ids={many}"), Some(&me))
        .await;
    assert_eq!(
        (res.status, res.text()),
        (StatusCode::BAD_REQUEST, "Too many IDs")
    );

    let res = env.get("/api/module/preload", None).await;
    let preload: Value = res.json();
    assert!(preload[paladin.to_string()]["name"] == "Paladin");
    assert_eq!(
        preload.as_object().unwrap().len(),
        env.store.get().module_preload.len()
    );
}

#[tokio::test]
async fn fittings_are_public() {
    let Some(env) = Env::new().await else { return };
    let res = env.get("/api/fittings", None).await;
    assert_eq!(res.status, StatusCode::OK);
    let body: Value = res.json();
    let fits = body["fittingdata"].as_array().unwrap();
    assert!(!fits.is_empty());
    assert!(
        fits.iter()
            .all(|f| !f["name"].as_str().unwrap().contains("ALTERNATIVE"))
    );
    assert!(body["notes"].is_array() && body["implants"].is_array());
    assert!(
        body["rules"]
            .as_array()
            .unwrap()
            .contains(&json!(env.type_id("Nestor")))
    );
}

fn plan(name: &str) -> Value {
    json!({"name": name, "description": "d", "plan": [{"type": "skill", "from": "Spaceship Command", "level": 3}]})
}

#[tokio::test]
async fn skill_plans_and_admin_crud() {
    let Some(env) = Env::new().await else { return };
    let user = env.pilot(1001, None).await;
    let admin = env.pilot(1002, Some("admin")).await;
    let fc = env.pilot(1003, Some("fc+")).await;

    let res = env.get("/api/skills/plans", Some(&user)).await;
    assert_eq!(res.status, StatusCode::OK);
    let initial = res.json::<Value>()["plans"].as_array().unwrap().len();
    assert_eq!(initial, env.store.get().skill_plans.response.plans.len());
    assert_eq!(
        env.get("/api/skills/plans", None).await.status,
        StatusCode::UNAUTHORIZED
    );

    let res = env.get("/api/admin/skillplans", Some(&fc)).await;
    assert_eq!(
        (res.status, res.text()),
        (StatusCode::UNAUTHORIZED, "access denied")
    );
    let res = env.get("/api/admin/skillplans", Some(&admin)).await;
    let count = res.json::<Value>().as_array().unwrap().len();

    let res = env
        .post_json(
            "/api/admin/skillplans",
            Some(&admin),
            &json!({"plan": plan("New Plan")}),
        )
        .await;
    assert_eq!(
        (res.status, res.text()),
        (StatusCode::OK, "Skill plan created successfully")
    );
    let res = env
        .post_json(
            "/api/admin/skillplans",
            Some(&admin),
            &json!({"plan": plan("New Plan")}),
        )
        .await;
    assert_eq!(
        (res.status, res.text()),
        (
            StatusCode::BAD_REQUEST,
            "Skill plan with name 'New Plan' already exists"
        )
    );
    let res = env
        .get("/api/admin/skillplans/New%20Plan", Some(&admin))
        .await;
    assert_eq!(res.json::<Value>(), plan("New Plan"));
    assert_eq!(
        env.get("/api/admin/skillplans", Some(&admin))
            .await
            .json::<Value>()
            .as_array()
            .unwrap()
            .len(),
        count + 1
    );
    // The public list is rebuilt from the new file.
    let plans = env
        .get("/api/skills/plans", Some(&user))
        .await
        .json::<Value>();
    assert!(
        plans["plans"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["source"]["name"] == "New Plan")
    );

    let mut bad = plan("Broken");
    bad["plan"][0]["from"] = json!("Not A Skill");
    let res = env
        .post_json("/api/admin/skillplans", Some(&admin), &json!({"plan": bad}))
        .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    assert!(
        res.text().starts_with("skill plan error: "),
        "{}",
        res.text()
    );

    let res = env
        .send_json(
            Method::PUT,
            "/api/admin/skillplans/New%20Plan",
            Some(&admin),
            &json!({"plan": plan("Renamed")}),
        )
        .await;
    assert_eq!(
        (res.status, res.text()),
        (StatusCode::OK, "Skill plan updated successfully")
    );
    let res = env
        .send_json(
            Method::PUT,
            "/api/admin/skillplans/New%20Plan",
            Some(&admin),
            &json!({"plan": plan("X")}),
        )
        .await;
    assert_eq!(
        (res.status, res.text()),
        (StatusCode::NOT_FOUND, "Skill plan not found")
    );
    assert_eq!(
        env.get("/api/admin/skillplans/New%20Plan", Some(&admin))
            .await
            .status,
        StatusCode::NOT_FOUND
    );

    let res = env
        .delete("/api/admin/skillplans/Renamed", Some(&admin))
        .await;
    assert_eq!(
        (res.status, res.text()),
        (StatusCode::OK, "Skill plan deleted successfully")
    );
    let res = env
        .delete("/api/admin/skillplans/Renamed", Some(&admin))
        .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    // Writes keep backups.
    let backups = std::fs::read_dir(env.data_dir.path())
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("skillplan.yaml.backup.")
        })
        .count();
    assert!(backups >= 1);
}

#[tokio::test]
async fn raw_skill_plan_file() {
    let Some(env) = Env::new().await else { return };
    let admin = env.pilot(1002, Some("admin")).await;
    let user = env.pilot(1001, None).await;

    let res = env.get("/api/admin/skillplans/raw", Some(&admin)).await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(
        res.header("content-type"),
        Some("text/plain; charset=utf-8")
    );
    let original = std::fs::read_to_string(env.data_dir.path().join("skillplan.yaml")).unwrap();
    assert_eq!(res.text(), original);
    assert_eq!(
        env.get("/api/admin/skillplans/raw", Some(&user))
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );

    let res = env
        .send_raw(
            Method::POST,
            "/api/admin/skillplans/raw",
            Some(&admin),
            "plans: [",
        )
        .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    assert!(
        res.text()
            .starts_with("skill plan error: file write error: Invalid YAML"),
        "{}",
        res.text()
    );

    let yaml = "plans:\n  - name: Only\n    description: d\n    plan:\n      - type: skill\n        from: Spaceship Command\n        level: 2\n";
    let res = env
        .send_raw(
            Method::POST,
            "/api/admin/skillplans/raw",
            Some(&admin),
            yaml,
        )
        .await;
    assert_eq!(
        (res.status, res.text()),
        (StatusCode::OK, "Skill plans saved successfully")
    );
    assert_eq!(
        std::fs::read_to_string(env.data_dir.path().join("skillplan.yaml")).unwrap(),
        yaml
    );
    let plans = env
        .get("/api/admin/skillplans", Some(&admin))
        .await
        .json::<Value>();
    assert_eq!(plans.as_array().unwrap().len(), 1);
}
