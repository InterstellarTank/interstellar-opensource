//! Rust server for World of Tanks 0.6.5.
//!
//! Subcommands:
//!   * `setup`          — one-shot client deployment: generate an RSA pair, patch the
//!                        client's packed `scripts_config.xml` to point at our login
//!                        app, install our public key.
//!   * `keys`           — only generate the RSA key pair.
//!   * `patch-client`   — only patch the client (needs an existing key pair).
//!   * `restore-client` — restore the original client files from `.orig` backups.
//!   * `check-client`   — show the client's current login endpoint / key state.
//!   * `serve`          — spawn the BigWorld login app (`bw201-login`) and base app
//!                        (`wot-baseapp`) and keep both alive (single-process supervisor).

mod account_cli;
mod deploy;

use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Child, Command as ProcessCommand, Stdio};
use std::str::FromStr;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use wot065_data::account_state::Preset;

#[derive(Debug, Parser)]
#[command(
    name = "wot-server",
    version,
    about = "Rust server for World of Tanks 0.6.5"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// One-shot client deployment: keys + patched config + installed public key.
    Setup(SetupArgs),
    /// Generate (or reuse) the login RSA key pair.
    Keys(KeysArgs),
    /// Patch the client's scripts_config.xml and install the public key.
    PatchClient(PatchClientArgs),
    /// Restore the client's original config and public key.
    RestoreClient(GameDirArgs),
    /// Show the client's current login endpoint and key state.
    CheckClient(CheckClientArgs),
    /// Run the login app + base app together (supervisor).
    Serve(ServeArgs),
    /// Inspect or mutate persisted offline account profiles between server runs.
    Account(account_cli::AccountArgs),
}

#[derive(Debug, clap::Args)]
struct SetupArgs {
    /// WoT 0.6.5 game directory (contains `res/`).
    #[arg(long)]
    game_dir: PathBuf,
    /// Directory where the RSA key pair lives / is created.
    #[arg(long, default_value = "keys")]
    keys_dir: PathBuf,
    /// Login app endpoint the client is pointed at.
    #[arg(long, default_value = deploy::DEFAULT_LOGIN_HOST)]
    login_host: String,
    /// Overwrite an existing key pair.
    #[arg(long)]
    force_keys: bool,
}

#[derive(Debug, clap::Args)]
struct KeysArgs {
    /// Directory where the RSA key pair lives / is created.
    #[arg(long, default_value = "keys")]
    keys_dir: PathBuf,
    /// Overwrite an existing key pair.
    #[arg(long)]
    force: bool,
}

#[derive(Debug, clap::Args)]
struct PatchClientArgs {
    /// WoT 0.6.5 game directory (contains `res/`).
    #[arg(long)]
    game_dir: PathBuf,
    /// Login app endpoint the client is pointed at.
    #[arg(long, default_value = deploy::DEFAULT_LOGIN_HOST)]
    login_host: String,
    /// Public key file (PEM) to install into the client.
    #[arg(long)]
    pubkey: PathBuf,
}

#[derive(Debug, clap::Args)]
struct GameDirArgs {
    /// WoT 0.6.5 game directory (contains `res/`).
    #[arg(long)]
    game_dir: PathBuf,
}

#[derive(Debug, clap::Args)]
struct CheckClientArgs {
    /// WoT 0.6.5 game directory (contains `res/`).
    #[arg(long)]
    game_dir: PathBuf,
    /// Our public key file (PEM), to compare against what the client uses.
    #[arg(long)]
    pubkey: Option<PathBuf>,
}

#[derive(Debug, clap::Args)]
struct ServeArgs {
    /// WoT 0.6.5 game directory, used to load entity/vehicle data.
    #[arg(long)]
    game_dir: PathBuf,
    /// Login app bind address.
    #[arg(long, default_value = "127.0.0.1:20016")]
    login_bind: String,
    /// Base app bind address.
    #[arg(long, default_value = deploy::DEFAULT_BASE_APP)]
    base_bind: String,
    /// RSA private key (PKCS#8 PEM) used to decrypt login requests.
    #[arg(long, default_value = ".scratch/wot-keys/loginapp_priv.pem")]
    priv_key: PathBuf,
    /// Login app executable (defaults to `bw201-login[.exe]` next to this binary).
    #[arg(long)]
    login_exe: Option<PathBuf>,
    /// Base app executable (defaults to `wot-baseapp[.exe]` next to this binary).
    #[arg(long)]
    base_exe: Option<PathBuf>,
    /// File where the login app publishes the session (Blowfish key + session key).
    #[arg(long)]
    session_file: Option<PathBuf>,
    /// Pickled full account-sync diff (raw pickle bytes).
    #[arg(long)]
    account_diff: Option<PathBuf>,
    /// Pickled showGUI ctx (raw pickle bytes).
    #[arg(long)]
    showgui_ctx: Option<PathBuf>,
    /// Shop cache stream payload, zlib-compressed pickle (RES_STREAM path).
    #[arg(long)]
    shop_cache: Option<PathBuf>,
    /// Dossier sync stream payload, zlib-compressed pickle (RES_STREAM path).
    #[arg(long)]
    dossier_sync: Option<PathBuf>,
    /// Directory for child logs (defaults to `.scratch/logs`).
    #[arg(long, default_value = ".scratch/logs")]
    log_dir: PathBuf,
    /// Initial account preset when creating fresh profiles (`retail` or `sandbox`).
    #[arg(long, default_value = "retail")]
    preset: String,
    /// Account storage directory for JSON profiles (defaults to `.scratch/accounts`).
    #[arg(long, default_value = ".scratch/accounts")]
    data_dir: PathBuf,
    /// Optional path to custom economics config JSON.
    #[arg(long)]
    economics_config: Option<PathBuf>,
    /// Number of vehicles per team in Random battles.
    #[arg(long, default_value_t = 7, value_parser = clap::value_parser!(u8).range(1..=15))]
    team_size: u8,
    /// Seconds the Random queue waits for other players before filling with bots.
    #[arg(long, default_value_t = 5)]
    queue_wait_secs: u64,
    /// Seconds between the last client becoming ready and the start of a Random battle.
    #[arg(long, default_value_t = 30)]
    prebattle_countdown_secs: u32,
    /// Enable developer battle commands (also enabled by the sandbox preset).
    #[arg(long)]
    dev_features: bool,
    /// Optional JSON override for battle balance.
    #[arg(long)]
    battle_config: Option<PathBuf>,
}

fn main() {
    let cli = Cli::parse();
    let code = match run(cli) {
        Ok(()) => 0,
        Err(err) => {
            eprintln!("error: {err:#}");
            1
        }
    };
    std::process::exit(code);
}

fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Setup(args) => {
            let (_privkey, pubkey) = deploy::generate_keys(&args.keys_dir, args.force_keys)?;
            deploy::patch_client(&args.game_dir, &args.login_host, &pubkey)?;
            println!();
            deploy::check_client(&args.game_dir, Some(&pubkey))?;
        }
        Command::Keys(args) => {
            let (privkey, pubkey) = deploy::generate_keys(&args.keys_dir, args.force)?;
            println!("private key: {}", privkey.display());
            println!("public key : {}", pubkey.display());
        }
        Command::PatchClient(args) => {
            deploy::patch_client(&args.game_dir, &args.login_host, &args.pubkey)?;
        }
        Command::RestoreClient(args) => {
            deploy::restore_client(&args.game_dir)?;
        }
        Command::CheckClient(args) => {
            deploy::check_client(&args.game_dir, args.pubkey.as_deref())?;
        }
        Command::Serve(args) => run_serve(args)?,
        Command::Account(args) => account_cli::run_account(args)?,
    }
    Ok(())
}

fn build_baseapp_args(args: &ServeArgs, session_file: &Path) -> Result<Vec<String>> {
    let _preset = Preset::from_str(&args.preset).map_err(anyhow::Error::msg)?;
    let mut base_args: Vec<String> = vec![
        "--bind".into(),
        args.base_bind.clone(),
        "--session-file".into(),
        session_file.display().to_string(),
        "--game-dir".into(),
        args.game_dir.display().to_string(),
        "--preset".into(),
        args.preset.clone(),
        "--data-dir".into(),
        args.data_dir.display().to_string(),
        "--team-size".into(),
        args.team_size.to_string(),
        "--queue-wait-secs".into(),
        args.queue_wait_secs.to_string(),
        "--prebattle-countdown-secs".into(),
        args.prebattle_countdown_secs.to_string(),
    ];
    if args.dev_features {
        base_args.push("--dev-features".into());
    }
    if let Some(path) = &args.battle_config {
        base_args.push("--battle-config".into());
        base_args.push(path.display().to_string());
    }
    if let Some(path) = args.economics_config.as_ref() {
        base_args.push("--economics-config".into());
        base_args.push(path.display().to_string());
    }
    if let Some(path) = args.account_diff.as_ref() {
        base_args.push("--account-diff".into());
        base_args.push(path.display().to_string());
    }
    if let Some(path) = args.showgui_ctx.as_ref() {
        base_args.push("--showgui-ctx".into());
        base_args.push(path.display().to_string());
    }
    if let Some(path) = args.shop_cache.as_ref() {
        base_args.push("--shop-cache".into());
        base_args.push(path.display().to_string());
    }
    if let Some(path) = args.dossier_sync.as_ref() {
        base_args.push("--dossier-sync".into());
        base_args.push(path.display().to_string());
    }
    Ok(base_args)
}

/// Resolve `name` in the directory of this executable (cargo target/release) or fail.
fn sibling_exe(name: &str) -> PathBuf {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("wot-server"));
    let dir = exe.parent().unwrap_or_else(|| Path::new("."));
    let file = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    dir.join(file)
}

fn spawn_child(
    name: &str,
    exe: &Path,
    args: &[String],
    log_dir: &Path,
) -> Result<Child> {
    if !exe.exists() {
        bail!(
            "{} binary not found at {} — build it first (cargo build --release) or pass its path",
            name,
            exe.display()
        );
    }
    std::fs::create_dir_all(log_dir)
        .with_context(|| format!("failed to create log dir {}", log_dir.display()))?;
    let out_path = log_dir.join(format!("{name}.out.log"));
    let err_path = log_dir.join(format!("{name}.err.log"));
    let out = File::create(&out_path)
        .with_context(|| format!("failed to create {}", out_path.display()))?;
    let err = File::create(&err_path)
        .with_context(|| format!("failed to create {}", err_path.display()))?;
    println!(
        "starting {name}: {} {}",
        exe.display(),
        args.join(" ")
    );
    println!("  logs: {} / {}", out_path.display(), err_path.display());
    ProcessCommand::new(exe)
        .args(args)
        .current_dir(std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
        .stdout(Stdio::from(out))
        .stderr(Stdio::from(err))
        .spawn()
        .with_context(|| format!("failed to start {name} ({})", exe.display()))
}

/// Resolve the session handoff path inside `--log-dir` by default so child
/// processes never fall back to OS `%TEMP%` (which breaks on Windows when
/// workspace-built binaries run at Low Integrity Level).
fn resolve_session_file(explicit: Option<&Path>, log_dir: &Path) -> PathBuf {
    explicit
        .map(Path::to_path_buf)
        .unwrap_or_else(|| log_dir.join("wot065-session.json"))
}

/// Remove any stale session file and verify the target directory is writable
/// before launching `bw201-login` and `wot-baseapp`.
fn prepare_session_file(session_file: &Path) -> Result<()> {
    if session_file.exists() {
        std::fs::remove_file(session_file).with_context(|| {
            format!(
                "failed to remove stale session handoff file {}",
                session_file.display()
            )
        })?;
    }
    if let Some(parent) = session_file.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!(
                    "session handoff parent {} could not be created (pass --session-file inside a writable directory)",
                    parent.display()
                )
            })?;
        }
    }
    let file_name = session_file
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "session.json".to_string());
    let mut probe = session_file.to_path_buf();
    probe.set_file_name(format!(".{file_name}.probe-{}", std::process::id()));
    match File::create(&probe) {
        Ok(file) => {
            drop(file);
            let _ = std::fs::remove_file(&probe);
            Ok(())
        }
        Err(err) => Err(err).with_context(|| {
            format!(
                "session handoff path {} is not writable (pass --session-file inside a writable directory)",
                session_file.display()
            )
        }),
    }
}

fn run_serve(args: ServeArgs) -> Result<()> {
    let login_exe = args
        .login_exe
        .clone()
        .unwrap_or_else(|| sibling_exe("bw201-login"));
    let base_exe = args
        .base_exe
        .clone()
        .unwrap_or_else(|| sibling_exe("wot-baseapp"));

    let session_file = resolve_session_file(args.session_file.as_deref(), &args.log_dir);
    let account_diff = args.account_diff.clone();
    let showgui_ctx = args.showgui_ctx.clone();
    let shop_cache = args.shop_cache.clone();
    let dossier_sync = args.dossier_sync.clone();

    if !args.priv_key.exists() {
        bail!(
            "RSA private key not found at {} — run `wot-server setup` or `wot-server keys` first",
            args.priv_key.display()
        );
    }
    prepare_session_file(&session_file)?;

    // Login app: accepts the client's login and writes the session (Blowfish key) for
    // the base app.
    let mut login_args: Vec<String> = vec![
        "--bind".into(),
        args.login_bind.clone(),
        "--base-app".into(),
        args.base_bind.clone(),
        "--priv-key".into(),
        args.priv_key.display().to_string(),
        "--session-out".into(),
        session_file.display().to_string(),
    ];
    login_args.push("--name".into());
    login_args.push("Player".into());

    // Base app: creates the Account entity and serves the garage data.
    let base_args = build_baseapp_args(&args, &session_file)?;

    let log_dir = args.log_dir.clone();
    let mut children: Vec<(&str, Child)> = Vec::new();
    children.push((
        "loginapp",
        spawn_child("bw201-login", &login_exe, &login_args, &log_dir)?,
    ));
    // give the login app a moment to bind
    std::thread::sleep(Duration::from_millis(500));
    children.push((
        "baseapp",
        spawn_child("wot-baseapp", &base_exe, &base_args, &log_dir)?,
    ));

    println!();
    println!("login app: udp {} (client scripts_config.xml points here)", args.login_bind);
    println!("base app : udp {} (address handed to the client by the login app)", args.base_bind);
    println!("session  : {}", session_file.display());
    println!("preset   : {} (account store: {})", args.preset, args.data_dir.display());
    if let Some(path) = args.economics_config.as_ref() {
        println!("economics: {}", path.display());
    }
    for (name, path) in [
        ("account diff", account_diff.as_ref()),
        ("showGUI ctx", showgui_ctx.as_ref()),
        ("shop cache", shop_cache.as_ref()),
        ("dossier sync", dossier_sync.as_ref()),
    ] {
        match path {
            Some(p) => println!("{name:<13}: {}", p.display()),
            None => println!("{name:<13}: <live AccountStore / generated from --game-dir>"),
        }
    }
    println!();
    println!("Press Ctrl+C to stop both apps.");

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to build tokio runtime")?;
    rt.block_on(async {
        let _ = tokio::signal::ctrl_c().await;
    });

    println!("\nshutting down...");
    for (name, child) in children.iter_mut() {
        let _ = child.kill();
        let status = child.wait();
        println!("{name} stopped: {:?}", status.map(|s| s.success()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn default_session_file_lives_in_log_dir() {
        let log_dir = Path::new(".scratch/logs");
        assert_eq!(
            resolve_session_file(None, log_dir),
            log_dir.join("wot065-session.json")
        );
        let custom = Path::new("custom/session.json");
        assert_eq!(resolve_session_file(Some(custom), log_dir), custom);
    }

    #[test]
    fn prepare_session_file_rejects_unwritable_parent() {
        let dir = test_temp_dir("wot-server-unwritable-session");
        let blocker = dir.join("not_a_dir");
        std::fs::write(&blocker, b"blocker").unwrap();

        let err = prepare_session_file(&blocker.join("session.json"))
            .expect_err("unwritable session parent must fail before spawning children");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            err.to_string().contains("session handoff"),
            "unexpected error message: {err:#}"
        );
    }

    #[test]
    fn prepare_session_file_removes_stale_file_and_leaves_no_probe() {
        let dir = test_temp_dir("wot-server-prepare-session");
        let session = dir.join("wot065-session.json");
        std::fs::write(&session, b"stale").unwrap();

        prepare_session_file(&session).expect("prepare writable session file");
        let session_exists = session.exists();
        let entries: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        let _ = std::fs::remove_dir_all(&dir);

        assert!(!session_exists, "stale session file must be removed");
        assert!(entries.is_empty(), "leftover files found: {entries:?}");
    }

    #[test]
    fn serve_forwards_preset_data_dir_and_economics_config() {
        let cli = Cli::try_parse_from([
            "wot-server",
            "serve",
            "--game-dir",
            r"C:\WoT",
            "--preset",
            "sandbox",
            "--data-dir",
            ".scratch/custom-accounts",
            "--economics-config",
            ".scratch/econ.json",
        ])
        .expect("parse serve flags");
        let Command::Serve(args) = cli.command else {
            panic!("expected Serve command");
        };
        let forwarded = build_baseapp_args(&args, Path::new(".scratch/logs/session.json"))
            .expect("build baseapp args");
        assert!(forwarded.windows(2).any(|w| w == ["--preset", "sandbox"]));
        assert!(forwarded
            .windows(2)
            .any(|w| w == ["--data-dir", ".scratch/custom-accounts"]));
        assert!(forwarded
            .windows(2)
            .any(|w| w == ["--economics-config", ".scratch/econ.json"]));
    }

    #[test]
    fn serve_forwards_battle_settings_and_rejects_bad_team_sizes() {
        let cli = Cli::try_parse_from([
            "wot-server", "serve", "--game-dir", "WoT", "--team-size", "1",
            "--queue-wait-secs", "2", "--prebattle-countdown-secs", "4", "--dev-features",
            "--battle-config", ".scratch/balance.json",
        ]).expect("parse battle options");
        let Command::Serve(args) = cli.command else { panic!("expected Serve command") };
        let flags = build_baseapp_args(&args, Path::new(".scratch/session.json")).unwrap();
        for pair in [
            ["--team-size", "1"], ["--queue-wait-secs", "2"],
            ["--prebattle-countdown-secs", "4"], ["--battle-config", ".scratch/balance.json"],
        ] {
            assert!(flags.windows(2).any(|arg| arg == pair));
        }
        assert!(flags.iter().any(|arg| arg == "--dev-features"));
        for invalid in ["0", "16"] {
            assert!(Cli::try_parse_from([
                "wot-server", "serve", "--game-dir", "WoT", "--team-size", invalid,
            ]).is_err());
        }
    }

    #[test]
    fn serve_rejects_invalid_preset() {
        let cli = Cli::try_parse_from([
            "wot-server",
            "serve",
            "--game-dir",
            r"C:\WoT",
            "--preset",
            "unknown_mode",
        ])
        .expect("parse flags");
        let Command::Serve(args) = cli.command else {
            panic!("expected Serve command");
        };
        assert!(build_baseapp_args(&args, Path::new(".scratch/logs/session.json")).is_err());
    }
}
