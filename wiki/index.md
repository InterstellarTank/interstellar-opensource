# Index

Каталог усіх сторінок вікі. Огляд проєкту — [[overview]], хронологія змін — [[log]].

## Sources

_Кандидати у `raw/` (очікують перенесення людиною, див. `docs/research/`):_

- `docs/research/client-analysis.md` — контракт клієнта 0.6.5 (login → Account sync → ангар)
- `docs/research/protocol-scout.md` — BigWorld 2.0.1 wire protocol
- `docs/research/bw201-login-wire.md` — LoginApp UDP wire spec (BW 2.0.1 / WoT 0.6.5)
- `docs/research/bw201-baseapp-wire.md` — BaseApp UDP wire spec (BW 2.0.1 / WoT 0.6.5)
- `docs/research/vehicle-catalog.json` — 130 типів танків з compact descriptors

## Concepts

- [[concepts/bigworld-protocol]] — Mercury/BigWorld 2.0.1: пакети, bundle, RSA/Blowfish, LoginApp/BaseApp
- [[concepts/packed-xml]] — BigWorld packed XML (BWXML): формат і кодек
- [[concepts/account-sync]] — синхронізація акаунта в 0.6.5: CMD_SYNC_DATA, pickle, ангар
- [[concepts/compact-descriptor]] — compact descriptors предметів/танків у 0.6.5

## Entities

- [[entities/wot-0-6-5-client]] — ретейл-клієнт 0.6.5: версії, файли, Python 2.6 скрипти
- [[entities/bigworld-engine-2-0-1]] — рушій і його публічне C++ джерело
- [[entities/wg-toolkit-rs]] — MIT Rust-тулкіт WG: packed XML, entity model, мережевий шар

## Analyses

- [[analyses/rust-server-architecture]] — архітектура нашого Rust-сервера та статус
