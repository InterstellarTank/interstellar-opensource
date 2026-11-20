//! Account payload builders and file-override loading for `wot-baseapp`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use wot065_data::account::{
    build_showgui_ctx_ext, dossier_sync_pickle, full_sync_pickle, stream_bundle, FullSyncOptions,
};
use wot065_data::account_state::build_shop_cache;
use wot065_data::catalog::{self, Catalog};
use wot065_data::economics::EconomicsConfig;
use wot065_data::pickle::{self, Value};

/// Chunk size per `ClientInterface::resourceFragment` message (bytes).
pub const RESOURCE_CHUNK_SIZE: usize = 1024;

/// Optional overrides for pre-built payloads (typically generated offline in `.scratch`).
#[derive(Debug, Clone, Default)]
pub struct PayloadOverrides {
    pub account_diff: Option<PathBuf>,
    pub showgui_ctx: Option<PathBuf>,
    pub shop_cache: Option<PathBuf>,
    pub dossier_sync: Option<PathBuf>,
}

/// Pre-encoded account payloads served to connecting clients.
#[derive(Debug, Clone)]
pub struct AccountPayloads {
    pub vehicle_count: usize,
    pub full_sync_pickle: Vec<u8>,
    pub full_sync_stream: (Vec<u8>, Vec<u8>),
    pub shop_cache_stream: (Vec<u8>, Vec<u8>),
    pub dossier_sync_stream: (Vec<u8>, Vec<u8>),
    pub showgui_ctx_override: Option<Vec<u8>>,
    pub server_settings_pickle: Vec<u8>,
    pub catalog: Arc<Catalog>,
    pub economics: Arc<EconomicsConfig>,
    pub overrides: PayloadOverrides,
}

fn read_bytes(path: &Path, label: &str) -> anyhow::Result<Vec<u8>> {
    std::fs::read(path)
        .with_context(|| format!("failed to read {label} override {}", path.display()))
}

fn load_pickle_override(path: &Path, label: &str) -> anyhow::Result<Vec<u8>> {
    let raw = read_bytes(path, label)?;
    pickle::loads(&raw).with_context(|| format!("invalid pickle in {}", path.display()))?;
    Ok(raw)
}

fn validate_compressed_stream(
    bytes: Vec<u8>,
    path: &Path,
    label: &str,
) -> anyhow::Result<(Vec<u8>, Vec<u8>)> {
    pickle::zlib_decompress(&bytes)
        .with_context(|| format!("invalid zlib payload in {label} ({})", path.display()))?;
    let desc = pickle::stream_description(&bytes);
    Ok((bytes, desc))
}

impl AccountPayloads {
    /// Default BaseApp `Account.serverSettings` pickle required by retail `Account.py:75`
    /// (`self.serverSettings['vivoxDomain']` expects `''` when Vivox is disabled).
    pub fn default_server_settings_pickle() -> Vec<u8> {
        pickle::dumps(&Value::dict(vec![(
            Value::str("vivoxDomain"),
            Value::str(b""),
        )]))
    }

    /// Load the vehicle catalog from `game_dir` and build all BaseApp payloads,
    /// applying any explicit file overrides.
    pub fn load(game_dir: &Path, overrides: &PayloadOverrides) -> anyhow::Result<Self> {
        Self::load_with_economics(game_dir, overrides, None)
    }

    /// Load catalog and optional custom economics config.
    pub fn load_with_economics(
        game_dir: &Path,
        overrides: &PayloadOverrides,
        economics_path: Option<&Path>,
    ) -> anyhow::Result<Self> {
        let catalog = Arc::new(catalog::load(game_dir).with_context(|| {
            format!(
                "failed to load vehicle catalog from {}",
                game_dir.display()
            )
        })?);
        let economics = Arc::new(if let Some(path) = economics_path {
            EconomicsConfig::load_from_file(path)
                .with_context(|| format!("failed to load economics config {}", path.display()))?
        } else {
            EconomicsConfig::default()
        });
        let vehicle_count = catalog.count;

        let full_sync_pickle = match &overrides.account_diff {
            Some(path) => load_pickle_override(path, "account-diff")?,
            None => full_sync_pickle(
                &catalog,
                &FullSyncOptions {
                    unlock_all: true,
                    ..FullSyncOptions::default()
                },
            ),
        };
        let full_sync_stream = stream_bundle(&full_sync_pickle);

        let shop_cache_stream = match &overrides.shop_cache {
            Some(path) => validate_compressed_stream(
                read_bytes(path, "shop-cache")?,
                path,
                "shop-cache",
            )?,
            None => stream_bundle(&pickle::dumps(&build_shop_cache(&catalog, &economics, 1))),
        };

        let dossier_sync_stream = match &overrides.dossier_sync {
            Some(path) => validate_compressed_stream(
                read_bytes(path, "dossier-sync")?,
                path,
                "dossier-sync",
            )?,
            None => stream_bundle(&dossier_sync_pickle(0)),
        };

        let showgui_ctx_override = match &overrides.showgui_ctx {
            Some(path) => Some(load_pickle_override(path, "showgui-ctx")?),
            None => None,
        };

        Ok(Self {
            vehicle_count,
            full_sync_pickle,
            full_sync_stream,
            shop_cache_stream,
            dossier_sync_stream,
            showgui_ctx_override,
            server_settings_pickle: Self::default_server_settings_pickle(),
            catalog,
            economics,
            overrides: overrides.clone(),
        })
    }

    /// Build `showGUI(ctx)` payload using override bytes if supplied, otherwise a live timestamp.
    pub fn showgui_ctx_bytes(&self, database_id: i64, now_utc: i64) -> Vec<u8> {
        match &self.showgui_ctx_override {
            Some(bytes) => bytes.clone(),
            None => pickle::dumps(&build_showgui_ctx_ext(database_id, now_utc, now_utc)),
        }
    }

    /// Return current UTC seconds since Unix epoch.
    pub fn unix_now() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(1_700_000_000)
    }

    /// Incremental no-op sync diff `{'rev': rev, 'prevRev': rev}` for follow-up `CMD_SYNC_DATA`
    /// polls once client revision > 0 so `Account._update` does not wipe the cache.
    pub fn noop_delta_pickle(rev: i64) -> Vec<u8> {
        pickle::dumps(&Value::dict(vec![
            (Value::str("rev"), Value::Int(rev)),
            (Value::str("prevRev"), Value::Int(rev)),
        ]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noop_delta_has_matching_rev_and_prev_rev() {
        let bytes = AccountPayloads::noop_delta_pickle(1);
        let val = pickle::loads(&bytes).expect("valid protocol-2 pickle");
        assert_eq!(val.get("rev").and_then(|v| v.as_int()), Some(1));
        assert_eq!(val.get("prevRev").and_then(|v| v.as_int()), Some(1));
    }

    #[test]
    fn default_server_settings_includes_vivox_domain() {
        let bytes = AccountPayloads::default_server_settings_pickle();
        let val = pickle::loads(&bytes).expect("valid protocol-2 pickle");
        assert_eq!(
            val.get("vivoxDomain").and_then(|v| v.as_bytes()),
            Some(&b""[..])
        );
    }

    #[test]
    fn loads_live_catalog_and_builds_all_streams() {
        let game_dir = Path::new(r"C:\Users\qwerty\Desktop\World_of_Tanks");
        if !game_dir.join("res").exists() {
            return;
        }
        let payloads = AccountPayloads::load(game_dir, &PayloadOverrides::default())
            .expect("load catalog payloads");
        assert_eq!(payloads.vehicle_count, 130);
        let parsed_sync = pickle::loads(&payloads.full_sync_pickle).unwrap();
        let vehs = parsed_sync
            .get("inventory")
            .unwrap()
            .get_int(1)
            .unwrap()
            .get("compDescr")
            .unwrap();
        assert_eq!(
            match vehs {
                Value::Dict(items) => items.len(),
                _ => 0,
            },
            130
        );
        let gui = pickle::loads(&payloads.showgui_ctx_bytes(1, 1_700_000_000)).unwrap();
        assert_eq!(gui.get("databaseID").and_then(|v| v.as_int()), Some(1));
        let settings = pickle::loads(&payloads.server_settings_pickle).unwrap();
        assert_eq!(
            settings.get("vivoxDomain").and_then(|v| v.as_bytes()),
            Some(&b""[..])
        );
    }

    #[test]
    fn shop_cache_respects_custom_economics_config() {
        let game_dir = Path::new(r"C:\Users\qwerty\Desktop\World_of_Tanks");
        if !game_dir.join("res").exists() {
            return;
        }
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-tmp")
            .join(format!("shop-econ-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let econ_path = dir.join("custom_econ.json");
        let mut custom = EconomicsConfig::default();
        custom.exchange_rate = 777;
        custom.slots_prices = (6, vec![450]);
        std::fs::write(&econ_path, serde_json::to_string_pretty(&custom).unwrap()).unwrap();

        let payloads = AccountPayloads::load_with_economics(
            game_dir,
            &PayloadOverrides::default(),
            Some(&econ_path),
        )
        .expect("load payloads with custom economics");

        let (compressed, _) = &payloads.shop_cache_stream;
        let shop = pickle::loads(&pickle::zlib_decompress(compressed).unwrap()).unwrap();
        assert_eq!(
            shop.get("exchangeRate").and_then(|v| v.as_int()),
            Some(777)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
