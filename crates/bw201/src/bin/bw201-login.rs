//! Standalone BigWorld 2.0.1 UDP LoginApp server / datagram dumper.

use std::net::{SocketAddr, SocketAddrV4};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use clap::Parser;
use tracing_subscriber::EnvFilter;

use bw201::crypto::LoginKey;
use bw201::loginapp::{dump_datagram, LoginApp, LoginAppConfig};

#[derive(Debug, Parser)]
#[command(
    name = "bw201-login",
    about = "BigWorld 2.0.1 / WoT 0.6.5 UDP LoginApp"
)]
struct Args {
    /// UDP address to bind (client `scripts_config.xml` points here).
    #[arg(long, default_value = "127.0.0.1:20016")]
    bind: SocketAddr,

    /// BaseApp IPv4 endpoint returned in the Blowfish-encrypted LOGGED_ON reply.
    #[arg(long, default_value = "127.0.0.1:20017")]
    base_app: SocketAddrV4,

    /// Path to the RSA private key PEM (PKCS#8) matching client `loginapp_wot.pubkey`.
    #[arg(long)]
    priv_key: PathBuf,

    /// Write the negotiated BaseApp session JSON atomically here on successful login.
    #[arg(long)]
    session_out: Option<PathBuf>,

    /// Session key handed to both client and BaseApp (decimal or `0x` hex).
    #[arg(long, default_value = "0x0BADF00D", value_parser = parse_u32_radices)]
    session_key: u32,

    /// Optional display label for log messages (e.g. player name).
    #[arg(long)]
    name: Option<String>,

    /// Decode a captured raw login datagram file and exit instead of serving.
    #[arg(long)]
    dump_request: Option<PathBuf>,

    /// Enable debug-level packet and hexdump logging.
    #[arg(short, long)]
    verbose: bool,
}

fn parse_u32_radices(s: &str) -> Result<u32, String> {
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u32::from_str_radix(hex, 16).map_err(|e| e.to_string())
    } else {
        s.parse::<u32>().map_err(|e| e.to_string())
    }
}

fn run() -> Result<(), String> {
    let args = Args::parse();
    let default_filter = if args.verbose { "debug" } else { "info" };
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_filter));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let pem = std::fs::read_to_string(&args.priv_key)
        .map_err(|e| format!("failed to read private key {}: {e}", args.priv_key.display()))?;
    let key = LoginKey::from_pkcs8_pem(&pem)
        .map_err(|e| format!("failed to load RSA private key {}: {e}", args.priv_key.display()))?;

    if let Some(dump_path) = &args.dump_request {
        let data = std::fs::read(dump_path)
            .map_err(|e| format!("failed to read datagram {}: {e}", dump_path.display()))?;
        let summary = dump_datagram(&key, &data)?;
        print!("{summary}");
        return Ok(());
    }

    let config = LoginAppConfig {
        bind: args.bind,
        base_app: args.base_app,
        session_key: args.session_key,
        session_out: args.session_out,
    };
    let mut app = LoginApp::bind(config, key)
        .map_err(|e| format!("failed to bind LoginApp on {}: {e}", args.bind))?;
    let bound = app.local_addr().map_err(|e| e.to_string())?;
    tracing::info!(
        bind = %bound,
        base_app = %args.base_app,
        label = args.name.as_deref().unwrap_or("default"),
        "bw201-login ready"
    );
    app.run(Arc::new(AtomicBool::new(false)))
        .map_err(|e| format!("LoginApp loop failed: {e}"))
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
