mod scenario;

use std::fs;
use std::net::SocketAddrV4;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, ensure, Context, Result};
use bw201::stream::BinaryReader;
use clap::{Parser, Subcommand};
use rsa::{pkcs1::DecodeRsaPublicKey, pkcs8::DecodePublicKey, RsaPublicKey};
use serde::Serialize;
use wot065_data::descr::VehicleDescr;
use wot065_data::dossier::AccountDossier;
use wot065_data::pickle::Value;
use wot065_data::xml::read_xml;
use wot_baseapp::protocol::{client_method, cmd};
use wot_battle::world::ARENA_PERIOD_BATTLE;
use wot_client_emulator::{dossier_count, BattleResult, Client, CommandArgs, Phase, WireStats};

use scenario::Scenario;

#[derive(Parser)]
#[command(about = "Headless World of Tanks 0.6.5 client for a development server")]
struct Args {
    /// LoginApp UDP address.
    #[arg(long, global = true, default_value = "127.0.0.1:20016")]
    login: SocketAddrV4,
    /// Installed 0.6.5 client. Reads its public key and validates resource mappings.
    #[arg(long, global = true)]
    game_dir: Option<PathBuf>,
    /// LoginApp public key PEM. Overrides the key in --game-dir.
    #[arg(long, global = true)]
    pub_key: Option<PathBuf>,
    /// Existing or new test account. Defaults to a fresh account per run.
    #[arg(long, global = true)]
    username: Option<String>,
    /// Run the test this many times.
    #[arg(long, global = true, default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..=1000))]
    repeat: u32,
    /// Maximum seconds per connection or scenario wait.
    #[arg(long, global = true, default_value_t = 60, value_parser = clap::value_parser!(u64).range(1..=3600))]
    timeout: u64,
    /// Print one JSON report per successful run.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    mode: Option<Mode>,
}

#[derive(Subcommand)]
enum Mode {
    /// Check LoginApp, BaseApp, Account, inventory, shop, dossiers and server stats.
    Hangar,
    /// Enter a Random battle, drive and shoot, then return to the hangar.
    Battle {
        /// Arena type ID. Zero lets the matchmaker choose a map.
        #[arg(long, default_value_t = 0)]
        map: i32,
        /// Owned vehicle inventory ID. Defaults to the first unlocked vehicle with ammo.
        #[arg(long)]
        vehicle: Option<i32>,
        /// Stay through the battle result and check the persisted account dossier.
        #[arg(long)]
        wait_for_results: bool,
    },
    /// Run a JSON scenario against a live server.
    Scenario { file: PathBuf },
}

#[derive(Serialize)]
struct Report<'a> {
    username: &'a str,
    base_app: SocketAddrV4,
    duration_ms: u128,
    hangar: Option<HangarReport>,
    battle: Option<BattleReport>,
    scenario_steps: Option<usize>,
    wire: WireStats,
}

#[derive(Serialize)]
struct HangarReport {
    account_id: i64,
    account_revision: i64,
    vehicles: usize,
    shop_revision: i64,
    dossier_records: usize,
    online_players: u32,
}

#[derive(Serialize)]
struct BattleReport {
    map_id: i32,
    geometry: String,
    vehicle_inventory_id: i32,
    own_snapshots: u64,
    forward_meters: f32,
    own_shots: u64,
    result: Option<BattleResult>,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let mode = args.mode.as_ref().unwrap_or(&Mode::Hangar);
    let scenario = match mode {
        Mode::Scenario { file } => Some(Scenario::read(file)?),
        _ => None,
    };
    let key = load_public_key(args.game_dir.as_deref(), args.pub_key.as_deref())?;
    let password = std::env::var("WOT_EMULATOR_PASSWORD").unwrap_or_else(|_| "emulator".into());
    let run_id: u32 = rand::random();
    for iteration in 1..=args.repeat {
        let username = args
            .username
            .clone()
            .unwrap_or_else(|| format!("emulator-{run_id:08x}-{iteration}"));
        run_once(&args, mode, scenario.as_ref(), &key, &username, &password)
            .with_context(|| format!("run {iteration}/{} for {username}", args.repeat))?;
    }
    Ok(())
}

fn run_once(
    args: &Args,
    mode: &Mode,
    scenario: Option<&Scenario>,
    key: &RsaPublicKey,
    username: &str,
    password: &str,
) -> Result<()> {
    let start = Instant::now();
    let timeout = Duration::from_secs(args.timeout);
    let mut client = Client::connect(args.login, key, username, password, timeout)?;
    let (hangar, battle, scenario_steps) = match mode {
        Mode::Hangar => (Some(check_hangar(&mut client, timeout)?), None, None),
        Mode::Battle {
            map,
            vehicle,
            wait_for_results,
        } => {
            let before = check_hangar(&mut client, timeout)?;
            let battle = check_battle(
                &mut client,
                *map,
                *vehicle,
                *wait_for_results,
                args.game_dir.as_deref(),
                timeout,
            )?;
            let after = check_hangar(&mut client, timeout).context("hangar after battle")?;
            ensure!(
                before.account_id == after.account_id,
                "battle returned a different account"
            );
            ensure!(
                after.account_revision >= before.account_revision,
                "account revision went backwards after battle"
            );
            (Some(after), Some(battle), None)
        }
        Mode::Scenario { .. } => {
            let steps = scenario
                .context("scenario is missing")?
                .run(&mut client, timeout)?;
            (None, None, Some(steps))
        }
    };
    client.disconnect().context("disconnect from BaseApp")?;
    let report = Report {
        username,
        base_app: client.base_addr(),
        duration_ms: start.elapsed().as_millis(),
        hangar,
        battle,
        scenario_steps,
        wire: client.wire_stats(),
    };
    print_report(&report, args.json)
}

fn load_public_key(game_dir: Option<&Path>, override_path: Option<&Path>) -> Result<RsaPublicKey> {
    if let Some(game_dir) = game_dir {
        let version = read_xml(&game_dir.join("version.xml"))?;
        ensure!(
            version
                .child("version")
                .is_some_and(|version| version.text() == "v.0.6.5"),
            "{} is not a 0.6.5 client directory",
            game_dir.display()
        );
        let entities = read_xml(&game_dir.join("res/scripts/entities.xml"))?;
        for entity in ["Account", "Avatar", "Vehicle"] {
            ensure!(
                entities.child(entity).is_some(),
                "client entities.xml is missing {entity}"
            );
        }
    }
    let key_path = override_path
        .map(Path::to_path_buf)
        .or_else(|| game_dir.map(|dir| dir.join("res/loginapp_wot.pubkey")))
        .context("provide --game-dir or --pub-key for the local LoginApp")?;
    let pem = fs::read_to_string(&key_path)
        .with_context(|| format!("read public key {}", key_path.display()))?;
    RsaPublicKey::from_public_key_pem(&pem)
        .or_else(|_| RsaPublicKey::from_pkcs1_pem(&pem))
        .with_context(|| format!("parse RSA public key {}", key_path.display()))
}

fn check_hangar(client: &mut Client, timeout: Duration) -> Result<HangarReport> {
    client.wait_for(timeout, "synchronized hangar", |client| {
        client.hangar_ready().then_some(())
    })?;
    let ctx = client.show_gui().context("missing Account.showGUI")?;
    let account_id = field_int(ctx, "databaseID")?;
    let account = client.account().context("missing Account snapshot")?;
    let account_revision = field_int(account, "rev")?;
    let descriptors = vehicle_descriptors(account)?;
    for (id, desc) in descriptors {
        let descriptor = desc
            .as_bytes()
            .context("vehicle descriptor is not a byte string")?;
        VehicleDescr::from_bytes(descriptor)
            .with_context(|| format!("invalid vehicle descriptor for {id:?}"))?;
    }
    let vehicles = descriptors.len();
    let shop_revision = field_int(client.shop().context("missing shop cache")?, "rev")?;
    let dossier_records = client
        .dossiers()
        .and_then(dossier_count)
        .context("invalid dossiers")?;

    let cursor = client.event_cursor();
    let reply = client.command(cmd::CMD_REQ_SERVER_STATS, CommandArgs::default(), timeout)?;
    ensure!(
        reply.code == cmd::RES_SUCCESS,
        "CMD_REQ_SERVER_STATS returned {}",
        reply.code
    );
    let online_players = client.wait_for(timeout, "receiveServerStats", |client| {
        client
            .events_since(cursor)
            .find(|event| event.id == client_method::RECEIVE_SERVER_STATS && event.player_method)
            .and_then(|event| event.payload_prefix.get(4..))
            .and_then(|payload| BinaryReader::new(payload).u32().ok())
    })?;
    ensure!(
        online_players >= 1,
        "receiveServerStats excludes the connected player"
    );

    let rev = i32::try_from(shop_revision).context("shop revision exceeds the wire range")?;
    let reply = client.command(cmd::CMD_SYNC_SHOP, CommandArgs::Int3([rev, 0, 0]), timeout)?;
    ensure!(
        reply.code == cmd::RES_CACHE,
        "CMD_SYNC_SHOP returned {} for installed revision {shop_revision}, expected RES_CACHE",
        reply.code
    );
    Ok(HangarReport {
        account_id,
        account_revision,
        vehicles,
        shop_revision,
        dossier_records,
        online_players,
    })
}

fn field_int(value: &Value, name: &str) -> Result<i64> {
    value
        .get(name)
        .and_then(Value::as_int)
        .with_context(|| format!("missing integer field {name}"))
}

fn vehicle_inventory(account: &Value) -> Result<&Value> {
    account
        .get("inventory")
        .and_then(|inventory| inventory.get_int(1))
        .context("missing vehicle inventory")
}

fn vehicle_descriptors(account: &Value) -> Result<&[(Value, Value)]> {
    vehicle_inventory(account)?
        .get("compDescr")
        .and_then(Value::as_dict)
        .context("missing vehicle compact descriptors")
}

fn choose_vehicle(account: &Value, requested: Option<i32>) -> Result<i32> {
    let inventory = vehicle_inventory(account)?;
    let descriptors = vehicle_descriptors(account)?;
    let usable = |id: i64| {
        let lock = inventory
            .get("lock")
            .and_then(|v| v.get_int(id))
            .and_then(Value::as_int);
        let shells = inventory
            .get("shells")
            .and_then(|v| v.get_int(id))
            .and_then(Value::as_list);
        lock.unwrap_or(0) == 0
            && shells.is_some_and(|v| {
                v.chunks_exact(2)
                    .any(|shell| shell[1].as_int().is_some_and(|amount| amount > 0))
            })
    };
    if let Some(requested) = requested {
        let id = i64::from(requested);
        ensure!(
            descriptors.iter().any(|(key, _)| key.as_int() == Some(id)) && usable(id),
            "vehicle {requested} is unavailable for a shot test (not owned, locked, or out of ammunition)"
        );
        return Ok(requested);
    }
    descriptors
        .iter()
        .filter_map(|(id, _)| id.as_int())
        .filter(|id| usable(*id))
        .filter_map(|id| i32::try_from(id).ok())
        .min()
        .context("no unlocked vehicle with ammunition")
}

fn check_battle(
    client: &mut Client,
    requested_map: i32,
    requested_vehicle: Option<i32>,
    wait_for_results: bool,
    game_dir: Option<&Path>,
    timeout: Duration,
) -> Result<BattleReport> {
    ensure!(requested_map >= 0, "arena type ID must be nonnegative");
    let account = client.account().context("missing Account for queueing")?;
    let vehicle = choose_vehicle(account, requested_vehicle)?;
    let battles_before = wait_for_results
        .then(|| count_battles(account))
        .transpose()?;
    let cursor = client.event_cursor();
    client.notify(
        cmd::CMD_ENQUEUE_FOR_ARENA,
        CommandArgs::Int3([vehicle, requested_map, 1]),
    )?;
    let queue_event = client.wait_for(timeout, "onEnqueued or queue failure", |client| {
        client
            .events_since(cursor)
            .find(|event| {
                event.phase == Phase::Hangar
                    && event.player_method
                    && matches!(
                        event.id,
                        client_method::ON_ENQUEUED
                            | client_method::ON_ENQUEUE_FAILURE
                            | client_method::ON_ARENA_JOIN_FAILURE
                    )
            })
            .map(|event| (event.id, event.payload_prefix.get(4).copied()))
    })?;
    ensure!(
        queue_event.0 == client_method::ON_ENQUEUED,
        "battle queue failed (callback {:#04x}, reason {:?})",
        queue_event.0,
        queue_event.1
    );

    let (map_id, geometry, vehicle_id) = client.wait_for(
        timeout,
        "arena mapping and own Vehicle creation",
        |client| {
            if let Some(event) = client.events_since(cursor).find(|event| {
                event.phase == Phase::Hangar
                    && event.player_method
                    && event.id == client_method::ON_ARENA_JOIN_FAILURE
            }) {
                return Some(Err(anyhow!(
                    "arena join failed (reason {:?})",
                    event.payload_prefix.get(4)
                )));
            }
            let arena = client.arena()?;
            if !arena.is_ready() {
                return None;
            }
            Some(Ok((
                arena.map_id,
                arena.geometry.clone()?,
                arena.vehicle_id?,
            )))
        },
    )??;
    ensure!(
        requested_map == 0 || requested_map == map_id,
        "requested arena type {requested_map}, server assigned {map_id}"
    );
    if let Some(game_dir) = game_dir {
        validate_geometry(game_dir, &geometry)?;
    }
    let (start_pose, snapshots) = client.wait_for(
        timeout,
        "Battle period and first own Vehicle snapshot",
        |client| {
            let arena = client.arena()?;
            if arena.period != Some(ARENA_PERIOD_BATTLE) || arena.own_snapshots == 0 {
                return None;
            }
            Some((arena.own_vehicle()?.pose, arena.own_snapshots))
        },
    )?;
    let (sin, cos) = start_pose.yaw.sin_cos();
    client.move_vehicle(1)?;
    let moved = client.wait_for(
        timeout.min(Duration::from_secs(6)),
        "server-confirmed forward movement",
        |client| {
            let arena = client.arena()?;
            let [x, _, z] = arena.own_vehicle()?.pose.position;
            let forward = (x - start_pose.position[0]) * sin + (z - start_pose.position[2]) * cos;
            (arena.own_snapshots >= snapshots + 2 && forward >= 0.5).then_some(forward)
        },
    );
    client.move_vehicle(0).context("release movement input")?;
    let forward_meters = moved?;

    let arena = client.arena().context("battle ended during movement")?;
    let last_shots = arena.own_shots;
    let pose = arena
        .own_vehicle()
        .context("own Vehicle left the area of interest")?
        .pose;
    let [x, y, z] = pose.position;
    client.aim([x + sin * 80.0, y + 2.0, z + cos * 80.0])?;
    client.shoot()?;
    let (own_shots, own_snapshots) = client.wait_for(
        timeout.min(Duration::from_secs(10)),
        "tracer from own Vehicle",
        |client| {
            let arena = client.arena()?;
            (arena.own_shots > last_shots).then_some((arena.own_shots, arena.own_snapshots))
        },
    )?;
    let result = if wait_for_results {
        let result = client.wait_for(
            timeout,
            "onVehicleLeftArena battle results",
            Client::battle_result,
        )?;
        ensure!(
            result.vehicle_inventory_id == vehicle
                && result.vehicle_id == vehicle_id
                && i32::try_from(result.map_id).ok() == Some(map_id)
                && result.regular
                && result.shots > 0,
            "result does not describe the tested Random battle: {result:?}"
        );
        Some(result)
    } else {
        None
    };
    if client.phase() == Phase::Arena {
        client.leave_arena()?;
    }
    client.wait_for(timeout, "return to synchronized hangar", |client| {
        client.hangar_ready().then_some(())
    })?;
    if let Some(before) = battles_before {
        let after = count_battles(client.account().context("missing Account after battle")?)?;
        ensure!(
            after > before,
            "battle result did not increment the account dossier ({before} → {after})"
        );
    }
    Ok(BattleReport {
        map_id,
        geometry,
        vehicle_inventory_id: vehicle,
        own_snapshots,
        forward_meters,
        own_shots,
        result,
    })
}

fn count_battles(account: &Value) -> Result<u32> {
    let bytes = account
        .get("stats")
        .and_then(|stats| stats.get("dossier"))
        .and_then(Value::as_bytes)
        .context("account stats are missing the dossier")?;
    Ok(AccountDossier::from_bytes(bytes)?.battles_count)
}

fn validate_geometry(game_dir: &Path, geometry: &str) -> Result<()> {
    let path = Path::new(geometry);
    let mut components = path.components();
    if !matches!(components.next(), Some(Component::Normal(root)) if root == "spaces")
        || !components.all(|component| matches!(component, Component::Normal(_)))
    {
        bail!("invalid arena geometry path {geometry:?}");
    }
    ensure!(
        game_dir.join("res").join(path).is_dir(),
        "client is missing arena geometry {geometry:?}"
    );
    Ok(())
}

fn print_report(report: &Report<'_>, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string(report)?);
        return Ok(());
    }
    println!(
        "{}  BaseApp {}  {} ms",
        report.username, report.base_app, report.duration_ms
    );
    if let Some(hangar) = &report.hangar {
        println!(
            "Hangar  account {} (rev {})  {} vehicles  shop rev {}  {} dossiers  {} online",
            hangar.account_id,
            hangar.account_revision,
            hangar.vehicles,
            hangar.shop_revision,
            hangar.dossier_records,
            hangar.online_players
        );
    }
    if let Some(battle) = &report.battle {
        println!(
            "Battle  {}  {} own snapshots  {:.2} m forward  {} own shots  returned to hangar",
            battle.geometry, battle.own_snapshots, battle.forward_meters, battle.own_shots
        );
        if let Some(result) = battle.result {
            println!(
                "Battle results  {} XP  {} credits  dossier verified",
                result.xp, result.credits
            );
        }
    }
    if let Some(steps) = report.scenario_steps {
        println!("Scenario  {steps} steps passed");
    }
    println!(
        "UDP  {} sent  {} received  {} retransmitted",
        report.wire.sent, report.wire.received, report.wire.retransmitted
    );
    Ok(())
}
