//! End-to-end integration tests against the real WoT 0.6.5 installation and the
//! Python-2 reference payloads produced while reverse-engineering the client.
//!
//! Tests that need an input which may be absent on another machine (the retail install,
//! the research artifacts, the bundled Python interpreters) print a `skip:` line and
//! return instead of failing. On this development machine all of them run.

use std::path::{Path, PathBuf};
use std::process::Command;

use wot065_data::account::{self, FullSyncOptions};
use wot065_data::catalog;
use wot065_data::pickle;
use wot065_data::testutil;

fn game_dir() -> Option<PathBuf> {
    testutil::game_dir()
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create test dir");
    dir
}

/// Load the real catalog once per test that needs it.
fn real_catalog() -> Option<catalog::Catalog> {
    let dir = game_dir()?;
    let catalog = catalog::load(&dir).expect("catalog loads from the retail install");
    Some(catalog)
}

#[test]
fn known_vehicles_from_the_retail_game_files() {
    let Some(catalog) = real_catalog() else {
        eprintln!("skip: no WoT 0.6.5 installation");
        return;
    };
    assert_eq!(catalog.len(), 130, "0.6.5 has 130 vehicle types");
    assert_eq!(
        catalog.counts_by_nation(),
        vec![("ussr", 45), ("germany", 44), ("usa", 41)]
    );
    assert!(catalog.problems.is_empty(), "problems: {:?}", catalog.problems);

    let expectations = [
        // (name, nationID, typeID, level, vehTypeCompDescr, vehCompDescrHex)
        ("T-34", 0, 0u16, 5u8, 1i32, "010000000000cb0003000000000000"),
        ("MS-1", 0, 13, 1, 3329, "010d1a000e00c90000001700170000"),
        ("T-34-85", 0, 10, 6, 2561, "010a14000100cb0003001100060000"),
        ("PzIV", 1, 0, 5, 17, "110000000000cb0002000000000000"),
        ("Maus", 1, 27, 10, 6929, "111b39001900cd000d002d00250000"),
        ("Lowe", 1, 212, 8, 54289, "11d455002800cd0010004300330000"),
        ("T1_Cunningham", 2, 2, 1, 545, "210203000400c90003000300040000"),
    ];
    for (name, nation_id, type_id, level, type_cd, hex) in expectations {
        let vehicle = catalog
            .by_name(name)
            .unwrap_or_else(|| panic!("{name} missing from catalog"));
        assert_eq!(vehicle.nation_id, nation_id, "{name}: nation id");
        assert_eq!(vehicle.type_id, type_id, "{name}: type id");
        assert_eq!(vehicle.level, level, "{name}: tier");
        assert_eq!(vehicle.veh_type_comp_descr, type_cd, "{name}: vehTypeCompDescr");
        assert_eq!(vehicle.veh_comp_descr_hex, hex, "{name}: vehCompDescr");
        assert_eq!(vehicle.comp_descr.len(), 15, "{name}: descriptor length");
        // The binary descriptor decodes back to the components we read from the XML.
        let parsed = catalog::parse_comp_descr(&vehicle.comp_descr).expect("decodes");
        assert_eq!(parsed.nation_id, nation_id as u8);
        assert_eq!(parsed.type_id as u16, type_id);
        assert_eq!(parsed.chassis, vehicle.component_ids.chassis);
        assert_eq!(parsed.gun, vehicle.component_ids.gun);
    }

    // Every vehicle has a decodable 15-byte descriptor.
    for vehicle in &catalog.vehicles {
        assert_eq!(vehicle.comp_descr.len(), 15, "{}: length", vehicle.name);
        catalog::parse_comp_descr(&vehicle.comp_descr)
            .unwrap_or_else(|e| panic!("{}: {e}", vehicle.name));
    }
}

#[test]
fn catalog_json_is_field_identical_to_the_reference_catalog() {
    let (Some(catalog), Some(reference_path)) = (real_catalog(), testutil::reference_catalog_path())
    else {
        eprintln!("skip: need game install + reference catalog");
        return;
    };
    let reference: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&reference_path).expect("reference json"))
            .expect("parse reference json");
    let reference_vehicles = reference["vehicles"].as_array().expect("vehicles array");
    assert_eq!(reference["count"].as_u64(), Some(130));
    assert_eq!(reference_vehicles.len(), catalog.vehicles.len());

    for (index, vehicle) in catalog.vehicles.iter().enumerate() {
        let expected = &reference_vehicles[index];
        let actual = serde_json::to_value(vehicle).expect("serialize vehicle");
        assert_eq!(
            &actual,
            expected,
            "catalog entry {index} ({}) differs from the reference catalog",
            vehicle.name
        );
    }
}

#[test]
fn full_sync_diff_matches_reference_baseline_and_populates_hangar_data() {
    let (Some(catalog), Some(reference_dir)) = (real_catalog(), testutil::reference_dir()) else {
        eprintln!("skip: need game install + reference payloads");
        return;
    };
    let reference_bytes =
        std::fs::read(reference_dir.join("account_sync_full.pickle")).expect("reference pickle");
    let reference = pickle::loads(&reference_bytes).expect("parse reference pickle");

    let ours = account::build_full_sync(&catalog, &FullSyncOptions::default());
    for key in ["rev", "prevRev", "stats", "account", "cache"] {
        assert_eq!(
            ours.get(key),
            reference.get(key),
            "our CMD_SYNC_DATA `{key}` must match the reference baseline"
        );
    }

    let inventory = ours.get("inventory").expect("inventory");
    for kind in 1..=11 {
        assert!(
            matches!(inventory.get_int(kind), Some(pickle::Value::Dict(_))),
            "inventory missing dict for item type {kind}"
        );
    }
    let vehicles = inventory
        .get_int(account::item_type::VEHICLE)
        .expect("inventory[1]");
    let ref_vehicles = reference
        .get("inventory")
        .unwrap()
        .get_int(account::item_type::VEHICLE)
        .unwrap();
    assert_eq!(
        vehicles.get("compDescr"),
        ref_vehicles.get("compDescr"),
        "owned vehicle descriptors must match the reference"
    );
    let comp_descrs = vehicles.get("compDescr").expect("compDescr").as_dict().unwrap();
    assert_eq!(comp_descrs.len(), 130, "all 130 vehicles are owned");
    for (key, value) in comp_descrs {
        assert!(key.as_int().is_some(), "vehInvID is an int");
        let bytes = value.as_bytes().expect("vehCompDescr is a byte str");
        assert_eq!(bytes.len(), 15);
        catalog::parse_comp_descr(bytes).expect("descriptor decodes");
    }
    // The nine inventory[1] sub-keys the client parser indexes.
    for key in [
        "compDescr",
        "shellsLayout",
        "shells",
        "crew",
        "repair",
        "eqsLayout",
        "eqs",
        "settings",
        "lock",
    ] {
        assert!(vehicles.contains(key), "inventory[1] misses {key}");
    }
}

#[test]
fn real_catalog_populates_full_crews_default_shells_and_shop_items() {
    let Some(catalog) = real_catalog() else {
        eprintln!("skip: no WoT 0.6.5 installation");
        return;
    };
    let diff = account::build_full_sync(&catalog, &FullSyncOptions::default());
    let shop = account::build_shop_cache_for_catalog(&catalog);

    let inv = diff.get("inventory").expect("inventory");
    let veh = inv.get_int(account::item_type::VEHICLE).expect("inventory[1]");
    let crew_map = veh.get("crew").expect("crew");
    let shells_map = veh.get("shells").expect("shells");
    let tankmen = inv.get_int(account::item_type::TANKMAN).expect("inventory[8]");
    let tman_cds = tankmen.get("compDescr").expect("tman compDescr");
    let tman_vehs = tankmen.get("vehicle").expect("tman vehicle");
    let shop_items = shop.get("items").expect("shop items");

    let mut seen_tman_ids = std::collections::HashSet::new();
    for (idx, vehicle) in catalog.vehicles.iter().enumerate() {
        let veh_inv_id = idx as i64 + 1;
        let crew = crew_map
            .get_int(veh_inv_id)
            .and_then(|v| v.as_list())
            .unwrap_or_else(|| panic!("{} missing crew", vehicle.name));
        assert!(
            !crew.is_empty(),
            "{} has empty crew list",
            vehicle.name
        );
        for tman_val in crew {
            let tid = tman_val.as_int().expect("tmanInvID int");
            assert!(
                seen_tman_ids.insert(tid),
                "duplicate tmanInvID {tid} on {}",
                vehicle.name
            );
            assert_eq!(
                tman_vehs.get_int(tid).and_then(|v| v.as_int()),
                Some(veh_inv_id)
            );
            let cd = tman_cds
                .get_int(tid)
                .and_then(|v| v.as_bytes())
                .unwrap_or_else(|| panic!("missing compDescr for tman {tid}"));
            assert_eq!(cd.len(), 19);
            assert_eq!(cd[0] & 0x0f, account::item_type::TANKMAN as u8);
            assert_eq!((cd[0] >> 4) & 0x0f, vehicle.nation_id);
            assert_eq!(cd[1], vehicle.type_id as u8);
            assert!((1..=5).contains(&cd[2]), "invalid role id {}", cd[2]);
            assert_eq!(cd[3], 100);
        }

        let shells = shells_map
            .get_int(veh_inv_id)
            .and_then(|v| v.as_list())
            .unwrap_or_else(|| panic!("{} missing default shells", vehicle.name));
        assert!(
            !shells.is_empty() && shells.len() % 2 == 0,
            "{}: invalid shells list len {}",
            vehicle.name,
            shells.len()
        );
        let nation_shells = shop_items
            .get_int(vehicle.nation_id as i64)
            .unwrap()
            .get_int(account::item_type::SHELL)
            .unwrap()
            .as_list()
            .unwrap();
        let mut total_shells = 0i64;
        for pair in shells.chunks_exact(2) {
            let shell_cd = pair[0].as_int().unwrap();
            let count = pair[1].as_int().unwrap();
            total_shells += count;
            assert!(
                nation_shells[0].get_int(shell_cd).is_some(),
                "{} default shell {shell_cd} missing from shop['items'][{}][10]",
                vehicle.name,
                vehicle.nation_id
            );
        }
        assert!(total_shells > 0, "{} has zero total shells", vehicle.name);

        let nation_shop = shop_items.get_int(vehicle.nation_id as i64).unwrap();
        for (mod_type, comp_id) in [
            (account::item_type::CHASSIS, vehicle.component_ids.chassis),
            (account::item_type::TURRET, vehicle.component_ids.turret),
            (account::item_type::GUN, vehicle.component_ids.gun),
            (account::item_type::ENGINE, vehicle.component_ids.engine),
            (account::item_type::FUEL_TANK, vehicle.component_ids.fuel_tank),
            (account::item_type::RADIO, vehicle.component_ids.radio),
        ] {
            let mod_cd = catalog::make_item_comp_descr(mod_type as u8, vehicle.nation_id, comp_id);
            let bucket = nation_shop.get_int(mod_type).unwrap().as_list().unwrap();
            assert!(
                bucket[0].get_int(mod_cd).is_some(),
                "{} stock component type={mod_type} id={comp_id} missing from shop",
                vehicle.name
            );
        }
    }

    // Spot-check expected crew sizes from XML.
    let ms1 = catalog.by_name("MS-1").unwrap();
    let ms1_inv_id = ms1.index_in(&catalog).unwrap() as i64 + 1;
    assert_eq!(
        crew_map.get_int(ms1_inv_id).unwrap().as_list().unwrap().len(),
        2
    );
    let t34 = catalog.by_name("T-34").unwrap();
    let t34_inv_id = t34.index_in(&catalog).unwrap() as i64 + 1;
    assert_eq!(
        crew_map.get_int(t34_inv_id).unwrap().as_list().unwrap().len(),
        4
    );

    // Shop buckets populated across all 3 nations + common NONE_INDEX(15).
    for (nation_id, expected_vehicles) in [(0i64, 45usize), (1, 44), (2, 41)] {
        let nation = shop_items.get_int(nation_id).unwrap();
        assert_eq!(
            nation
                .get_int(account::item_type::VEHICLE)
                .unwrap()
                .as_list()
                .unwrap()[0]
                .as_dict()
                .unwrap()
                .len(),
            expected_vehicles
        );
        for item_type in 2..=7 {
            assert!(
                !nation
                    .get_int(item_type)
                    .unwrap()
                    .as_list()
                    .unwrap()[0]
                    .as_dict()
                    .unwrap()
                    .is_empty(),
                "nation {nation_id} module type {item_type} unexpectedly empty"
            );
        }
        assert!(
            !nation
                .get_int(account::item_type::SHELL)
                .unwrap()
                .as_list()
                .unwrap()[0]
                .as_dict()
                .unwrap()
                .is_empty()
        );
    }
    let lowe = catalog.by_name("Lowe").unwrap();
    let lowe_price = shop_items
        .get_int(1)
        .unwrap()
        .get_int(account::item_type::VEHICLE)
        .unwrap()
        .as_list()
        .unwrap()[0]
        .get_int(lowe.type_id as i64)
        .unwrap()
        .as_list()
        .unwrap();
    assert_eq!(lowe_price[0].as_int(), Some(0));
    assert!(lowe_price[1].as_int().unwrap() > 0);

    let common = shop_items.get_int(15).unwrap();
    for item_type in [
        account::item_type::OPTIONAL_DEVICE,
        account::item_type::EQUIPMENT,
    ] {
        assert!(
            !common
                .get_int(item_type)
                .unwrap()
                .as_list()
                .unwrap()[0]
                .as_dict()
                .unwrap()
                .is_empty(),
            "common item type {item_type} unexpectedly empty"
        );
    }
}

#[test]
fn reference_stream_payload_and_description_are_consistent() {
    let Some(reference_dir) = testutil::reference_dir() else {
        eprintln!("skip: no reference payloads");
        return;
    };
    let pickled = std::fs::read(reference_dir.join("account_sync_full.pickle")).expect("pickle");
    let z = std::fs::read(reference_dir.join("account_sync_full.pickle.z")).expect("zlib payload");
    assert_eq!(pickle::zlib_decompress(&z).expect("decompress"), pickled);
    let desc = pickle::loads(
        &std::fs::read(reference_dir.join("account_sync_full.desc")).expect("desc"),
    )
    .expect("parse desc");
    let items = desc.as_list().expect("tuple");
    assert_eq!(items[0].as_int(), Some(z.len() as i64));
    assert_eq!(items[1].as_int(), Some(pickle::signed_crc32(&z)));
    // Our own stream helpers produce a self-consistent (payload, description) pair for the
    // same object (the exact zlib byte stream is not required to match Python's zlib).
    let (our_z, our_desc) = account::stream_bundle(&pickled);
    assert_eq!(pickle::zlib_decompress(&our_z).expect("decompress"), pickled);
    let our_desc = pickle::loads(&our_desc).expect("parse desc");
    let our_items = our_desc.as_list().expect("tuple");
    assert_eq!(our_items[0].as_int(), Some(our_z.len() as i64));
    assert_eq!(our_items[1].as_int(), Some(pickle::signed_crc32(&our_z)));
}

#[test]
fn cli_generates_the_contract_artifacts_and_they_validate_in_python() {
    let Some(game_dir) = game_dir() else {
        eprintln!("skip: no WoT 0.6.5 installation");
        return;
    };
    let out_dir = temp_dir("cli-artifacts");
    let status = Command::new(env!("CARGO_BIN_EXE_wot065-data"))
        .arg("all")
        .arg("--game-dir")
        .arg(&game_dir)
        .arg("--out-dir")
        .arg(&out_dir)
        .status()
        .expect("run CLI");
    assert!(status.success(), "`wot065-data all` failed");

    for file in [
        "vehicles_catalog.json",
        "account_sync_full.pickle",
        "account_sync_full.pickle.z",
        "account_sync_full.desc",
        "showgui_ctx.pickle",
        "shop_cache.pickle",
        "shop_cache.pickle.z",
        "shop_cache.desc",
        "dossier_sync.pickle",
        "dossier_sync.pickle.z",
        "dossier_sync.desc",
    ] {
        let path = out_dir.join(file);
        let len = std::fs::metadata(&path)
            .unwrap_or_else(|e| panic!("{file} missing: {e}"))
            .len();
        assert!(len > 0, "{file} is empty");
    }

    // The specification requires generated pickles to load under the bundled Python 3.
    let Some(python3) = testutil::python3() else {
        eprintln!("skip: bundled Python 3 not found");
        return;
    };
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/python_validate.py");
    let output = Command::new(&python3)
        .arg(&script)
        .arg("--dir")
        .arg(&out_dir)
        .arg("--expect-vehicles")
        .arg("130")
        .output()
        .expect("run python3 validator");
    println!("{}", String::from_utf8_lossy(&output.stdout));
    eprintln!("{}", String::from_utf8_lossy(&output.stderr));
    assert!(output.status.success(), "Python 3 structural validation failed");
}
