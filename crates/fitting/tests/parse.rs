//! EFT and DNA parsing against the real static data (ported from the legacy waitlist's tests).

mod common;

use common::eve;
use wl_fitting::Fitting;

#[test]
fn parse_dna() {
    let Some(eve) = eve() else { return };
    let parsed = Fitting::from_dna("670::", &eve).unwrap();
    assert_eq!(parsed.hull, 670);
    assert!(parsed.modules.is_empty());
    assert!(parsed.cargo.is_empty());
    assert_eq!(parsed.to_dna(&eve).unwrap(), "670::");

    let parsed = Fitting::from_dna("59630:24554;1:23674;1:24550;1:24552;1::", &eve).unwrap();
    assert_eq!(parsed.hull, 59630);
    assert!(parsed.cargo.is_empty());
    assert_eq!(parsed.modules.len(), 4);
    for id in [24554, 23674, 24550, 24552] {
        assert_eq!(parsed.modules[&id], 1);
    }
    assert_eq!(
        parsed.to_dna(&eve).unwrap(),
        "59630:23674;1:24550;1:24552;1:24554;1::"
    );

    let parsed = Fitting::from_dna("17736:3057;4:12816;2:4383_;1::", &eve).unwrap();
    assert_eq!(parsed.hull, 17736);
    assert_eq!(parsed.modules.len(), 1);
    assert_eq!(parsed.cargo.len(), 2);
    assert_eq!(parsed.modules[&3057], 4);
    assert_eq!(parsed.cargo[&12816], 2);
    assert_eq!(parsed.cargo[&4383], 1);
    assert_eq!(
        parsed.to_dna(&eve).unwrap(),
        "17736:3057;4:4383_;1:12816;2::"
    );
}

#[test]
fn parse_eft() {
    let Some(eve) = eve() else { return };
    let parsed = Fitting::from_eft(
        "[Nightmare, Nightmare]


Mega Pulse Laser II
Mega Pulse Laser II
Mega Pulse Laser II
Mega Pulse Laser II





Conflagration L x2
Large Micro Jump Drive x1
",
        &eve,
    )
    .unwrap();
    assert_eq!(parsed.len(), 1);
    let parsed = &parsed[0];
    assert_eq!(parsed.hull, 17736);
    assert_eq!(parsed.modules.len(), 1);
    assert_eq!(parsed.cargo.len(), 2);
    assert_eq!(parsed.modules[&3057], 4);
    assert_eq!(parsed.cargo[&12816], 2);
    assert_eq!(parsed.cargo[&4383], 1);
    assert_eq!(
        parsed.to_dna(&eve).unwrap(),
        "17736:3057;4:4383_;1:12816;2::"
    );
}

#[test]
fn parse_eft_drones_and_cargo_sections() {
    let Some(eve) = eve() else { return };
    let parsed = Fitting::from_eft(
        "[Venture, *Simulated Venture Fitting]






Hobgoblin II x2

1600mm Steel Plates II x1

",
        &eve,
    )
    .unwrap()
    .pop()
    .unwrap();
    assert_eq!(parsed.modules[&2456], 2);
    assert_eq!(parsed.cargo[&20353], 1);
}
