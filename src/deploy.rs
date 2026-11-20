//! Client deployment helpers: RSA key management and packed-XML client config patching.
//!
//! The retail WoT 0.6.5 client reads its login app endpoint from
//! `res/scripts_config.xml` (BigWorld packed XML, magic `45 4E A1 62`) and the login
//! public key from the file named by `login/host/public_key_path` (relative to `res/`).
//! To point the client at our Rust server we:
//!   1. generate/pick an RSA-2048 key pair (PKCS#8 PEM private, SPKI PEM public),
//!   2. repack `scripts_config.xml` with `login/host/url = 127.0.0.1:20016`,
//!   3. install our public key over `res/loginapp_wot.pubkey`.
//!
//! Everything is reversible: the first patch backs the original files up next to them
//! (`.orig` suffix), and `restore-client` puts them back.

use std::fs;
use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use rsa::pkcs8::{EncodePrivateKey, EncodePublicKey, LineEnding};
use rsa::RsaPrivateKey;
use sha2::{Digest, Sha256};

use wgtk::pxml::{self, Element, Value};

/// Default endpoint the client is patched to.
pub const DEFAULT_LOGIN_HOST: &str = "127.0.0.1:20016";
/// Default base app endpoint returned to the client by our login app.
pub const DEFAULT_BASE_APP: &str = "127.0.0.1:20017";

/// Relative path of the packed client config, from the game directory.
pub const SCRIPTS_CONFIG: &str = "res/scripts_config.xml";
/// Relative path of the client login public key file.
pub const PUBKEY: &str = "res/loginapp_wot.pubkey";

/// Generate a 2048-bit RSA key pair in `out_dir`.
///
/// Returns `(private_key_path, public_key_path)`. Existing files are only overwritten
/// with `force`.
pub fn generate_keys(out_dir: &Path, force: bool) -> Result<(PathBuf, PathBuf)> {
    fs::create_dir_all(out_dir)
        .with_context(|| format!("failed to create key dir {}", out_dir.display()))?;

    let priv_path = out_dir.join("loginapp_priv.pem");
    let pub_path = out_dir.join("loginapp_pub.pem");

    if !force && priv_path.exists() && pub_path.exists() {
        return Ok((priv_path, pub_path));
    }

    let mut rng = rand::rngs::OsRng;
    let private = RsaPrivateKey::new(&mut rng, 2048).context("failed to generate RSA key")?;
    let public = private.to_public_key();

    let priv_pem = private
        .to_pkcs8_pem(LineEnding::LF)
        .context("failed to encode PKCS#8 private key")?;
    let pub_pem = public
        .to_public_key_pem(LineEnding::LF)
        .context("failed to encode SPKI public key")?;

    fs::write(&priv_path, priv_pem.as_bytes())
        .with_context(|| format!("failed to write {}", priv_path.display()))?;
    fs::write(&pub_path, pub_pem.as_bytes())
        .with_context(|| format!("failed to write {}", pub_path.display()))?;

    Ok((priv_path, pub_path))
}

/// SHA-256 of a file, hex encoded.
pub fn sha256_file(path: &Path) -> Result<String> {
    let data = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    let mut hasher = Sha256::new();
    hasher.update(&data);
    Ok(hex::encode(hasher.finalize()))
}

fn backup_once(path: &Path) -> Result<PathBuf> {
    let backup = path.with_extension(format!(
        "{}.orig",
        path.extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default()
    ));
    if !backup.exists() {
        fs::copy(path, &backup)
            .with_context(|| format!("failed to back up {} -> {}", path.display(), backup.display()))?;
    }
    Ok(backup)
}

/// Set a leaf value inside an element, handling both "string child" and
/// "element holding a string value" representations used by packed XML.
fn set_leaf(parent: &mut Element, key: &str, value: impl Into<String>) -> bool {
    let value = Value::String(value.into());
    match parent.get_child_mut(key) {
        Some(Value::Element(elt)) => {
            elt.value = value;
            true
        }
        Some(slot) => {
            *slot = value;
            true
        }
        None => false,
    }
}

fn child_element_mut<'a>(parent: &'a mut Element, key: &str) -> Option<&'a mut Element> {
    match parent.get_child_mut(key)? {
        Value::Element(elt) => Some(&mut **elt),
        _ => None,
    }
}

/// Patch `res/scripts_config.xml` so the client connects to `login_host`, and install
/// `pubkey` as the client's login public key.
///
/// Backs up both files once (`.orig`) and writes a readable unpacked dump of the
/// patched config next to it (`scripts_config.patched.xml`).
pub fn patch_client(game_dir: &Path, login_host: &str, pubkey: &Path) -> Result<()> {
    let config_path = game_dir.join(SCRIPTS_CONFIG);
    let pubkey_path = game_dir.join(PUBKEY);

    if !config_path.exists() {
        bail!("not a WoT 0.6.5 game dir (missing {})", config_path.display());
    }
    if !pubkey.exists() {
        bail!("public key not found: {}", pubkey.display());
    }

    let config_backup = backup_once(&config_path)?;
    let pubkey_backup = backup_once(&pubkey_path)?;

    let file = File::open(&config_path)
        .with_context(|| format!("failed to open {}", config_path.display()))?;
    let mut root = pxml::from_reader(file)
        .with_context(|| format!("failed to parse packed XML {}", config_path.display()))?;

    let login = child_element_mut(&mut root, "login").context("scripts_config.xml: missing <login>")?;
    let host = child_element_mut(login, "host").context("scripts_config.xml: missing <login><host>")?;

    if !set_leaf(host, "url", login_host) {
        bail!("scripts_config.xml: missing login/host/url");
    }
    // Keep the key file name stable; we simply replace the key file's content.
    if host.get_child("public_key_path").is_none() {
        host.add_children("public_key_path", Value::String("loginapp_wot.pubkey".to_string()));
    }

    // Write to a sibling temp file, then replace, so a crash cannot leave a truncated
    // config behind.
    let tmp_path = config_path.with_file_name("scripts_config.xml.new");
    {
        let file = File::create(&tmp_path)
            .with_context(|| format!("failed to create {}", tmp_path.display()))?;
        let mut writer = BufWriter::new(file);
        pxml::to_writer(&mut writer, &root).context("failed to serialize patched packed XML")?;
    }
    fs::rename(&tmp_path, &config_path)
        .with_context(|| format!("failed to replace {}", config_path.display()))?;

    fs::copy(pubkey, &pubkey_path)
        .with_context(|| format!("failed to install public key {}", pubkey_path.display()))?;

    // Human-readable evidence of what was changed.
    let dump_path = game_dir.join("res/scripts_config.patched.xml");
    fs::write(
        &dump_path,
        format!(
            "<!-- unpacked by wot-server setup; login host: {login_host} -->\n{}",
            element_to_xml(&root, 0)
        ),
    )
    .with_context(|| format!("failed to write {}", dump_path.display()))?;

    println!("patched {}", config_path.display());
    println!("  login host      : {login_host}");
    println!("  public key      : {}", pubkey_path.display());
    println!("  backup (config) : {}", config_backup.display());
    println!("  backup (pubkey) : {}", pubkey_backup.display());
    println!("  readable dump   : {}", dump_path.display());
    Ok(())
}

/// Restore the original client config and public key from their `.orig` backups.
pub fn restore_client(game_dir: &Path) -> Result<()> {
    let config_path = game_dir.join(SCRIPTS_CONFIG);
    let pubkey_path = game_dir.join(PUBKEY);
    let config_backup = backup_once_path(&config_path)?;
    let pubkey_backup = backup_once_path(&pubkey_path)?;

    fs::copy(&config_backup, &config_path)
        .with_context(|| format!("failed to restore {}", config_path.display()))?;
    fs::copy(&pubkey_backup, &pubkey_path)
        .with_context(|| format!("failed to restore {}", pubkey_path.display()))?;
    let dump = game_dir.join("res/scripts_config.patched.xml");
    let _ = fs::remove_file(dump);

    println!("restored {}", config_path.display());
    println!("restored {}", pubkey_path.display());
    Ok(())
}

fn backup_once_path(path: &Path) -> Result<PathBuf> {
    let backup = path.with_extension(format!(
        "{}.orig",
        path.extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default()
    ));
    if !backup.exists() {
        bail!("backup not found: {}", backup.display());
    }
    Ok(backup)
}

/// Print what the client config currently says (login endpoint, key file, hashes).
pub fn check_client(game_dir: &Path, our_pubkey: Option<&Path>) -> Result<()> {
    let config_path = game_dir.join(SCRIPTS_CONFIG);
    let pubkey_path = game_dir.join(PUBKEY);

    let file = File::open(&config_path)
        .with_context(|| format!("failed to open {}", config_path.display()))?;
    let root = pxml::from_reader(file).context("failed to parse packed XML")?;

    let login = root
        .get_child_element("login")
        .context("scripts_config.xml: missing <login>")?;
    let host = login
        .get_child_element("host")
        .context("scripts_config.xml: missing <login><host>")?;

    println!("config          : {}", config_path.display());
    println!("login url       : {}", host.get_child_string("url").unwrap_or("<missing>"));
    println!(
        "public key path : {}",
        host.get_child_string("public_key_path").unwrap_or("<missing>")
    );
    println!("pubkey file     : {}", pubkey_path.display());
    if pubkey_path.exists() {
        println!("pubkey sha256   : {}", sha256_file(&pubkey_path)?);
    }
    if let Some(ours) = our_pubkey {
        let ours_hash = sha256_file(ours)?;
        println!("our pubkey      : {} ({})", ours.display(), ours_hash);
        if pubkey_path.exists() && sha256_file(&pubkey_path)? == ours_hash {
            println!("status          : MATCH (client is set up for our server)");
        } else {
            println!("status          : MISMATCH (run `wot-server setup` to install)");
        }
    }
    let backup = config_path.with_extension("xml.orig");
    println!(
        "backup exists   : {}",
        if backup.exists() { "yes" } else { "no" }
    );
    Ok(())
}

fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn element_to_xml(elt: &Element, depth: usize) -> String {
    let indent = "\t".repeat(depth);
    match &elt.value {
        Value::Element(child) => element_to_xml(child, depth),
        Value::String(s) => {
            let mut out = format!("{indent}{}\n", escape_xml(s));
            for (name, value) in elt.iter_children_all() {
                out.push_str(&format!("{indent}<{name}>\n"));
                match value {
                    Value::Element(e) => out.push_str(&element_to_xml(e, depth + 1)),
                    Value::String(s) => {
                        out.push_str(&format!("{}\t{}\n", indent, escape_xml(s)))
                    }
                    Value::Integer(i) => out.push_str(&format!("{}\t{i}\n", indent)),
                    Value::Boolean(b) => out.push_str(&format!("{}\t{b}\n", indent)),
                    Value::Vector(_) => out.push_str(&format!("{}\t<vector>\n", indent)),
                }
                out.push_str(&format!("{indent}</{name}>\n"));
            }
            out
        }
        Value::Integer(i) => format!("{indent}{i}\n"),
        Value::Boolean(b) => format!("{indent}{b}\n"),
        Value::Vector(v) => format!("{indent}{v:?}\n"),
    }
}

#[allow(dead_code)]
fn value_as_string(value: &Value) -> Option<&str> {
    match value {
        Value::String(s) => Some(s),
        Value::Element(elt) => elt.value.as_string(),
        _ => None,
    }
}
