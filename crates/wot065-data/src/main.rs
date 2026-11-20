//! CLI for the WoT 0.6.5 account/garage data layer.
//!
//! ```text
//! wot065-data count   [--game-dir DIR]
//! wot065-data catalog --game-dir DIR --out vehicles_catalog.json
//! wot065-data diff    --game-dir DIR --out account_sync_full.pickle [--stream] [--unlock-all]
//! wot065-data gui     --out showgui_ctx.pickle [--database-id N] [--server-utc N]
//! wot065-data shop    --out shop_cache.pickle [--stream]
//! wot065-data dossier --out dossier_sync.pickle [--stream]
//! wot065-data all     --game-dir DIR --out-dir DIR
//! ```
//!
//! `diff`/`catalog` read the real game installation (packed BigWorld XML). `--stream`
//! makes `diff`/`shop`/`dossier` write the zlib-compressed resource-stream payload to
//! `--out` plus the pickled `(len, crc32)` stream description to `<out>.desc`, which is
//! what the baseapp needs for `RES_STREAM` responses (`client/SyncController.py:52-56`,
//! `client/game.py:151-157`).

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use wot065_data::account::{self, FullSyncOptions};
use wot065_data::catalog::{self, Catalog};
use wot065_data::pickle;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(command) = args.first().map(String::as_str) else {
        print_usage();
        return Err("missing command".into());
    };
    let opts = Options::parse(&args[1..])?;
    match command {
        "count" => cmd_count(&opts),
        "catalog" => cmd_catalog(&opts),
        "diff" => cmd_diff(&opts),
        "gui" => cmd_gui(&opts),
        "shop" => cmd_shop(&opts),
        "dossier" => cmd_dossier(&opts),
        "all" => cmd_all(&opts),
        "help" | "--help" | "-h" => {
            print_usage();
            Ok(())
        }
        other => Err(format!("unknown command {other:?} (try `help`)")),
    }
}

fn print_usage() {
    eprintln!(
        "wot065-data - WoT 0.6.5 account/garage data layer\n\
         \n\
         usage:\n  \
         wot065-data count   [--game-dir DIR]\n  \
         wot065-data catalog --game-dir DIR --out vehicles_catalog.json\n  \
         wot065-data diff    --game-dir DIR --out account_sync_full.pickle [--stream] [--unlock-all] [--rev N] [--prev-rev N]\n  \
         wot065-data gui     --out showgui_ctx.pickle [--database-id N] [--server-utc N]\n  \
         wot065-data shop    --out shop_cache.pickle [--stream]\n  \
         wot065-data dossier --out dossier_sync.pickle [--stream] [--version N]\n  \
         wot065-data all     --game-dir DIR --out-dir DIR [--unlock-all]\n\
         \n\
         --game-dir defaults to $WOT065_GAME_DIR or the local install location."
    );
}

#[derive(Default)]
struct Options {
    positionals: Vec<String>,
    values: std::collections::HashMap<String, String>,
    flags: std::collections::HashSet<String>,
}

impl Options {
    fn parse(args: &[String]) -> Result<Self, String> {
        let mut opts = Options::default();
        let mut i = 0;
        while i < args.len() {
            let arg = &args[i];
            if let Some(name) = arg.strip_prefix("--") {
                if let Some((key, value)) = name.split_once('=') {
                    opts.values.insert(key.to_string(), value.to_string());
                    i += 1;
                } else if i + 1 < args.len() && !args[i + 1].starts_with("--") {
                    opts.values.insert(name.to_string(), args[i + 1].clone());
                    i += 2;
                } else {
                    opts.flags.insert(name.to_string());
                    i += 1;
                }
            } else {
                opts.positionals.push(arg.clone());
                i += 1;
            }
        }
        Ok(opts)
    }

    fn value(&self, key: &str) -> Option<&str> {
        self.values
            .get(key)
            .map(String::as_str)
            .or_else(|| std::env::var(format!("WOT065_{}", key.to_uppercase().replace('-', "_"))).ok().map(|s| Box::leak(s.into_boxed_str()) as &str))
    }

    fn int(&self, key: &str, default: i64) -> Result<i64, String> {
        match self.values.get(key) {
            None => Ok(default),
            Some(value) => value
                .parse()
                .map_err(|_| format!("--{key} expects an integer, got {value:?}")),
        }
    }

    fn flag(&self, key: &str) -> bool {
        self.flags.contains(key)
    }

    fn game_dir(&self) -> Result<PathBuf, String> {
        if let Some(dir) = self.value("game-dir") {
            if !Path::new(dir).is_dir() {
                return Err(format!("--game-dir {dir:?} is not a directory"));
            }
            return Ok(PathBuf::from(dir));
        }
        wot065_data::testutil::game_dir()
            .ok_or_else(|| "no game directory: pass --game-dir <World_of_Tanks> or set $WOT065_GAME_DIR".to_string())
    }

    fn require_out(&self, what: &str) -> Result<PathBuf, String> {
        self.value("out")
            .map(PathBuf::from)
            .ok_or_else(|| format!("missing --out <file> for `{what}`"))
    }
}

fn load_catalog(opts: &Options) -> Result<Catalog, String> {
    let game_dir = opts.game_dir()?;
    let catalog = catalog::load(&game_dir).map_err(|e| e.to_string())?;
    if !catalog.problems.is_empty() {
        eprintln!(
            "warning: {} vehicle(s) skipped due to definition problems:",
            catalog.problems.len()
        );
        for problem in &catalog.problems {
            eprintln!("  - {problem}");
        }
    }
    Ok(catalog)
}

fn print_counts(catalog: &Catalog) {
    println!("vehicle catalog: {} types", catalog.len());
    for (nation, count) in catalog.counts_by_nation() {
        println!("  {nation:<8} {count:>3}");
    }
}

fn write_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("create {}: {e}", parent.display()))?;
        }
    }
    std::fs::write(path, bytes).map_err(|e| format!("write {}: {e}", path.display()))
}

fn cmd_count(opts: &Options) -> Result<(), String> {
    let catalog = load_catalog(opts)?;
    print_counts(&catalog);
    Ok(())
}

fn cmd_catalog(opts: &Options) -> Result<(), String> {
    let catalog = load_catalog(opts)?;
    let out = opts.require_out("catalog")?;
    let json = catalog::to_json(&catalog);
    write_file(&out, json.as_bytes())?;
    print_counts(&catalog);
    println!("catalog -> {} ({} bytes)", out.display(), json.len());
    Ok(())
}

fn now_utc() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn full_sync_options(opts: &Options) -> Result<FullSyncOptions, String> {
    Ok(FullSyncOptions {
        rev: opts.int("rev", 1)?,
        prev_rev: opts.int("prev-rev", 0)?,
        unlock_all: opts.flag("unlock-all"),
        ..FullSyncOptions::default()
    })
}

fn cmd_diff(opts: &Options) -> Result<(), String> {
    let catalog = load_catalog(opts)?;
    let options = full_sync_options(opts)?;
    let out = opts.require_out("diff")?;
    let pickled = account::full_sync_pickle(&catalog, &options);
    print_counts(&catalog);
    println!(
        "diff: {} vehicles as owned (rev {}, prevRev {}, unlock_all={})",
        catalog.len(),
        options.rev,
        options.prev_rev,
        options.unlock_all
    );
    if opts.flag("stream") {
        let (z, desc) = account::stream_bundle(&pickled);
        write_file(&out, &z)?;
        let desc_path = append_suffix(&out, ".desc");
        write_file(&desc_path, &desc)?;
        println!(
            "stream payload -> {} ({} bytes zlib; pickle {} bytes)",
            out.display(),
            z.len(),
            pickled.len()
        );
        println!(
            "stream description -> {} ({} bytes, crc32={})",
            desc_path.display(),
            desc.len(),
            pickle::signed_crc32(&z)
        );
    } else {
        write_file(&out, &pickled)?;
        println!("full-sync pickle -> {} ({} bytes)", out.display(), pickled.len());
    }
    Ok(())
}

fn cmd_gui(opts: &Options) -> Result<(), String> {
    let database_id = opts.int("database-id", 1)?;
    let server_utc = opts.int("server-utc", now_utc())?;
    let out = opts.require_out("gui")?;
    let pickled = account::showgui_ctx_pickle(database_id, server_utc);
    write_file(&out, &pickled)?;
    println!(
        "showGUI ctx -> {} ({} bytes, databaseID={}, serverUTC={})",
        out.display(),
        pickled.len(),
        database_id,
        server_utc
    );
    Ok(())
}

fn cmd_shop(opts: &Options) -> Result<(), String> {
    let out = opts.require_out("shop")?;
    let pickled = if opts.value("game-dir").is_some() || wot065_data::testutil::game_dir().is_some()
    {
        let catalog = load_catalog(opts)?;
        account::shop_cache_pickle_for_catalog(&catalog)
    } else {
        account::shop_cache_pickle()
    };
    if opts.flag("stream") {
        let (z, desc) = account::stream_bundle(&pickled);
        write_file(&out, &z)?;
        let desc_path = append_suffix(&out, ".desc");
        write_file(&desc_path, &desc)?;
        println!(
            "shop stream -> {} ({} bytes zlib; pickle {} bytes)",
            out.display(),
            z.len(),
            pickled.len()
        );
        println!("shop stream description -> {} ({} bytes)", desc_path.display(), desc.len());
    } else {
        write_file(&out, &pickled)?;
        println!("shop cache pickle -> {} ({} bytes)", out.display(), pickled.len());
    }
    Ok(())
}

fn cmd_dossier(opts: &Options) -> Result<(), String> {
    let out = opts.require_out("dossier")?;
    let version = opts.int("version", 0)?;
    let pickled = account::dossier_sync_pickle(version);
    if opts.flag("stream") {
        let (z, desc) = account::stream_bundle(&pickled);
        write_file(&out, &z)?;
        let desc_path = append_suffix(&out, ".desc");
        write_file(&desc_path, &desc)?;
        println!(
            "dossier stream -> {} ({} bytes zlib; pickle {} bytes)",
            out.display(),
            z.len(),
            pickled.len()
        );
        println!(
            "dossier stream description -> {} ({} bytes)",
            desc_path.display(),
            desc.len()
        );
    } else {
        write_file(&out, &pickled)?;
        println!("dossier pickle -> {} ({} bytes)", out.display(), pickled.len());
    }
    Ok(())
}

fn cmd_all(opts: &Options) -> Result<(), String> {
    let catalog = load_catalog(opts)?;
    let options = full_sync_options(opts)?;
    let out_dir = opts
        .value("out-dir")
        .map(PathBuf::from)
        .ok_or_else(|| "missing --out-dir <dir> for `all`".to_string())?;
    std::fs::create_dir_all(&out_dir).map_err(|e| format!("create {}: {e}", out_dir.display()))?;
    print_counts(&catalog);
    println!("out dir: {}", out_dir.display());

    // Catalog JSON (same schema as the research reference, so it can be diffed against it).
    let json = catalog::to_json(&catalog);
    write_file(&out_dir.join("vehicles_catalog.json"), json.as_bytes())?;
    println!("  vehicles_catalog.json {:>7} bytes", json.len());

    // Account sync diff: raw pickle, zlib stream payload, stream description.
    let pickled = account::full_sync_pickle(&catalog, &options);
    let (z, desc) = account::stream_bundle(&pickled);
    write_file(&out_dir.join("account_sync_full.pickle"), &pickled)?;
    write_file(&out_dir.join("account_sync_full.pickle.z"), &z)?;
    write_file(&out_dir.join("account_sync_full.desc"), &desc)?;
    println!(
        "  account_sync_full.pickle {:>6} bytes (zlib {} bytes, crc32 {})",
        pickled.len(),
        z.len(),
        pickle::signed_crc32(&z)
    );

    // showGUI ctx (raw pickle; not compressed).
    let gui = account::showgui_ctx_pickle(opts.int("database-id", 1)?, now_utc());
    write_file(&out_dir.join("showgui_ctx.pickle"), &gui)?;
    println!("  showgui_ctx.pickle {:>10} bytes", gui.len());

    // Shop cache (stream only in the client).
    let shop = account::shop_cache_pickle_for_catalog(&catalog);
    let (shop_z, shop_desc) = account::stream_bundle(&shop);
    write_file(&out_dir.join("shop_cache.pickle"), &shop)?;
    write_file(&out_dir.join("shop_cache.pickle.z"), &shop_z)?;
    write_file(&out_dir.join("shop_cache.desc"), &shop_desc)?;
    println!(
        "  shop_cache.pickle {:>13} bytes (zlib {} bytes, crc32 {})",
        shop.len(),
        shop_z.len(),
        pickle::signed_crc32(&shop_z)
    );

    // Dossier sync (stream only in the client).
    let dossier = account::dossier_sync_pickle(opts.int("version", 0)?);
    let (dossier_z, dossier_desc) = account::stream_bundle(&dossier);
    write_file(&out_dir.join("dossier_sync.pickle"), &dossier)?;
    write_file(&out_dir.join("dossier_sync.pickle.z"), &dossier_z)?;
    write_file(&out_dir.join("dossier_sync.desc"), &dossier_desc)?;
    println!(
        "  dossier_sync.pickle {:>10} bytes (zlib {} bytes, crc32 {})",
        dossier.len(),
        dossier_z.len(),
        pickle::signed_crc32(&dossier_z)
    );
    Ok(())
}

fn append_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut os = path.as_os_str().to_os_string();
    os.push(suffix);
    PathBuf::from(os)
}
