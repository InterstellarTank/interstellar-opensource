---
name: wot-live-triage
description: >-
  Use when deploying, patching, running, or troubleshooting live World of Tanks 0.6.5
  client login, BaseApp handoff, or hangar hangs against wot-server, bw201-login, and
  wot-baseapp on Windows.
---

# WoT 0.6.5 Live Server Triage

Follow this order before proposing code changes so client, daemon, and Windows permission issues are separated cleanly.

## Client Patching & Server Launch Cheatsheet

- **Key-path asymmetry (`src/main.rs`):** `setup` and `keys` default `--keys-dir` to `keys`, whereas `serve` defaults `--priv-key` to `.scratch/wot-keys/loginapp_priv.pem`. Reuse `.scratch\wot-keys` explicitly across commands so login decryption matches the client key.
- **Check / patch client (`--pubkey` is required on `patch-client`):**
  - Inspect current client endpoint & key: `.\target\release\wot-server.exe check-client --game-dir "<GAME_DIR>" --pubkey .scratch\wot-keys\loginapp_pub.pem`
  - Patch existing key: `.\target\release\wot-server.exe patch-client --game-dir "<GAME_DIR>" --pubkey .scratch\wot-keys\loginapp_pub.pem`
  - One-shot keygen + patch: `.\target\release\wot-server.exe setup --game-dir "<GAME_DIR>" --keys-dir .scratch\wot-keys`
- **Serve & offline account tools:**
  - Run server: `.\target\release\wot-server.exe serve --game-dir "<GAME_DIR>" --preset <retail|sandbox>` (defaults: `--data-dir .scratch/accounts`, `--log-dir .scratch/logs`, optional `--economics-config <PATH>`).
  - Edit profiles between runs: `wot-server.exe account {show,reset,grant,damage} <USERNAME> ...` (in-memory `AccountRegistry` in `wot-baseapp` does **not** hot-reload disk JSON edits mid-run — restart `serve` after mutating).

## Steps

1. **Check active server processes and binary age**
   - Inspect running `wot-server`, `bw201-login`, and `wot-baseapp` processes (PID, executable path `target/debug` vs `target/release`, command line, `StartTime`).
   - Confirm whether the failure happened against an outdated binary, wrong `--priv-key` vs client `loginapp_wot.pubkey`, stale `.scratch/accounts/<name>.json`, or overridden `--session-file` / `--account-diff` / `--shop-cache` / `--dossier-sync` / `--economics-config`.

2. **Correlate the three runtime logs**
   - Client script exceptions (hangar freezes, UI errors): `<game_dir>/python.log`
   - Login handoff, `LoginRejectedServerNotReady`, and `.pending/<key>.json` write errors: `.scratch/logs/bw201-login.out.log` / `.err.log`
   - BaseApp UDP handshake, missing session warnings, and `CMD_*` handling: `.scratch/logs/wot-baseapp.out.log` / `.err.log`

3. **Diagnose Windows `Access is denied (os error 5)` systematically**
   - Never assume NTFS DACLs alone explain `os error 5` when a command shell write succeeds.
   - Run `icacls` on both the target directory and `.exe` image to check `Mandatory Label\...`, and verify process integrity RID (`0x1000` Low IL vs `0x2000` Medium IL).
   - Verify no target files live under OS `%TEMP%` or another Medium-IL directory when binaries run at Low IL.

4. **Verify without disturbing live binaries**
   - Run `cargo test --workspace` for unit/integration coverage (remember `live_daemon_udp_smoke` is `#[ignore]`).
   - Do not overwrite locked `target/release/*.exe` while the user's server is running; instruct the user to stop with `Ctrl+C` and run `cargo build --release --workspace` outside the sandbox.
