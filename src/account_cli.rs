//! Offline `wot-server account` management subcommands.

use std::path::PathBuf;

use anyhow::Result;
use clap::Subcommand;

#[derive(Debug, clap::Args)]
pub struct AccountArgs {
    #[command(subcommand)]
    pub command: AccountCommand,
}

#[derive(Debug, Subcommand)]
pub enum AccountCommand {
    /// Inspect persisted account state (balances, slots/berths, vehicles, tankmen).
    Show(ShowArgs),
    /// Reset (or create) an account to `retail` or `sandbox` preset defaults.
    Reset(ResetArgs),
    /// Grant credits, gold, free XP, vehicle XP, tankman XP, or unlock all items.
    Grant(GrantArgs),
    /// Damage one or all vehicles in the garage to test in-game repairs.
    Damage(DamageArgs),
}

#[derive(Debug, clap::Args)]
pub struct ShowArgs {
    /// Account username to inspect.
    pub username: String,
    /// Account storage directory containing `<name>.json` files.
    #[arg(long, default_value = ".scratch/accounts")]
    pub data_dir: PathBuf,
}

#[derive(Debug, clap::Args)]
pub struct ResetArgs {
    /// Account username to create or reset.
    pub username: String,
    /// Initial account preset (`retail` or `sandbox`).
    #[arg(long, default_value = "retail")]
    pub preset: String,
    /// Account storage directory containing `<name>.json` files.
    #[arg(long, default_value = ".scratch/accounts")]
    pub data_dir: PathBuf,
    /// WoT 0.6.5 game directory (`res/scripts/item_defs/...`).
    #[arg(long, default_value = r"C:\Users\qwerty\Desktop\World_of_Tanks")]
    pub game_dir: PathBuf,
    /// Optional path to custom economics config JSON.
    #[arg(long)]
    pub economics_config: Option<PathBuf>,
}

#[derive(Debug, clap::Args)]
pub struct GrantArgs {
    /// Account username to update.
    pub username: String,
    /// Credits delta to add.
    #[arg(long)]
    pub credits: Option<i64>,
    /// Gold delta to add.
    #[arg(long)]
    pub gold: Option<i64>,
    /// Free XP delta to add.
    #[arg(long)]
    pub free_xp: Option<i64>,
    /// Vehicle XP grant `<NATION:NAME|TYPE_COMP_DESCR>=<XP>` (repeatable).
    #[arg(long = "veh-xp", value_name = "VEH=XP")]
    pub veh_xp: Vec<String>,
    /// Tankman free XP grant `<TMAN_INV_ID>=<XP>` (repeatable).
    #[arg(long = "tman-xp", value_name = "TMAN_ID=XP")]
    pub tman_xp: Vec<String>,
    /// Unlock every tech-tree vehicle and module across all nations.
    #[arg(long)]
    pub unlock_all: bool,
    /// Account storage directory containing `<name>.json` files.
    #[arg(long, default_value = ".scratch/accounts")]
    pub data_dir: PathBuf,
    /// WoT 0.6.5 game directory (`res/scripts/item_defs/...`).
    #[arg(long, default_value = r"C:\Users\qwerty\Desktop\World_of_Tanks")]
    pub game_dir: PathBuf,
    /// Optional path to custom economics config JSON.
    #[arg(long)]
    pub economics_config: Option<PathBuf>,
}

#[derive(Debug, clap::Args)]
pub struct DamageArgs {
    /// Account username to update.
    pub username: String,
    /// Vehicle inventory ID to damage.
    #[arg(long, conflicts_with = "all")]
    pub veh_inv_id: Option<i64>,
    /// Damage every vehicle in the account's garage.
    #[arg(long)]
    pub all: bool,
    /// Target HP fraction in `0.0..=0.999` (default `0.5`).
    #[arg(long, default_value_t = 0.5)]
    pub hp_fraction: f64,
    /// Account storage directory containing `<name>.json` files.
    #[arg(long, default_value = ".scratch/accounts")]
    pub data_dir: PathBuf,
    /// WoT 0.6.5 game directory (`res/scripts/item_defs/...`).
    #[arg(long, default_value = r"C:\Users\qwerty\Desktop\World_of_Tanks")]
    pub game_dir: PathBuf,
}

/// Dispatch `wot-server account <subcommand>` and print human-readable results.
pub fn run_account(args: AccountArgs) -> Result<()> {
    let out = match args.command {
        AccountCommand::Show(a) => execute_show(&a)?,
        AccountCommand::Reset(a) => execute_reset(&a)?,
        AccountCommand::Grant(a) => execute_grant(&a)?,
        AccountCommand::Damage(a) => execute_damage(&a)?,
    };
    println!("{out}");
    Ok(())
}

use std::path::Path;
use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context};
use wot065_data::account_state::{create_profile, AccountProfile, Preset};
use wot065_data::catalog::{self, Catalog};
use wot065_data::descr::VehicleDescr;
use wot065_data::economics::EconomicsConfig;
use wot065_data::ops::{AccountEngine, EngineCmd};
use wot065_data::store::{damage_vehicle, JsonAccountStore};

fn now_utc_secs() -> u32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .min(u32::MAX as u64) as u32
}

fn load_catalog_and_economics(
    game_dir: &Path,
    economics_config: Option<&Path>,
) -> Result<(Catalog, EconomicsConfig)> {
    let catalog = catalog::load(game_dir)
        .with_context(|| format!("failed to load WoT catalog from {}", game_dir.display()))?;
    let economics = match economics_config {
        Some(path) => EconomicsConfig::load_from_file(path)
            .with_context(|| format!("failed to load economics config from {}", path.display()))?,
        None => EconomicsConfig::default(),
    };
    Ok((catalog, economics))
}

fn parse_veh_xp_spec(spec: &str, catalog: &Catalog) -> Result<(i32, i32)> {
    let (lhs, rhs) = spec
        .split_once('=')
        .with_context(|| format!("invalid --veh-xp '{spec}': expected <NATION:NAME|TYPE_COMP_DESCR>=<XP>"))?;
    let lhs = lhs.trim();
    let xp: i32 = rhs
        .trim()
        .parse()
        .with_context(|| format!("invalid XP amount in --veh-xp '{spec}'"))?;
    if xp < 0 {
        bail!("vehicle XP grant must be non-negative (got {xp})");
    }

    if let Ok(cd) = lhs.parse::<i32>() {
        if catalog.vehicle_def(cd).is_none() {
            bail!("unknown vehicle compact descriptor {cd} in catalog");
        }
        return Ok((cd, xp));
    }

    let (nation_part, name_part) = lhs
        .split_once(':')
        .with_context(|| format!("invalid vehicle selector '{lhs}': expected <NATION:NAME> or numeric compact descriptor"))?;
    let nation_q = nation_part.trim();
    let name_q = name_part.trim();
    let nation_id_q: Option<u8> = match nation_q.to_ascii_lowercase().as_str() {
        "ussr" | "0" => Some(0),
        "germany" | "1" => Some(1),
        "usa" | "2" => Some(2),
        _ => None,
    };
    let found = catalog.vehicle_defs.values().find(|def| {
        let nation_matches = def.nation.eq_ignore_ascii_case(nation_q)
            || nation_id_q.is_some_and(|id| def.nation_id == id);
        nation_matches && def.name.eq_ignore_ascii_case(name_q)
    });
    match found {
        Some(def) => Ok((def.veh_type_comp_descr, xp)),
        None => bail!("vehicle '{lhs}' not found in catalog"),
    }
}

fn parse_tman_xp_spec(spec: &str) -> Result<(i64, i32)> {
    let (lhs, rhs) = spec
        .split_once('=')
        .with_context(|| format!("invalid --tman-xp '{spec}': expected <TMAN_INV_ID>=<XP>"))?;
    let tman_inv_id: i64 = lhs
        .trim()
        .parse()
        .with_context(|| format!("invalid tankman inv_id in --tman-xp '{spec}'"))?;
    let xp: i32 = rhs
        .trim()
        .parse()
        .with_context(|| format!("invalid XP amount in --tman-xp '{spec}'"))?;
    if xp < 0 {
        bail!("tankman XP grant must be non-negative (got {xp})");
    }
    Ok((tman_inv_id, xp))
}

fn format_account_summary(profile: &AccountProfile) -> String {
    let mut lines = Vec::new();
    lines.push(format!(
        "account: {} (normalized='{}', database_id={}, preset={}, rev={})",
        profile.username,
        profile.normalized_name,
        profile.database_id,
        profile.preset,
        profile.rev
    ));
    lines.push(format!(
        "balances: credits={}, gold={}, free_xp={}, slots={}, berths={}, premium_expiry={}",
        profile.credits,
        profile.gold,
        profile.free_xp,
        profile.slots,
        profile.berths,
        profile.premium_expiry_time
    ));
    lines.push(format!(
        "progression: unlocks={}, elite_vehicles={}, veh_xp_entries={}, depot_items={}",
        profile.unlocks.len(),
        profile.elite_vehicles.len(),
        profile.veh_type_xp.len(),
        profile.items.len()
    ));
    lines.push(format!("vehicles ({}):", profile.vehicles.len()));
    for (inv_id, veh) in &profile.vehicles {
        let type_info = VehicleDescr::from_bytes(&veh.comp_descr)
            .map(|d| format!("nation={} type_id={} turret={} gun={}", d.nation_id, d.type_id, d.turret_id, d.gun_id))
            .unwrap_or_else(|_| format!("raw_bytes={}", veh.comp_descr.len()));
        let repair_str = match veh.repair {
            Some((cost, hp)) => format!("damaged(cost={cost}, hp={hp})"),
            None => "healthy".to_string(),
        };
        let crew_assigned = veh.crew.iter().filter(|s| s.is_some()).count();
        lines.push(format!(
            "  - inv_id={inv_id}: {type_info}, crew={crew_assigned}/{}, {repair_str}",
            veh.crew.len()
        ));
    }
    lines.push(format!("tankmen ({}):", profile.tankmen.len()));
    for (inv_id, tman) in &profile.tankmen {
        let assigned = tman
            .vehicle_inv_id
            .map(|v| format!("veh_inv_id={v}"))
            .unwrap_or_else(|| "barracks".to_string());
        lines.push(format!("  - inv_id={inv_id}: {assigned}"));
    }
    lines.join("\n")
}

pub fn execute_show(args: &ShowArgs) -> Result<String> {
    let store = JsonAccountStore::open(&args.data_dir)
        .with_context(|| format!("failed to open account store at {}", args.data_dir.display()))?;
    let profile = store.load_required(&args.username)?;
    Ok(format_account_summary(&profile))
}

pub fn execute_reset(args: &ResetArgs) -> Result<String> {
    let preset = Preset::from_str(&args.preset).map_err(anyhow::Error::msg)?;
    let (catalog, economics) =
        load_catalog_and_economics(&args.game_dir, args.economics_config.as_deref())?;
    let store = JsonAccountStore::open(&args.data_dir)
        .with_context(|| format!("failed to open account store at {}", args.data_dir.display()))?;

    let profile = if let Some(existing) = store.load(&args.username)? {
        let rebuilt = create_profile(
            existing.database_id,
            &args.username,
            preset,
            &catalog,
            &economics,
            now_utc_secs(),
        )
        .map_err(anyhow::Error::msg)?;
        store.save(&rebuilt)?;
        rebuilt
    } else {
        store.create_new(&args.username, preset, &catalog, &economics)?
    };

    Ok(format!(
        "reset account '{}' (normalized='{}', database_id={}) to preset '{}' (credits={}, gold={}, vehicles={}, tankmen={})\nnote: offline store updated; restart `wot-server serve` if currently running.",
        profile.username,
        profile.normalized_name,
        profile.database_id,
        profile.preset,
        profile.credits,
        profile.gold,
        profile.vehicles.len(),
        profile.tankmen.len()
    ))
}

pub fn execute_grant(args: &GrantArgs) -> Result<String> {
    if args.credits.is_none()
        && args.gold.is_none()
        && args.free_xp.is_none()
        && args.veh_xp.is_empty()
        && args.tman_xp.is_empty()
        && !args.unlock_all
    {
        bail!(
            "nothing to grant: pass at least one of --credits, --gold, --free-xp, --veh-xp, --tman-xp, or --unlock-all"
        );
    }
    if let Some(c) = args.credits.filter(|&v| v < 0) {
        bail!("--credits grant must be non-negative (got {c})");
    }
    if let Some(g) = args.gold.filter(|&v| v < 0) {
        bail!("--gold grant must be non-negative (got {g})");
    }
    if let Some(fxp) = args.free_xp.filter(|&v| v < 0) {
        bail!("--free-xp grant must be non-negative (got {fxp})");
    }

    let (catalog, economics) =
        load_catalog_and_economics(&args.game_dir, args.economics_config.as_deref())?;
    let veh_grants = args
        .veh_xp
        .iter()
        .map(|s| parse_veh_xp_spec(s, &catalog))
        .collect::<Result<Vec<_>>>()?;
    let tman_grants = args
        .tman_xp
        .iter()
        .map(|s| parse_tman_xp_spec(s))
        .collect::<Result<Vec<_>>>()?;

    let store = JsonAccountStore::open(&args.data_dir)
        .with_context(|| format!("failed to open account store at {}", args.data_dir.display()))?;
    let mut profile = store.load_required(&args.username)?;
    let now = now_utc_secs();

    let mut currency_changed = false;
    if let Some(c) = args.credits {
        profile.credits = profile.credits.saturating_add(c);
        currency_changed = true;
    }
    if let Some(g) = args.gold {
        profile.gold = profile.gold.saturating_add(g);
        currency_changed = true;
    }
    if let Some(fxp) = args.free_xp {
        profile.free_xp = profile.free_xp.saturating_add(fxp);
        currency_changed = true;
    }
    if currency_changed && !args.unlock_all && veh_grants.is_empty() && tman_grants.is_empty() {
        profile.rev += 1;
    }

    if args.unlock_all {
        AccountEngine::apply(&mut profile, &EngineCmd::UnlockAll, &catalog, &economics, 1, now)
            .map_err(|e| anyhow::anyhow!("unlock-all failed: {}", e.message))?;
    }
    for (veh_type_cd, xp) in veh_grants {
        AccountEngine::apply(
            &mut profile,
            &EngineCmd::AddXp { veh_type_cd, xp },
            &catalog,
            &economics,
            1,
            now,
        )
        .map_err(|e| anyhow::anyhow!("add vehicle XP failed: {}", e.message))?;
    }
    for (tman_inv_id, xp) in tman_grants {
        AccountEngine::apply(
            &mut profile,
            &EngineCmd::AddTmanXp { tman_inv_id, xp },
            &catalog,
            &economics,
            1,
            now,
        )
        .map_err(|e| anyhow::anyhow!("add tankman XP failed: {}", e.message))?;
    }

    store.save(&profile)?;
    Ok(format!(
        "granted resources to '{}' (rev={}): credits={}, gold={}, free_xp={}, unlocks={}\nnote: offline store updated; restart `wot-server serve` if currently running.",
        profile.username,
        profile.rev,
        profile.credits,
        profile.gold,
        profile.free_xp,
        profile.unlocks.len()
    ))
}

pub fn execute_damage(args: &DamageArgs) -> Result<String> {
    if !args.all && args.veh_inv_id.is_none() {
        bail!("specify --veh-inv-id <ID> or --all");
    }
    let catalog = catalog::load(&args.game_dir)
        .with_context(|| format!("failed to load WoT catalog from {}", args.game_dir.display()))?;
    let store = JsonAccountStore::open(&args.data_dir)
        .with_context(|| format!("failed to open account store at {}", args.data_dir.display()))?;
    let mut profile = store.load_required(&args.username)?;

    let targets: Vec<i64> = if args.all {
        profile.vehicles.keys().copied().collect()
    } else {
        vec![args.veh_inv_id.expect("checked is_some")]
    };
    if targets.is_empty() {
        bail!("account '{}' has no vehicles to damage", profile.username);
    }

    let mut summary = Vec::with_capacity(targets.len());
    for veh_inv_id in targets {
        let (cost, hp) =
            damage_vehicle(&mut profile, veh_inv_id, args.hp_fraction, &catalog)
                .map_err(anyhow::Error::msg)?;
        summary.push(format!("veh_inv_id={veh_inv_id}(hp={hp}, repair_cost={cost})"));
    }

    store.save(&profile)?;
    Ok(format!(
        "damaged {} vehicle(s) on '{}' (rev={}): {}\nnote: offline store updated; restart `wot-server serve` if currently running.",
        summary.len(),
        profile.username,
        profile.rev,
        summary.join(", ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use wot065_data::catalog;
    use wot065_data::descr::VehicleDescr;
    use wot065_data::store::JsonAccountStore;
    use wot065_data::testutil;

    fn test_temp_dir(label: &str) -> PathBuf {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target/test-tmp")
            .join(format!(
                "{label}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn test_game_dir() -> Option<PathBuf> {
        testutil::game_dir().or_else(|| {
            let p = Path::new(r"C:\Users\qwerty\Desktop\World_of_Tanks");
            p.join("res").exists().then(|| p.to_path_buf())
        })
    }

    #[test]
    fn account_reset_sandbox_and_show_roundtrip() {
        let Some(game_dir) = test_game_dir() else {
            eprintln!("skip: retail game files absent");
            return;
        };
        let data_dir = test_temp_dir("acct-cli-reset-show");

        let reset_out = execute_reset(&ResetArgs {
            username: "Alice@Test.com".into(),
            preset: "sandbox".into(),
            data_dir: data_dir.clone(),
            game_dir: game_dir.clone(),
            economics_config: None,
        })
        .expect("reset sandbox account");
        assert!(reset_out.contains("sandbox"));

        let store = JsonAccountStore::open(&data_dir).unwrap();
        let profile = store.load_required("alice@test.com").unwrap();
        assert_eq!(profile.credits, 50_000_000);
        assert_eq!(profile.gold, 1_000_000);
        assert_eq!(profile.vehicles.len(), 3);

        let show_out = execute_show(&ShowArgs {
            username: "Alice@Test.com".into(),
            data_dir: data_dir.clone(),
        })
        .expect("show account");
        assert!(show_out.contains("Alice@Test.com"));
        assert!(show_out.contains("50000000"));
        assert!(show_out.contains("vehicles"));

        let _ = std::fs::remove_dir_all(&data_dir);
    }

    #[test]
    fn account_grant_adds_currencies_xp_and_unlock_all() {
        let Some(game_dir) = test_game_dir() else {
            eprintln!("skip: retail game files absent");
            return;
        };
        let data_dir = test_temp_dir("acct-cli-grant");

        execute_reset(&ResetArgs {
            username: "tester".into(),
            preset: "retail".into(),
            data_dir: data_dir.clone(),
            game_dir: game_dir.clone(),
            economics_config: None,
        })
        .expect("reset retail");

        let store = JsonAccountStore::open(&data_dir).unwrap();
        let initial_profile = store.load_required("tester").unwrap();
        let tman_inv_id = *initial_profile
            .tankmen
            .keys()
            .next()
            .expect("retail preset must create starter tankmen");

        let grant_out = execute_grant(&GrantArgs {
            username: "tester".into(),
            credits: Some(5_000),
            gold: Some(100),
            free_xp: Some(200),
            veh_xp: vec!["ussr:MS-1=10000".into()],
            tman_xp: vec![format!("{tman_inv_id}=50000")],
            unlock_all: true,
            data_dir: data_dir.clone(),
            game_dir: game_dir.clone(),
            economics_config: None,
        })
        .expect("grant resources");
        assert!(grant_out.contains("tester"));

        let cat = catalog::load(&game_dir).unwrap();
        let ms1 = cat
            .vehicle_defs
            .values()
            .find(|v| v.nation_id == 0 && v.name == "MS-1")
            .unwrap();

        let profile = store.load_required("tester").unwrap();
        assert_eq!(profile.credits, 15_000); // 10_000 retail + 5_000
        assert_eq!(profile.gold, 100);
        assert_eq!(profile.free_xp, 200);
        assert_eq!(profile.veh_type_xp.get(&ms1.veh_type_comp_descr), Some(&10_000));
        assert!(
            profile.unlocks.len() > 10,
            "unlock-all must unlock full catalog tree"
        );
        let tman_descr = wot065_data::descr::TankmanDescr::from_bytes(
            &profile.tankmen[&tman_inv_id].comp_descr,
        )
        .unwrap();
        assert!(tman_descr.free_xp >= 50_000);

        let _ = std::fs::remove_dir_all(&data_dir);
    }

    #[test]
    fn account_damage_all_sets_repair_cost_and_hp() {
        let Some(game_dir) = test_game_dir() else {
            eprintln!("skip: retail game files absent");
            return;
        };
        let data_dir = test_temp_dir("acct-cli-damage");

        execute_reset(&ResetArgs {
            username: "breaker".into(),
            preset: "retail".into(),
            data_dir: data_dir.clone(),
            game_dir: game_dir.clone(),
            economics_config: None,
        })
        .expect("reset retail");

        let dmg_out = execute_damage(&DamageArgs {
            username: "breaker".into(),
            veh_inv_id: None,
            all: true,
            hp_fraction: 0.5,
            data_dir: data_dir.clone(),
            game_dir: game_dir.clone(),
        })
        .expect("damage all vehicles");
        assert!(dmg_out.contains("damaged"));

        let cat = catalog::load(&game_dir).unwrap();
        let store = JsonAccountStore::open(&data_dir).unwrap();
        let profile = store.load_required("breaker").unwrap();
        assert_eq!(profile.vehicles.len(), 3);
        for veh in profile.vehicles.values() {
            let (cost, hp) = veh.repair.expect("repair tuple must be populated");
            let descr = VehicleDescr::from_bytes(&veh.comp_descr).unwrap();
            let def = cat.vehicle_def_by_id(descr.nation_id, descr.type_id as u16).unwrap();
            let turret = def.turrets.iter().find(|t| t.component.id == descr.turret_id).unwrap();
            let max_hp = def.hull_max_health + turret.max_health;
            let expected_hp = ((max_hp as f64) * 0.5).round() as i32;
            let expected_cost = (((max_hp - expected_hp) as f64) * def.repair_cost).ceil() as i32;
            assert_eq!(hp, expected_hp);
            assert_eq!(cost, expected_cost);
        }

        let _ = std::fs::remove_dir_all(&data_dir);
    }

    #[test]
    fn account_show_missing_fails_cleanly() {
        let data_dir = test_temp_dir("acct-cli-missing");
        let err = execute_show(&ShowArgs {
            username: "ghost_player".into(),
            data_dir: data_dir.clone(),
        })
        .unwrap_err();
        assert!(
            err.to_string().contains("not found") || err.to_string().contains("ghost_player"),
            "unexpected error: {err}"
        );
        let _ = std::fs::remove_dir_all(&data_dir);
    }

    #[test]
    fn account_grant_rejects_missing_flags_or_invalid_veh_spec() {
        let Some(game_dir) = test_game_dir() else {
            return;
        };
        let data_dir = test_temp_dir("acct-cli-invalid-grant");
        execute_reset(&ResetArgs {
            username: "tester".into(),
            preset: "retail".into(),
            data_dir: data_dir.clone(),
            game_dir: game_dir.clone(),
            economics_config: None,
        })
        .expect("reset retail");

        let empty_err = execute_grant(&GrantArgs {
            username: "tester".into(),
            credits: None,
            gold: None,
            free_xp: None,
            veh_xp: vec![],
            tman_xp: vec![],
            unlock_all: false,
            data_dir: data_dir.clone(),
            game_dir: game_dir.clone(),
            economics_config: None,
        })
        .unwrap_err();
        assert!(
            empty_err.to_string().contains("nothing to grant"),
            "unexpected error for empty grant flags: {empty_err}"
        );

        let spec_err = execute_grant(&GrantArgs {
            username: "tester".into(),
            credits: None,
            gold: None,
            free_xp: None,
            veh_xp: vec!["not_a_spec".into()],
            tman_xp: vec![],
            unlock_all: false,
            data_dir: data_dir.clone(),
            game_dir,
            economics_config: None,
        })
        .unwrap_err();
        assert!(
            spec_err.to_string().contains("invalid"),
            "unexpected error for bad veh-xp spec: {spec_err}"
        );
        let _ = std::fs::remove_dir_all(&data_dir);
    }

    #[test]
    fn account_damage_single_vehicle_and_missing_target_errors() {
        let Some(game_dir) = test_game_dir() else {
            return;
        };
        let data_dir = test_temp_dir("acct-cli-dmg-single");
        execute_reset(&ResetArgs {
            username: "solo".into(),
            preset: "retail".into(),
            data_dir: data_dir.clone(),
            game_dir: game_dir.clone(),
            economics_config: None,
        })
        .expect("reset retail");

        let store = JsonAccountStore::open(&data_dir).unwrap();
        let initial = store.load_required("solo").unwrap();
        let first_inv_id = *initial.vehicles.keys().next().unwrap();

        let no_target_err = execute_damage(&DamageArgs {
            username: "solo".into(),
            veh_inv_id: None,
            all: false,
            hp_fraction: 0.5,
            data_dir: data_dir.clone(),
            game_dir: game_dir.clone(),
        })
        .unwrap_err();
        assert!(no_target_err.to_string().contains("--veh-inv-id"));

        execute_damage(&DamageArgs {
            username: "solo".into(),
            veh_inv_id: Some(first_inv_id),
            all: false,
            hp_fraction: 0.25,
            data_dir: data_dir.clone(),
            game_dir,
        })
        .expect("damage single vehicle");

        let updated = store.load_required("solo").unwrap();
        assert!(updated.vehicles[&first_inv_id].repair.is_some());
        let undamaged = updated
            .vehicles
            .iter()
            .filter(|(id, v)| **id != first_inv_id && v.repair.is_none())
            .count();
        assert_eq!(undamaged, 2);

        let _ = std::fs::remove_dir_all(&data_dir);
    }
}
