---
updated: 2026-11-11
sources: [docs/research/protocol-scout.md]
---

# wg-toolkit-rs

Rust-тулкіт для кодеків Wargaming (BigWorld/Core), MIT, автор Théo Rozier
(`theorzr/wg-toolkit-rs`; вендорено в `vendor/wg-toolkit-rs/`).

## Що в ньому є

- `wg-toolkit` (lib `wgtk`): packed XML (pxml) кодек, resource VFS, script model (entity defs),
  **мережевий протокол**: packet/bundle/element, RSA/Blowfish фільтри, а також серверні застосунки
  LoginApp/BaseApp і **документація**: `doc/APPS.md`, `doc/ENTITY.md`, `doc/HANGAR_LOADING.md`,
  `doc/PROXY.md`.
- `wg-toolkit-wot`: proof-of-concept сервер і MITM-проксі для сучасної WoT (v2.3.1.3, 2026).

## Чому важливий

1. Підтвердив, що магія packed XML 0.6.5 (`45 4E A1 62`) — той самий формат; кодек використовуємо
   для патчу `scripts_config.xml`.
2. Структурний референс мережевого шару (пакети, bundle, Blowfish), хоч протокольні таблиці там
   **сучасні**, не 0.6.5.
3. `doc/HANGAR_LOADING.md` — детальний опис сучасного ангара (допомагає зрозуміти, що змінилося).

## Застереження

`app/login` і `app/base` цільові під WoT v2.3.1.3: інші message id, `onCmdResponse` з 3 аргументами,
`CMD_SYNC_DATA` з іншими аргументами. Для 0.6.5 брати **лише транспортні ідеї**, не таблиці.

## Джерела

- `vendor/wg-toolkit-rs/README.md`, `doc/APPS.md`, `doc/HANGAR_LOADING.md`
- `docs/research/protocol-scout.md`
