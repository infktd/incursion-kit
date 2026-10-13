//! The fit checker, diff and module variations against the example data set.

mod common;

use std::collections::{BTreeMap, BTreeSet};

use common::{eft, example_store, id};
use pretty_assertions::assert_eq;
use wl_eve_data::{EveData, TypeId};
use wl_fitting::{DataSet, FitCheckOutput, Fitting, PilotData, PilotSkills};

/// Skills meeting every requirement of the fit's hull and modules.
fn skills_for(eve: &EveData, fit: &Fitting) -> PilotSkills {
    let mut skills = std::collections::HashMap::new();
    for type_id in std::iter::once(fit.hull).chain(fit.modules.keys().copied()) {
        for (&skill, &level) in &eve.get(type_id).unwrap().skill_requirements {
            let entry = skills.entry(skill).or_insert(0);
            *entry = (*entry).max(level);
        }
    }
    PilotSkills(skills)
}

#[derive(Default)]
struct Pilot {
    implants: Vec<TypeId>,
    time_in_fleet: i64,
    access: BTreeSet<String>,
    badges: Vec<String>,
}

fn check(data: &DataSet, fit: &Fitting, pilot: &Pilot) -> FitCheckOutput {
    let skills = skills_for(data.eve(), fit);
    data.check_fit(
        &PilotData {
            implants: &pilot.implants,
            time_in_fleet: pilot.time_in_fleet,
            skills: &skills,
            access_keys: &pilot.access,
        },
        fit,
        &pilot.badges,
    )
    .unwrap()
}

/// The example doctrine fit whose name ends in `_<suffix>` (for example `ARMOR_PALADIN`).
fn doctrine(data: &DataSet, name: &str) -> Fitting {
    data.fits
        .iter()
        .find(|fit| fit.name == name)
        .unwrap_or_else(|| panic!("no doctrine fit named {name}"))
        .fit
        .clone()
}

fn hybrid_implants(eve: &EveData) -> Vec<TypeId> {
    ["Alpha", "Beta", "Delta", "Epsilon", "Gamma"]
        .iter()
        .map(|slot| id(eve, &format!("High-grade Amulet {slot}")))
        .collect()
}

#[test]
fn every_doctrine_fit_matches_itself() {
    let Some((_dir, store)) = example_store() else {
        return;
    };
    let data = store.get();
    for doctrine in data.fits.iter() {
        let (matched, diff) = data
            .find_fit(&doctrine.fit)
            .expect("a doctrine fit matches");
        assert_eq!(matched.name, doctrine.name);
        assert_eq!(diff, Default::default(), "{}", doctrine.name);
    }
}

#[test]
fn exact_match_is_approved() {
    let Some((_dir, store)) = example_store() else {
        return;
    };
    let data = store.get();
    let fit = doctrine(&data, "ARMOR_PALADIN");
    let output = check(&data, &fit, &Pilot::default());
    assert!(output.approved);
    assert_eq!(output.category, "bastion");
    assert_eq!(output.tags, Vec::<String>::new());
    assert_eq!(output.errors, Vec::<String>::new());
    let analysis = output.analysis.unwrap();
    assert_eq!(analysis.name, "ARMOR_PALADIN");
    assert!(analysis.missing.is_empty() && analysis.extra.is_empty());
}

#[test]
fn missing_module_blocks_approval() {
    let Some((_dir, store)) = example_store() else {
        return;
    };
    let data = store.get();
    let eve = data.eve();
    let mut fit = doctrine(&data, "ARMOR_PALADIN");
    let laser = id(eve, "Mega Pulse Laser II");
    fit.modules.insert(laser, 3);
    let output = check(&data, &fit, &Pilot::default());
    assert!(!output.approved);
    let analysis = output.analysis.unwrap();
    assert_eq!(analysis.missing, BTreeMap::from([(laser, 1)]));
}

#[test]
fn downgrade_blocks_approval_and_upgrade_does_not() {
    let Some((_dir, store)) = example_store() else {
        return;
    };
    let data = store.get();
    let eve = data.eve();
    let laser = id(eve, "Mega Pulse Laser II");

    let mut downgraded = doctrine(&data, "ARMOR_PALADIN");
    let tech_1 = id(eve, "Tachyon Modulated Energy Beam I");
    downgraded.modules.insert(laser, 3);
    downgraded.modules.insert(tech_1, 1);
    let output = check(&data, &downgraded, &Pilot::default());
    assert!(!output.approved);
    assert_eq!(
        output.analysis.unwrap().downgraded,
        BTreeMap::from([(laser, BTreeMap::from([(tech_1, 1)]))])
    );

    let mut upgraded = doctrine(&data, "ARMOR_PALADIN");
    let officer = id(eve, "Ahremen's Modified Mega Pulse Laser");
    upgraded.modules.insert(laser, 3);
    upgraded.modules.insert(officer, 1);
    let output = check(&data, &upgraded, &Pilot::default());
    assert!(output.approved);
    let analysis = output.analysis.unwrap();
    assert!(analysis.downgraded.is_empty() && analysis.missing.is_empty());
}

#[test]
fn extra_module_is_reported_but_approved() {
    let Some((_dir, store)) = example_store() else {
        return;
    };
    let data = store.get();
    let mut fit = doctrine(&data, "SHIELD_VARGUR");
    let booster = id(data.eve(), "Sensor Booster II");
    fit.modules.insert(booster, 1);
    let output = check(&data, &fit, &Pilot::default());
    assert!(output.approved);
    assert_eq!(
        output.analysis.unwrap().extra,
        BTreeMap::from([(booster, 1)])
    );
}

#[test]
fn cargo_tolerance_and_ignored_cargo() {
    let Some((_dir, store)) = example_store() else {
        return;
    };
    let data = store.get();
    let eve = data.eve();
    let fit = doctrine(&data, "ARMOR_PALADIN");
    let ammo = id(eve, "Conflagration L");
    assert_eq!(fit.cargo[&ammo], 40);

    // 32 of 40 missing is not more than 80%: still fine.
    let mut light = fit.clone();
    light.cargo.insert(ammo, 8);
    let output = check(&data, &light, &Pilot::default());
    assert!(output.approved, "{output:?}");

    // 33 of 40 missing is.
    light.cargo.insert(ammo, 7);
    let output = check(&data, &light, &Pilot::default());
    assert!(!output.approved);
    assert_eq!(
        output.analysis.unwrap().cargo_missing,
        BTreeMap::from([(ammo, 33)])
    );

    // cargo_ignore items are never required.
    let mut no_mjd = doctrine(&data, "ARMOR_VINDI");
    let mjd = id(eve, "Large Micro Jump Drive");
    assert!(no_mjd.cargo.remove(&mjd).is_some());
    assert!(check(&data, &no_mjd, &Pilot::default()).approved);
}

#[test]
fn missing_skills_are_errors() {
    let Some((_dir, store)) = example_store() else {
        return;
    };
    let data = store.get();
    let fit = doctrine(&data, "ARMOR_VINDI");
    let output = data
        .check_fit(
            &PilotData {
                implants: &[],
                time_in_fleet: 0,
                skills: &PilotSkills::default(),
                access_keys: &BTreeSet::new(),
            },
            &fit,
            &[],
        )
        .unwrap();
    assert!(
        output
            .errors
            .contains(&"Missing skills to online/use 'Vindicator'".to_string()),
        "{:?}",
        output.errors
    );
}

#[test]
fn role_and_badge_tags() {
    let Some((_dir, store)) = example_store() else {
        return;
    };
    let data = store.get();
    let fit = doctrine(&data, "ARMOR_NESTOR");
    let badges: Vec<String> = ["LOGI", "ALT", "DPS", "VINDI", "OTHER"]
        .map(String::from)
        .to_vec();

    let pilot = Pilot {
        badges: badges.clone(),
        ..Pilot::default()
    };
    assert_eq!(
        check(&data, &fit, &pilot).tags,
        ["ALT", "DPS", "LOGI", "VINDI"]
    );

    let hq = Pilot {
        access: BTreeSet::from(["waitlist-tag:HQ-FC".into(), "waitlist-tag:TRAINEE".into()]),
        badges: badges.clone(),
        ..Pilot::default()
    };
    assert_eq!(check(&data, &fit, &hq).tags, ["HQ-FC"]);

    let trainee = Pilot {
        access: BTreeSet::from(["waitlist-tag:TRAINEE".into()]),
        badges,
        ..Pilot::default()
    };
    assert_eq!(check(&data, &fit, &trainee).tags, ["TRAINEE"]);
}

#[test]
fn vindicator_badge_and_hours_rules() {
    let Some((_dir, store)) = example_store() else {
        return;
    };
    let data = store.get();
    let mut fit = doctrine(&data, "ARMOR_VINDI");
    // Way off the doctrine: auto-approved anyway with the badge, without any analysis.
    fit.modules.clear();
    let badge = Pilot {
        badges: vec!["VINDI".into()],
        ..Pilot::default()
    };
    let output = check(&data, &fit, &badge);
    assert!(output.approved);
    assert_eq!(output.analysis, None);
    assert_eq!(output.tags, ["VINDI"]);

    let fit = doctrine(&data, "ARMOR_VINDI");
    let veteran = Pilot {
        time_in_fleet: 20 * 3600 + 1,
        ..Pilot::default()
    };
    let output = check(&data, &fit, &veteran);
    assert!(!output.approved);
    assert_eq!(output.tags, ["DPS-HOURS-REACHED"]);

    let exactly = Pilot {
        time_in_fleet: 20 * 3600,
        ..Pilot::default()
    };
    assert!(check(&data, &fit, &exactly).approved);

    let dps = Pilot {
        time_in_fleet: 100 * 3600,
        badges: vec!["DPS".into()],
        ..Pilot::default()
    };
    let output = check(&data, &fit, &dps);
    assert!(output.approved);
    assert_eq!(output.tags, ["DPS"]);
}

#[test]
fn implant_and_rig_combinations() {
    let Some((_dir, store)) = example_store() else {
        return;
    };
    let data = store.get();
    let eve = data.eve();
    let hybrid = Pilot {
        implants: hybrid_implants(eve),
        ..Pilot::default()
    };

    // The Paladin doctrine fit uses Trimark pumps: fine without the set, flagged with it.
    let paladin = doctrine(&data, "ARMOR_PALADIN");
    assert!(
        paladin
            .modules
            .contains_key(&id(eve, "Large Trimark Armor Pump II"))
    );
    assert!(check(&data, &paladin, &Pilot::default()).approved);
    let output = check(&data, &paladin, &hybrid);
    assert!(!output.approved);
    assert_eq!(output.tags, ["HYBRID-TRIMARK"]);

    // Hyperspatial rigs need the set.
    let mut fast = paladin.clone();
    fast.modules
        .insert(id(eve, "Large Hyperspatial Velocity Optimizer II"), 1);
    let output = check(&data, &fast, &Pilot::default());
    assert!(!output.approved);
    assert_eq!(output.tags, ["NON-HYBRID-HYPERSPATIAL"]);

    // Exempt hulls are never flagged.
    let mut nestor = doctrine(&data, "ARMOR_NESTOR");
    nestor
        .modules
        .insert(id(eve, "Large Hyperspatial Velocity Optimizer II"), 1);
    assert_eq!(
        check(&data, &nestor, &Pilot::default()).tags,
        Vec::<String>::new()
    );
}

#[test]
fn fit_variation_rules_forgive_listed_swaps() {
    let Some((_dir, store)) = example_store() else {
        return;
    };
    let data = store.get();
    let eve = data.eve();
    let tractor = id(eve, "Small Tractor Beam I");
    let repairer = id(eve, "Large Remote Armor Repairer II");

    let paladin = doctrine(&data, "ARMOR_PALADIN");
    assert_eq!(paladin.modules.get(&tractor), Some(&1));
    let mut swapped = paladin.clone();
    swapped.modules.remove(&tractor);
    *swapped.modules.entry(repairer).or_insert(0) += 1;
    let output = check(&data, &swapped, &Pilot::default());
    assert!(output.approved, "{output:?}");
    let analysis = output.analysis.unwrap();
    assert!(analysis.missing.is_empty() && analysis.extra.is_empty());

    // Swapping in two repairers is not the listed exception.
    *swapped.modules.entry(repairer).or_insert(0) += 1;
    let output = check(&data, &swapped, &Pilot::default());
    assert!(!output.approved);
}

#[test]
fn srp_modified_annotation() {
    let Some((_dir, store)) = example_store() else {
        return;
    };
    let data = store.get();
    let eve = data.eve();
    let paladin = doctrine(&data, "ARMOR_PALADIN");
    let peace = id(eve, "'Peace' Large Remote Armor Repairer");
    let mut fit = paladin.clone();
    fit.modules.insert(peace, 1);
    let analysis = check(&data, &fit, &Pilot::default()).analysis.unwrap();
    let json = serde_json::to_value(&analysis).unwrap();
    assert_eq!(
        json["srp_modified"],
        serde_json::json!([{"type_id": peace, "flat": 1000000000}])
    );

    // Hull-scoped: no annotation on a Kronos.
    let mut kronos = doctrine(&data, "ARMOR_KRONOS");
    kronos.modules.insert(peace, 1);
    let analysis = check(&data, &kronos, &Pilot::default()).analysis.unwrap();
    assert!(analysis.srp_modified.is_empty());
    assert!(
        serde_json::to_value(&analysis)
            .unwrap()
            .get("srp_modified")
            .is_none()
    );
}

#[test]
fn unmatched_hull_is_not_approved() {
    let Some((_dir, store)) = example_store() else {
        return;
    };
    let data = store.get();
    let eve = data.eve();
    let fit = eft(eve, "[Rifter, test]\nDamage Control II\n");
    let output = check(&data, &fit, &Pilot::default());
    assert!(!output.approved);
    assert_eq!(output.category, "other");
    assert_eq!(output.analysis, None);
}

/// Ported from the legacy fit differ test.
#[test]
fn diff_classifies_missing_extra_upgrades_and_downgrades() {
    let Some((_dir, store)) = example_store() else {
        return;
    };
    let data = store.get();
    let eve = data.eve();
    let expect = eft(
        eve,
        "[Atron, test]\nMega Pulse Laser II\nCore X-Type 500MN Microwarpdrive\n10MN Afterburner II\nCentum A-Type Multispectrum Energized Membrane\nConflagration L x123\n",
    );
    let actual = eft(
        eve,
        "[Atron, test]\nMega Pulse Laser II\n10MN Abyssal Afterburner\nCentii A-Type Multispectrum Coating\nSensor Booster II\n",
    );
    let diff = data.diff(&expect, &actual);

    assert_eq!(
        diff.cargo_missing,
        BTreeMap::from([(id(eve, "Conflagration L"), 123)])
    );
    assert_eq!(
        diff.module_missing,
        BTreeMap::from([(id(eve, "Core X-Type 500MN Microwarpdrive"), 1)])
    );
    assert_eq!(
        diff.module_extra,
        BTreeMap::from([(id(eve, "Sensor Booster II"), 1)])
    );
    assert_eq!(
        diff.module_upgraded,
        BTreeMap::from([(
            id(eve, "10MN Afterburner II"),
            BTreeMap::from([(id(eve, "10MN Abyssal Afterburner"), 1)])
        )])
    );
    assert_eq!(
        diff.module_downgraded,
        BTreeMap::from([(
            id(eve, "Centum A-Type Multispectrum Energized Membrane"),
            BTreeMap::from([(id(eve, "Centii A-Type Multispectrum Coating"), 1)])
        )])
    );
}

/// Ported from the legacy module variation test.
#[test]
fn module_variations() {
    let Some((_dir, store)) = example_store() else {
        return;
    };
    let data = store.get();
    let eve = data.eve();
    let meta_diff = |from: &str, to: &str| {
        let to_id = id(eve, to);
        data.modules
            .variator
            .get(id(eve, from))
            .unwrap_or_else(|| panic!("no variations for {from}"))
            .iter()
            .find(|variation| variation.to == to_id)
            .unwrap_or_else(|| panic!("{to} is not a variation of {from}"))
            .meta_diff
    };

    // Alternatives
    assert!(
        meta_diff(
            "Agency 'Pyrolancea' DB5 Dose II",
            "Agency 'Pyrolancea' DB3 Dose I"
        ) < 0
    );
    // The example modules.yaml puts DB5, DB7 and DB9 in the same tier.
    assert_eq!(
        meta_diff(
            "Agency 'Pyrolancea' DB5 Dose II",
            "Agency 'Pyrolancea' DB7 Dose III"
        ),
        0
    );
    assert!(
        meta_diff(
            "Agency 'Pyrolancea' DB3 Dose I",
            "Agency 'Pyrolancea' DB9 Dose IV"
        ) > 0
    );
    assert_eq!(
        meta_diff(
            "Agency 'Pyrolancea' DB5 Dose II",
            "Agency 'Pyrolancea' DB5 Dose II"
        ),
        0
    );
    // Tech 2 to tech 1
    assert!(
        meta_diff(
            "Heavy Hull Maintenance Bot II",
            "Heavy Hull Maintenance Bot I"
        ) < 0
    );
    assert!(
        meta_diff(
            "Heavy Hull Maintenance Bot I",
            "Heavy Hull Maintenance Bot II"
        ) > 0
    );
    assert_eq!(
        meta_diff(
            "Heavy Hull Maintenance Bot II",
            "Heavy Hull Maintenance Bot II"
        ),
        0
    );
    // Meta levels
    assert!(
        meta_diff(
            "Core X-Type 500MN Microwarpdrive",
            "Core C-Type 500MN Microwarpdrive"
        ) < 0
    );
    assert!(meta_diff("500MN Microwarpdrive I", "Core C-Type 500MN Microwarpdrive") > 0);
    assert_eq!(
        meta_diff(
            "Gist X-Type 500MN Microwarpdrive",
            "Core X-Type 500MN Microwarpdrive"
        ),
        0
    );
    // Attributes
    assert!(
        meta_diff(
            "Centum A-Type Multispectrum Energized Membrane",
            "Centii A-Type Multispectrum Coating"
        ) < 0
    );
    assert_eq!(
        meta_diff(
            "Centum A-Type Multispectrum Energized Membrane",
            "Corpum A-Type Multispectrum Energized Membrane"
        ),
        0
    );
    assert_eq!(
        meta_diff(
            "Federation Navy Multispectrum Energized Membrane",
            "Multispectrum Energized Membrane II"
        ),
        0
    );
}

/// FitAnalysis JSON (the stored and served shape) of every example doctrine fit, checked as
/// flown and with a typical mistake (one turret downgraded, one module missing, half the cargo).
#[test]
fn fit_analysis_snapshots() {
    let Some((_dir, store)) = example_store() else {
        return;
    };
    let data = store.get();
    let mut snapshot = BTreeMap::new();
    for doctrine in data.fits.iter() {
        let output = check(&data, &doctrine.fit, &Pilot::default());
        snapshot.insert(
            format!("{} as flown", doctrine.name),
            serde_json::json!({
                "approved": output.approved,
                "category": output.category,
                "analysis": output.analysis,
            }),
        );

        let mut sloppy = doctrine.fit.clone();
        let downgrade = sloppy.modules.keys().copied().find_map(|module| {
            data.modules
                .variator
                .get(module)?
                .iter()
                .find(|variation| variation.meta_diff < 0)
                .map(|variation| (module, variation.to))
        });
        if let Some((from, to)) = downgrade {
            let count = sloppy.modules[&from];
            if count == 1 {
                sloppy.modules.remove(&from);
            } else {
                sloppy.modules.insert(from, count - 1);
            }
            *sloppy.modules.entry(to).or_insert(0) += 1;
        }
        if let Some(&last) = sloppy.modules.keys().last() {
            sloppy.modules.remove(&last);
        }
        for count in sloppy.cargo.values_mut() {
            *count /= 2;
        }
        sloppy.cargo.retain(|_, count| *count > 0);
        let output = check(&data, &sloppy, &Pilot::default());
        snapshot.insert(
            format!("{} sloppy", doctrine.name),
            serde_json::json!({
                "approved": output.approved,
                "category": output.category,
                "analysis": output.analysis,
            }),
        );
    }
    insta::assert_json_snapshot!("fit_analysis", snapshot);
}
