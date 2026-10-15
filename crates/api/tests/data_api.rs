//! The structured data editors (`/api/data/*`): permissions, structured validation errors,
//! round trips through the files, compatibility of the written files and live events.

mod common;
mod support;

use axum::http::Method;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use support::{Env, next_event, no_event};
use wl_fitting::{DataFile, DataStore};
use wl_live::topics;

use common::{Login, TestResponse};

/// An account per role: user, fc, fc+ and admin.
struct Accounts {
    user: Login,
    fc: Login,
    fc_plus: Login,
    admin: Login,
}

async fn accounts(env: &Env) -> Accounts {
    Accounts {
        user: env.create_account(1001, "User", None).await,
        fc: env.create_account(1002, "Fc", Some("fc")).await,
        fc_plus: env.create_account(1003, "FcPlus", Some("fc+")).await,
        admin: env.create_account(1004, "Admin", Some("admin")).await,
    }
}

fn read(env: &Env, file: &str) -> String {
    std::fs::read_to_string(env.data_dir.path().join(file)).unwrap()
}

/// The files on disk load with a fresh store: the loader the server starts with.
fn reopen(env: &Env) -> DataStore {
    DataStore::open(env.eve.clone(), env.data_dir.path()).expect("written files load")
}

fn report(res: &TestResponse) -> Value {
    assert_eq!(
        res.header("content-type"),
        Some("application/json"),
        "{}",
        res.text()
    );
    res.json()
}

fn paths(res: &TestResponse) -> Vec<String> {
    assert_eq!(res.status, 400, "{}", res.text());
    let body = report(res);
    assert_eq!(body["valid"], false);
    body["errors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["path"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn permissions_follow_the_access_map() {
    let Some(env) = Env::new().await else { return };
    let a = accounts(&env).await;
    let fits_reads = [
        "/api/data/fits",
        "/api/data/fits/ARMOR_VINDI",
        "/api/data/fit-notes",
        "/api/data/implant-sets",
        "/api/data/types?q=vindi",
        "/api/data/options",
    ];
    let admin_reads = ["/api/data/categories", "/api/data/tags", "/api/data/rules"];
    for path in fits_reads.iter().chain(&admin_reads) {
        assert_eq!(env.get(path, None).await.status, 401, "{path}");
        for login in [&a.user, &a.fc] {
            let res = env.get(path, Some(login)).await;
            assert_eq!(
                (res.status.as_u16(), res.text()),
                (401, "access denied"),
                "{path}"
            );
        }
        assert_eq!(env.get(path, Some(&a.admin)).await.status, 200, "{path}");
    }
    for path in fits_reads {
        assert_eq!(env.get(path, Some(&a.fc_plus)).await.status, 200, "{path}");
    }
    for path in admin_reads {
        assert_eq!(env.get(path, Some(&a.fc_plus)).await.status, 401, "{path}");
    }
    let rules: Value = env.get("/api/data/rules", Some(&a.admin)).await.json();
    let plan = json!({ "plan": { "name": "x", "description": "", "plan": [] } });
    let writes: [(Method, &str, &Login, Value); 5] = [
        (
            Method::POST,
            "/api/data/fits?dry_run=true",
            &a.fc,
            json!({ "name": "X" }),
        ),
        (
            Method::DELETE,
            "/api/data/implant-sets/Implants",
            &a.fc,
            json!(null),
        ),
        (
            Method::PUT,
            "/api/data/tags?dry_run=true",
            &a.fc_plus,
            json!({ "public_tags": [] }),
        ),
        (
            Method::PUT,
            "/api/data/rules?dry_run=true",
            &a.fc_plus,
            rules["rules"].clone(),
        ),
        (
            Method::POST,
            "/api/data/skill-plans/validate",
            &a.fc_plus,
            plan,
        ),
    ];
    for (method, path, login, body) in writes {
        let res = env.send_json(method, path, Some(login), &body).await;
        assert_eq!(
            (res.status.as_u16(), res.text()),
            (401, "access denied"),
            "{path}"
        );
    }
    // The raw data-file editor stays admin-only.
    let res = env
        .get("/api/admin/data-files/fits.dat", Some(&a.fc_plus))
        .await;
    assert_eq!(res.status, 401);
    let res = env.get("/api/auth/whoami", Some(&a.fc_plus)).await;
    let access: Vec<String> =
        serde_json::from_value(res.json::<Value>()["access"].clone()).unwrap();
    assert!(access.contains(&"fits-manage".to_string()));
}

#[tokio::test]
async fn doctrine_fits_round_trip() {
    let Some(env) = Env::new().await else { return };
    let a = accounts(&env).await;
    let me = Some(&a.fc_plus);

    let list: Value = env.get("/api/data/fits", me).await.json();
    let fits = list["fits"].as_array().unwrap();
    assert_eq!(fits.len(), 15);
    let vindi = fits
        .iter()
        .find(|f| f["name"] == "ARMOR_VINDI")
        .unwrap()
        .clone();
    assert_eq!(vindi["hull"]["name"], "Vindicator");
    assert_eq!(vindi["section"], "OTHER");
    assert_eq!(vindi["category"], "other");
    assert_eq!(vindi["implant_set"], Value::Null);
    assert!(!vindi["layout"]["high"].as_array().unwrap().is_empty());
    let one: Value = env.get("/api/data/fits/ARMOR_VINDI", me).await.json();
    assert_eq!(one, vindi);
    let missing = env.get("/api/data/fits/NOPE", me).await;
    assert_eq!(
        (missing.status.as_u16(), missing.text()),
        (404, "Fit not found")
    );

    // Parse first, as an editor would.
    let eft = vindi["eft"]
        .as_str()
        .unwrap()
        .replace("ARMOR_VINDI", "NEW_VINDI");
    let parsed = env
        .post_json("/api/data/fits/parse", me, &json!({ "eft": eft }))
        .await;
    assert_eq!(parsed.status, 200);
    let parsed: Value = parsed.json();
    assert_eq!(parsed["valid"], true);
    assert_eq!(parsed["fit"]["name"], "NEW_VINDI");
    assert_eq!(parsed["fit"]["dna"], vindi["dna"]);
    assert_eq!(parsed["fit"]["same_as"], json!(["ARMOR_VINDI"]));
    assert_eq!(parsed["fit"]["layout"], vindi["layout"]);

    // A dry run writes nothing.
    let before = read(&env, "fits.dat");
    let input = json!({ "name": "NEW_VINDI", "eft": eft, "section": "OTHER" });
    let dry = env
        .post_json("/api/data/fits?dry_run=true", me, &input)
        .await;
    assert_eq!(dry.status, 200, "{}", dry.text());
    assert_eq!(
        report(&dry),
        json!({ "valid": true, "dry_run": true, "errors": [], "warnings": [], "warning_details": [] })
    );
    assert_eq!(read(&env, "fits.dat"), before);

    let created = env.post_json("/api/data/fits", me, &input).await;
    assert_eq!(created.status, 200, "{}", created.text());
    assert_eq!(report(&created)["dry_run"], false);
    let after = read(&env, "fits.dat");
    assert!(after.contains(&format!(
        r#"<a href="fitting:{}">NEW_VINDI</a>"#,
        vindi["dna"].as_str().unwrap()
    )));
    // Read back equals the input.
    let new: Value = env.get("/api/data/fits/NEW_VINDI", me).await.json();
    assert_eq!(
        (&new["dna"], &new["section"], &new["layout"]),
        (&vindi["dna"], &json!("OTHER"), &vindi["layout"])
    );
    assert_eq!(new["eft"].as_str().unwrap(), eft);
    // The file loads in the existing loader, and the public list shows the fit.
    assert!(
        reopen(&env)
            .get()
            .fits
            .iter()
            .any(|fit| fit.name == "NEW_VINDI")
    );
    let public: Value = env.get("/api/fittings", None).await.json();
    assert!(
        public["fittingdata"]
            .as_array()
            .unwrap()
            .iter()
            .any(|fit| fit["name"] == "NEW_VINDI")
    );

    // Update: rename, change the fit by DNA and link it to an implant set.
    let paladin = fits.iter().find(|f| f["name"] == "ARMOR_PALADIN").unwrap();
    let res = env
        .send_json(
            Method::PUT,
            "/api/data/fits/NEW_VINDI",
            me,
            &json!({ "name": "NEW_PALADIN", "dna": paladin["dna"], "implant_set": "Implants" }),
        )
        .await;
    assert_eq!(res.status, 200, "{}", res.text());
    let renamed: Value = env.get("/api/data/fits/NEW_PALADIN", me).await.json();
    assert_eq!(renamed["dna"], paladin["dna"]);
    assert_eq!(renamed["section"], "OTHER", "stays where it was");
    assert_eq!(renamed["implant_set"], "Implants");
    assert_eq!(env.get("/api/data/fits/NEW_VINDI", me).await.status, 404);
    let set: Value = env.get("/api/data/implant-sets/Implants", me).await.json();
    assert_eq!(set["fits"], json!(["NEW_PALADIN"]));
    let public: Value = env.get("/api/fittings", None).await.json();
    let public_fit = |name: &str| {
        public["fittingdata"]
            .as_array()
            .unwrap()
            .iter()
            .find(|fit| fit["name"] == name)
            .cloned()
            .unwrap()
    };
    assert_eq!(public_fit("NEW_PALADIN")["implant_set"], "Implants");
    // Fits without a set have no `implant_set` key, as before.
    assert_eq!(
        public_fit("ARMOR_VINDI"),
        json!({ "name": "ARMOR_VINDI", "dna": vindi["dna"] })
    );

    // Saving the set without `fits` (an older client, the YAML round trip) keeps its fits;
    // `"fits": []` takes them out.
    let slots_only = json!({ "name": set["name"], "slots": set["slots"] });
    let res = env
        .send_json(
            Method::PUT,
            "/api/data/implant-sets/Implants",
            me,
            &slots_only,
        )
        .await;
    assert_eq!(res.status, 200, "{}", res.text());
    let kept: Value = env.get("/api/data/implant-sets/Implants", me).await.json();
    assert_eq!(kept["fits"], json!(["NEW_PALADIN"]));
    let fit: Value = env.get("/api/data/fits/NEW_PALADIN", me).await.json();
    assert_eq!(fit["implant_set"], "Implants");
    let mut emptied = slots_only.clone();
    emptied["fits"] = json!([]);
    let res = env
        .send_json(Method::PUT, "/api/data/implant-sets/Implants", me, &emptied)
        .await;
    assert_eq!(res.status, 200, "{}", res.text());
    let fit: Value = env.get("/api/data/fits/NEW_PALADIN", me).await.json();
    assert_eq!(fit["implant_set"], Value::Null);
    let res = env
        .send_json(Method::PUT, "/api/data/implant-sets/Implants", me, &set)
        .await;
    assert_eq!(res.status, 200, "{}", res.text());

    // Delete: the file is as before, and the fit leaves its set.
    let res = env.delete("/api/data/fits/NEW_PALADIN", me).await;
    assert_eq!(res.status, 200, "{}", res.text());
    assert_eq!(read(&env, "fits.dat"), before);
    let set: Value = env.get("/api/data/implant-sets/Implants", me).await.json();
    assert_eq!(set.get("fits"), None);
    assert_eq!(
        env.delete("/api/data/fits/NEW_PALADIN", me).await.status,
        404
    );
    // Backups are kept.
    assert!(std::fs::read_dir(env.data_dir.path()).unwrap().any(|e| {
        e.unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("fits.dat.backup.")
    }));
}

#[tokio::test]
async fn fit_errors_are_structured() {
    let Some(env) = Env::new().await else { return };
    let a = accounts(&env).await;
    let me = Some(&a.fc_plus);
    let before = read(&env, "fits.dat");

    let res = env
        .post_json(
            "/api/data/fits",
            me,
            &json!({ "name": "ARMOR_VINDI", "eft": "[Vindicator, x]\nNo Such Module\n" }),
        )
        .await;
    assert_eq!(paths(&res), ["eft", "name"]);
    let body = report(&res);
    assert_eq!(body["errors"][0]["line"], 2);
    assert_eq!(
        body["errors"][0]["message"],
        "unknown type \"No Such Module\""
    );
    assert_eq!(
        body["errors"][1]["message"],
        "a fit named \"ARMOR_VINDI\" already exists"
    );
    assert_eq!(body["dry_run"], false);

    let res = env
        .post_json("/api/data/fits?dry_run=true", me, &json!({ "name": "" }))
        .await;
    assert_eq!(paths(&res), ["name", "eft"]);
    assert_eq!(report(&res)["dry_run"], true);

    let res = env
        .post_json(
            "/api/data/fits",
            me,
            &json!({ "name": "X", "dna": "1:2:3" }),
        )
        .await;
    assert_eq!(paths(&res), ["dna"]);

    let parsed: Value = env
        .post_json(
            "/api/data/fits/parse",
            me,
            &json!({ "eft": "hello\n[Rifter, a]\n" }),
        )
        .await
        .json();
    assert_eq!(parsed["valid"], false);
    assert_eq!(parsed["fit"], Value::Null);
    assert_eq!(parsed["errors"][0]["line"], 1);

    // Warnings come as text and structured, like errors.
    let parsed: Value = env
        .post_json(
            "/api/data/fits/parse",
            me,
            &json!({ "eft": "[Vindicator, a]\nNeutron Blaster Cannon II, Void L\n" }),
        )
        .await
        .json();
    assert_eq!(parsed["valid"], true);
    let message = "the loaded charge \"Void L\" is ignored; put charges in the cargo";
    assert_eq!(parsed["warnings"], json!([format!("line 2: {message}")]));
    assert_eq!(
        parsed["warning_details"],
        json!([{ "path": "eft", "line": 2, "message": message }])
    );
    let res = env
        .post_json(
            "/api/data/fits?dry_run=true",
            me,
            &json!({ "name": "W", "eft": "[Vindicator, a]\nNeutron Blaster Cannon II, Void L\n" }),
        )
        .await;
    assert_eq!(res.status, 200, "{}", res.text());
    assert_eq!(
        report(&res)["warning_details"],
        json!([{ "path": "eft", "line": 2, "message": message }])
    );

    // An unknown implant set is an error with the others.
    let res = env
        .post_json(
            "/api/data/fits",
            me,
            &json!({ "name": "", "dna": "1:2:3", "implant_set": "Nope" }),
        )
        .await;
    assert_eq!(paths(&res), ["name", "dna", "implant_set"]);

    // Bad JSON stays a plain-text 422.
    let res = env
        .post_json("/api/data/fits", me, &json!({ "eft": 1 }))
        .await;
    assert_eq!(res.status, 422);
    assert_eq!(read(&env, "fits.dat"), before);
}

#[tokio::test]
async fn notes_and_implant_sets_round_trip() {
    let Some(env) = Env::new().await else { return };
    let a = accounts(&env).await;
    let me = Some(&a.fc_plus);

    let notes: Value = env.get("/api/data/fit-notes", me).await.json();
    assert_eq!(notes["notes"].as_array().unwrap().len(), 12);
    let vindi: Value = env.get("/api/data/fit-notes/ARMOR_VINDI", me).await.json();
    assert_eq!(vindi["target"], "fit");

    let note = json!({ "name": "Logi", "description": "**Read this**\n\n- one\n- two\n" });
    let res = env.post_json("/api/data/fit-notes", me, &note).await;
    assert_eq!(res.status, 200, "{}", res.text());
    let back: Value = env.get("/api/data/fit-notes/Logi", me).await.json();
    assert_eq!(
        (&back["name"], &back["description"], &back["target"]),
        (&note["name"], &note["description"], &json!("tier"))
    );
    assert!(read(&env, "fitnotes.yaml").starts_with("# Notes shown next to doctrine fits"));
    assert_eq!(
        reopen(&env).get().fit_notes.last().unwrap().description,
        note["description"].as_str().unwrap()
    );
    let res = env
        .send_json(
            Method::PUT,
            "/api/data/fit-notes/Logi",
            me,
            &json!({ "name": "Shield", "description": "" }),
        )
        .await;
    assert_eq!(paths(&res), ["name"]);
    let res = env
        .send_json(Method::PUT, "/api/data/fit-notes/Nope", me, &note)
        .await;
    assert_eq!(
        (res.status.as_u16(), res.text()),
        (404, "Fit note not found")
    );
    assert_eq!(env.delete("/api/data/fit-notes/Logi", me).await.status, 200);
    assert_eq!(env.get("/api/data/fit-notes/Logi", me).await.status, 404);

    let sets: Value = env.get("/api/data/implant-sets", me).await.json();
    let original = sets["sets"][0].clone();
    assert_eq!(original["name"], "Implants");
    let set = json!({
        "name": "Budget",
        "slots": [
            { "slot": 1, "default": [{ "name": "Mid-grade Ascendancy Alpha", "note": "speed" }], "alternative": [] },
            { "slot": 7, "default": [{ "name": "MR-706", "group": "DPS" }], "alternative": [] },
        ],
    });
    let res = env.post_json("/api/data/implant-sets", me, &set).await;
    assert_eq!(res.status, 200, "{}", res.text());
    let back: Value = env.get("/api/data/implant-sets/Budget", me).await.json();
    assert_eq!(back, set);
    let public: Value = env.get("/api/fittings", None).await.json();
    assert_eq!(public["implants"][1], set);
    assert_eq!(reopen(&env).get().implants.len(), 2);
    // Updating the example set with its own data keeps it unchanged.
    let res = env
        .send_json(
            Method::PUT,
            "/api/data/implant-sets/Implants",
            me,
            &original,
        )
        .await;
    assert_eq!(res.status, 200, "{}", res.text());
    let back: Value = env.get("/api/data/implant-sets/Implants", me).await.json();
    assert_eq!(back, original);

    let bad = json!({
        "name": "Budget",
        "slots": [{ "slot": 0, "default": [{ "name": "" }], "alternative": [] }],
    });
    let res = env.post_json("/api/data/implant-sets", me, &bad).await;
    assert_eq!(
        paths(&res),
        ["name", "slots[0].slot", "slots[0].default[0].name"]
    );
    assert_eq!(
        env.delete("/api/data/implant-sets/Budget", me).await.status,
        200
    );
    assert_eq!(
        env.delete("/api/data/implant-sets/Budget", me).await.status,
        404
    );
}

#[tokio::test]
async fn categories_tags_and_rules_round_trip() {
    let Some(env) = Env::new().await else { return };
    let a = accounts(&env).await;
    let me = Some(&a.admin);

    // Writing back what was read keeps the data (comments aside).
    for (path, file) in [
        ("/api/data/categories", "categories.yaml"),
        ("/api/data/tags", "tags.yaml"),
    ] {
        let document: Value = env.get(path, me).await.json();
        let res = env.send_json(Method::PUT, path, me, &document).await;
        assert_eq!(res.status, 200, "{path}: {}", res.text());
        let again: Value = env.get(path, me).await.json();
        assert_eq!(again, document, "{path}");
        assert!(read(&env, file).starts_with('#'), "{file} keeps its header");
    }
    let categories_before = env.store.get().categories.rules.clone();
    assert_eq!(reopen(&env).get().categories.rules, categories_before);

    let mut categories: Value = env.get("/api/data/categories", me).await.json();
    categories["categories"]
        .as_array_mut()
        .unwrap()
        .push(json!({ "id": "sniper", "name": "Sniper" }));
    categories["rules"]
        .as_array_mut()
        .unwrap()
        .insert(0, json!({ "item": "Machariel", "category": "sniper" }));
    let res = env
        .send_json(Method::PUT, "/api/data/categories", me, &categories)
        .await;
    assert_eq!(res.status, 200, "{}", res.text());
    assert_eq!(
        env.get("/api/data/categories", me).await.json::<Value>(),
        categories
    );
    let public: Value = env.get("/api/categories", me).await.json();
    assert!(public.to_string().contains("Sniper"), "{public}");

    categories["rules"][0]["item"] = json!("Nope");
    categories["rules"][1]["category"] = json!("missing");
    let res = env
        .send_json(
            Method::PUT,
            "/api/data/categories?dry_run=true",
            me,
            &categories,
        )
        .await;
    assert_eq!(paths(&res), ["rules[0].item", "rules[1].category"]);

    // Removing a category rules.yaml names is a warning, not an error.
    let res = env
        .send_json(
            Method::PUT,
            "/api/data/categories?dry_run=true",
            me,
            &json!({ "categories": [{ "id": "x", "name": "X" }], "rules": [] }),
        )
        .await;
    assert_eq!(res.status, 200, "{}", res.text());
    assert!(
        report(&res)["warnings"][0]
            .as_str()
            .unwrap()
            .contains("categories not defined")
    );
    assert_eq!(report(&res)["warning_details"][0]["path"], "");
    assert_eq!(
        report(&res)["warning_details"][0]["message"],
        report(&res)["warnings"][0]
    );

    let res = env
        .send_json(
            Method::PUT,
            "/api/data/tags",
            me,
            &json!({ "public_tags": ["A", "A,B"] }),
        )
        .await;
    assert_eq!(paths(&res), ["public_tags[1]"]);

    // Rules: the built-in rules apply until saved; saving creates the file.
    let rules: Value = env.get("/api/data/rules", me).await.json();
    assert_eq!(rules["custom"], false);
    assert_eq!(
        rules["rules"]["fittings"]["hidden_fit_marker"],
        "ALTERNATIVE"
    );
    let mut document = rules["rules"].clone();
    document["war"]["tag"] = json!("WAR");
    let res = env
        .send_json(Method::PUT, "/api/data/rules", me, &document)
        .await;
    assert_eq!(res.status, 200, "{}", res.text());
    let saved: Value = env.get("/api/data/rules", me).await.json();
    assert_eq!(
        (&saved["custom"], &saved["rules"]),
        (&json!(true), &document)
    );
    assert_eq!(reopen(&env).get().rules.war.tag, "WAR");
    assert!(read(&env, "rules.yaml").starts_with("# Community rules"));

    document["fit_check"]["time_in_fleet_limits"][0]["hull"] = json!("Nope");
    document["skill_plans"]["tank_levels"][0]["skills"]["Nope Skill"] = json!(4);
    let res = env
        .send_json(Method::PUT, "/api/data/rules", me, &document)
        .await;
    assert_eq!(
        paths(&res),
        [
            "fit_check.time_in_fleet_limits[0].hull",
            "skill_plans.tank_levels[0].skills.Nope Skill",
        ]
    );
    // Unknown fields are rejected as bad JSON, as the loader would reject them.
    document["extra"] = json!(1);
    let res = env
        .send_json(Method::PUT, "/api/data/rules", me, &document)
        .await;
    assert_eq!(res.status, 422);
}

#[tokio::test]
async fn pickers_and_skill_plan_validation() {
    let Some(env) = Env::new().await else { return };
    let a = accounts(&env).await;

    let res: Value = env
        .get(
            "/api/data/types?q=vindicator&kind=ship&limit=3",
            Some(&a.fc_plus),
        )
        .await
        .json();
    assert_eq!(res["types"][0]["name"], "Vindicator");
    assert_eq!(res["types"][0]["kind"], "ship");
    let res: Value = env
        .get("/api/data/types?q=surgical&kind=skill", Some(&a.fc_plus))
        .await
        .json();
    assert_eq!(res["types"][0]["name"], "Surgical Strike");
    let res = env
        .get("/api/data/types?q=x&kind=boat", Some(&a.fc_plus))
        .await;
    assert_eq!(paths(&res), ["kind"]);

    let options: Value = env.get("/api/data/options", Some(&a.fc_plus)).await.json();
    let vindi = options["hulls"]
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h["name"] == "Vindicator")
        .unwrap();
    assert!(
        vindi["fits"]
            .as_array()
            .unwrap()
            .contains(&json!("ARMOR_VINDI"))
    );
    assert!(
        options["skill_sets"]
            .as_array()
            .unwrap()
            .contains(&json!({ "name": "Armor Kronos", "kind": "variant" }))
    );
    assert_eq!(options["skill_tiers"], json!(["min", "elite", "gold"]));
    assert_eq!(
        options["fit_sections"],
        json!(["LOGI", "MARAUDERS", "OTHER"])
    );
    assert_eq!(options["tank_levels"][0], "starter");

    let me = Some(&a.admin);
    let plan = json!({
        "name": "New plan",
        "description": "",
        "plan": [
            { "type": "fit", "hull": "Vindicator", "fit": "ARMOR_VINDI" },
            { "type": "skill", "from": "Gunnery", "level": 5 },
        ],
    });
    let res = env
        .post_json(
            "/api/data/skill-plans/validate",
            me,
            &json!({ "plan": plan }),
        )
        .await;
    assert_eq!(res.status, 200, "{}", res.text());
    let body: Value = res.json();
    assert_eq!(body["valid"], true);
    assert!(!body["levels"].as_array().unwrap().is_empty());
    assert_eq!(body["ships"][0]["name"], "Vindicator");

    let mut bad = plan.clone();
    bad["name"] = json!("Good Vindicator");
    bad["plan"][0]["fit"] = json!("NOPE");
    bad["plan"][1]["level"] = json!(7);
    let res = env
        .post_json(
            "/api/data/skill-plans/validate",
            me,
            &json!({ "plan": bad }),
        )
        .await;
    assert_eq!(paths(&res), ["plan[0].fit", "plan[1].level", "name"]);
    assert_eq!(
        report(&res)["errors"][0]["message"],
        "Fit 'NOPE' not found for hull 'Vindicator'"
    );
    // Updating a plan under its own name is fine.
    let mut same = plan.clone();
    same["name"] = json!("Good Vindicator");
    let res = env
        .post_json(
            "/api/data/skill-plans/validate",
            me,
            &json!({ "plan": same, "name": "Good Vindicator" }),
        )
        .await;
    assert_eq!(res.status, 200, "{}", res.text());
    let res = env
        .post_json(
            "/api/data/skill-plans/validate",
            me,
            &json!({ "plan": same, "name": "Nope" }),
        )
        .await;
    assert_eq!(res.status, 404);
}

#[tokio::test]
async fn data_changes_are_published() {
    let Some(env) = Env::new().await else { return };
    let a = accounts(&env).await;
    let mut sub = env.live().subscribe(vec![topics::DATA.to_owned()]);

    let note = json!({ "name": "Logi", "description": "x" });
    let res = env
        .post_json("/api/data/fit-notes?dry_run=true", Some(&a.fc_plus), &note)
        .await;
    assert_eq!(res.status, 200);
    no_event(&mut sub).await;

    env.post_json("/api/data/fit-notes", Some(&a.fc_plus), &note)
        .await;
    let event = next_event(&mut sub).await;
    assert_eq!(
        (event.name.as_str(), event.data.as_str()),
        ("data_update", r#"{"file":"fitnotes.yaml"}"#)
    );

    // Invalid writes publish nothing.
    env.post_json("/api/data/fit-notes", Some(&a.fc_plus), &note)
        .await;
    no_event(&mut sub).await;

    // The legacy editors publish too.
    let res = env
        .send_raw(
            Method::POST,
            "/api/admin/data-files/tags.yaml",
            Some(&a.admin),
            "public_tags: [A]\n",
        )
        .await;
    assert_eq!(res.status, 200, "{}", res.text());
    assert_eq!(next_event(&mut sub).await.data, r#"{"file":"tags.yaml"}"#);
    env.request(
        common::TestApp::build(
            Method::POST,
            "/api/admin/data-files/fits.dat/reload",
            Some(&a.admin),
        )
        .body(axum::body::Body::empty())
        .unwrap(),
    )
    .await;
    assert_eq!(next_event(&mut sub).await.data, r#"{"file":null}"#);

    // Skill plan administration too.
    let res = env
        .delete("/api/admin/skillplans/Good%20Vindicator", Some(&a.admin))
        .await;
    assert_eq!(res.status, 200, "{}", res.text());
    assert_eq!(
        next_event(&mut sub).await.data,
        format!(r#"{{"file":"{}"}}"#, DataFile::SkillPlans.file_name())
    );
}

#[tokio::test]
async fn the_data_topic_is_on_both_streams() {
    use futures::StreamExt;
    use tower::ServiceExt;
    let Some(env) = Env::new().await else { return };
    let a = accounts(&env).await;
    for (path, login) in [
        ("/api/sse/stream", Some(&a.user)),
        ("/api/public/stream", None),
    ] {
        let request = common::TestApp::build(Method::GET, path, login)
            .body(axum::body::Body::empty())
            .unwrap();
        let response = env.router.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), 200, "{path}");
        let mut body = response.into_body().into_data_stream();
        let mut next = async || {
            let chunk = tokio::time::timeout(std::time::Duration::from_secs(2), body.next())
                .await
                .expect("timed out")
                .expect("open")
                .unwrap();
            String::from_utf8(chunk.to_vec()).unwrap()
        };
        assert_eq!(next().await, ": connected\n\n");
        // A write through the API reaches the stream.
        let res = env
            .send_json(
                Method::PUT,
                "/api/data/tags",
                Some(&a.admin),
                &json!({ "public_tags": ["A"] }),
            )
            .await;
        assert_eq!(res.status, 200, "{}", res.text());
        assert_eq!(
            next().await,
            "event: data_update\ndata: {\"file\":\"tags.yaml\"}\n\n",
            "{path}"
        );
    }
}
