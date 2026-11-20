//! Standalone `wot-baseapp` UDP server binary.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use clap::Parser;
use tracing_subscriber::EnvFilter;
use wot065_data::account_state::Preset;
use wot_baseapp::payloads::PayloadOverrides;
use wot_baseapp::{BaseAppConfig, BattleHostConfig};
use wot_battle::balance::BalanceConfig;

#[derive(Debug, Parser)]
#[command(name = "wot-baseapp", about = "World of Tanks 0.6.5 UDP BaseApp server")]
struct Args {
    /// UDP socket address to bind on (must match `--base-app` on `bw201-login`).
    #[arg(long, default_value = "127.0.0.1:20017")]
    bind: SocketAddr,

    /// JSON session handoff file written by `bw201-login`.
    #[arg(long)]
    session_file: PathBuf,

    /// Path to WoT 0.6.5 game directory (`res/scripts/item_defs/...`).
    #[arg(long)]
    game_dir: PathBuf,

    /// Account storage directory for JSON profiles.
    #[arg(long, default_value = ".scratch/accounts")]
    data_dir: PathBuf,

    /// Initial account preset when creating fresh profiles (retail or sandbox).
    #[arg(long, default_value = "retail")]
    preset: String,

    /// Optional path to custom economics config file.
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

    /// Optional override for full account sync diff (raw protocol-2 pickle bytes).
    #[arg(long)]
    account_diff: Option<PathBuf>,

    /// Optional override for showGUI ctx (raw protocol-2 pickle bytes).
    #[arg(long)]
    showgui_ctx: Option<PathBuf>,

    /// Optional override for shop cache stream payload (zlib-compressed pickle).
    #[arg(long)]
    shop_cache: Option<PathBuf>,

    /// Optional override for dossier sync stream payload (zlib-compressed pickle).
    #[arg(long)]
    dossier_sync: Option<PathBuf>,

    /// Enable debug-level tracing logs.
    #[arg(long)]
    verbose: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let filter = if args.verbose {
        EnvFilter::new("debug")
    } else {
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"))
    };
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let preset = parse_preset(&args.preset)?;

    let cfg = BaseAppConfig {
        bind: args.bind,
        session_file: args.session_file,
        game_dir: args.game_dir,
        data_dir: args.data_dir,
        preset,
        economics_config: args.economics_config,
        overrides: PayloadOverrides {
            account_diff: args.account_diff,
            showgui_ctx: args.showgui_ctx,
            shop_cache: args.shop_cache,
            dossier_sync: args.dossier_sync,
        },
    };
    let battle = BattleHostConfig {
        team_size: usize::from(args.team_size),
        queue_wait_timeout: Duration::from_secs(args.queue_wait_secs),
        prebattle_countdown_secs: args.prebattle_countdown_secs,
        dev_features: args.dev_features,
        balance_config: BalanceConfig::load_or_default(args.battle_config.as_deref())?,
        ..BattleHostConfig::default()
    };
    wot_baseapp::serve_with_battle_and_ready(cfg, battle, None).await
}

fn parse_preset(raw: &str) -> anyhow::Result<Preset> {
    Preset::from_str(raw).map_err(anyhow::Error::msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_preset_flag_returns_err() {
        assert_eq!(parse_preset("retail").unwrap(), Preset::Retail);
        assert_eq!(parse_preset("sandbox").unwrap(), Preset::Sandbox);
        assert!(parse_preset("bogus_preset").is_err(), "invalid preset must error");
    }
}
