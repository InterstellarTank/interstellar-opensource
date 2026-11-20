//! Small helpers shared by the crate's tests: locate the repository root, the research
//! reference payloads and a local WoT 0.6.5 installation. They never fail hard - tests
//! that need an optional resource skip themselves when it is absent.

use std::path::{Path, PathBuf};

/// Repository root (the crate lives at `<root>/crates/wot065-data`).
pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crate is inside crates/")
        .to_path_buf()
}

/// Directory with the Python-2 reference payloads produced by the `client-reverse`
/// teammate (`account_sync_full.pickle`, `showgui_ctx.pickle`, ...). Read-only input.
pub fn reference_dir() -> Option<PathBuf> {
    let dir = repo_root().join(".scratch/research/decompiled/_generated");
    dir.is_dir().then_some(dir)
}

/// Reference vehicle catalog JSON (read-only input, validation only).
pub fn reference_catalog_path() -> Option<PathBuf> {
    let path = reference_dir()?.join("vehicles_catalog.json");
    path.is_file().then_some(path)
}

/// Locate the retail WoT 0.6.5 installation. `WOT065_GAME_DIR` wins; otherwise the
/// development machine's install location is probed. Returns the directory that contains
/// `res/scripts/item_defs/vehicles` when possible, but callers should use
/// [`crate::catalog::resolve_vehicles_dir`] to be tolerant of what exactly was passed.
pub fn game_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("WOT065_GAME_DIR") {
        let dir = PathBuf::from(dir);
        if dir.is_dir() {
            return Some(dir);
        }
    }
    let candidates = [
        r"C:\Users\qwerty\Desktop\World_of_Tanks",
        r"C:\Program Files\World_of_Tanks",
        r"C:\Program Files (x86)\World_of_Tanks",
    ];
    for candidate in candidates {
        let path = PathBuf::from(candidate);
        if resolve_from(&path).is_some() {
            return Some(path);
        }
    }
    None
}

/// Same resolution rules as [`crate::catalog::resolve_vehicles_dir`]; duplicated here only
/// to avoid a dependency cycle in the probe above.
fn resolve_from(dir: &Path) -> Option<PathBuf> {
    let candidates = [
        dir.join("res/scripts/item_defs/vehicles"),
        dir.join("scripts/item_defs/vehicles"),
        dir.join("item_defs/vehicles"),
        dir.to_path_buf(),
    ];
    candidates.into_iter().find(|p| p.is_dir())
}

/// Python 2.7 interpreter bundled in the research tools (used for optional validation of
/// generated pickles under an interpreter that behaves like the retail client).
pub fn python2() -> Option<PathBuf> {
    let candidates = [
        repo_root()
            .join(".scratch/research/decompiled/_tools/py27/inst/python.exe"),
    ];
    candidates.into_iter().find(|p| p.is_file())
}

/// Python 3 virtual env bundled in the research tools.
pub fn python3() -> Option<PathBuf> {
    let candidates = [
        repo_root().join(".scratch/research/decompiled/_tools/venv/Scripts/python.exe"),
    ];
    candidates.into_iter().find(|p| p.is_file())
}
