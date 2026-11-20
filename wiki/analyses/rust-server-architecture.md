---
updated: 2026-11-13
sources: [docs/research/bw201-login-wire.md, docs/research/bw201-baseapp-wire.md, docs/research/client-analysis.md, docs/research/protocol-scout.md]
---

# Rust server architecture

Архітектура сервера для ретейл-клієнта [[entities/wot-0-6-5-client]] на базі [[entities/bigworld-engine-2-0-1]]: Rust workspace у репозиторії `interstellar` (огляд проєкту — [[overview]]).

## Крейти

| Крейт | Роль |
|---|---|
| `wot-server` (`src/`) | CLI: деплой клієнта (RSA-ключі, patch [[concepts/packed-xml]], `restore-client`, `check-client`) і супервізор `serve`, який піднімає обидва процеси |
| `crates/bw201` | Транспорт [[concepts/bigworld-protocol]]: `BinaryStream` (LE, packed int, MOTP), Mercury-пакети/bundle, RSA-OAEP-SHA1, Blowfish-фільтри та бінарник `bw201-login` (`LoginApp` на `UDP 20016`) |
| `crates/wot-baseapp` | `BaseApp` (`UDP 20017`): надійний UDP-канал, мультиклієнтський роутер за `SocketAddr`, `baseAppLogin`/`authenticate`, початкова ініціалізація клієнта, `createBasePlayer(Account)`, `showGUI`, обробка `Account.doCmd*` (`[[concepts/account-sync]]`) |
| `crates/wot065-data` | Дані гри: каталог 130 танків із [[concepts/compact-descriptor]], Python-2 pickle protocol 2, payload'и синхронізації/ангара |
| `vendor/wg-toolkit-rs` | MIT-референс [[concepts/packed-xml]] і структур [[entities/wg-toolkit-rs]] |

## Дротовий потік ([[concepts/bigworld-protocol]])

```
client --UDP 20016--> bw201-login: LoginInterface::login (u32 v=57, [build_tag], RSA(LogOnParams))
                     <-- LOGGED_ON + Blowfish stream(LoginReplyRecord{BaseApp addr, sessionKey})
client --UDP 20017--> wot-baseapp: baseAppLogin(sessionKey) [Blowfish packet filter]
                     <-- reply + ClientInterface::authenticate
client --UDP 20017--> authenticate, enableEntities
                     <-- bandwidthNotification, updateFrequencyNotification, setGameTime
                     <-- createBasePlayer(Account, typeID=0, serverSettings={'vivoxDomain': ''})
                     <-- Account.showGUI(pickle ctx)
client --entityMsg--> doCmdInt3(reqID, CMD_SYNC_DATA, revHi=0, revLo=0, 0)
                     <-- onCmdResponse(reqID, RES_STREAM) + resourceHeader/resourceFragment(rid=reqID)
client --entityMsg--> doCmdInt3(reqID, CMD_SYNC_DATA, revHi=0, revLo=1, 0)
                     <-- onCmdResponseExt(reqID, RES_SUCCESS, {'rev': N, 'prevRev': N})
client --entityMsg--> CMD_SYNC_SHOP / CMD_SYNC_DOSSIERS -> resource stream (rid=reqID)
                     | CMD_REQ_SERVER_STATS -> receiveServerStats | інші cmd -> RES_FAILURE (-1)
```

Сесія між `bw201-login` і `wot-baseapp` передається атомарно записаним JSON-файлом (`--session-out` / `--session-file`): `{username, encryption_key_hex, session_key, client_addr}` (`docs/research/bw201-login-wire.md`), оскільки Blowfish-ключ генерує клієнт усередині `LogOnParams`. `wot-baseapp` кешує останні handoff-сесії та ізолює активних клієнтів за `SocketAddr` (`crates/wot-baseapp/src/lib.rs`).

> Суперечність із попередніми нотатками: у [[concepts/bigworld-protocol]] раніше вказано BaseApp як TCP і `LOGIN_VERSION = 56` + `bool encrypted`; за `docs/research/bw201-login-wire.md`, `docs/research/bw201-baseapp-wire.md` та `docs/research/login-request-capture.md` обидва сокети працюють поверх **UDP** (`20016` і `20017`), `bool encrypted` у BW 2.0.1 відсутній, а живий клієнт 0.6.5 шле `version = 57`. У [[concepts/account-sync]] також зазначено `typeID = 1` для `Account` і єдиний `onCmdResponseExt` без стріму, проте фактичний `scripts/entities.xml` дає `ClientIndex = 0`, а `CMD_SYNC_DATA` при `rev = 0` віддає `RES_STREAM` + zlib resource stream із `rid == requestID` (`crates/wot-baseapp/src/protocol.rs`).

## Деплой клієнта

`wot-server setup` генерує RSA-2048 пару (PKCS#8 private + SPKI public), перепаковує `res/scripts_config.xml` на `127.0.0.1:20016`, підкладає публічний ключ у `res/loginapp_wot.pubkey` і зберігає бекапи `.orig`; `restore-client` повертає оригінали (`src/deploy.rs`).

## Статус і відкриті питання

- Реалізовано й перевірено unit- та UDP E2E-тестами (`crates/bw201`, `crates/wot065-data`, `crates/wot-baseapp`): повний RSA+Blowfish handoff, зовнішні unreliable sequence numbers для ACK-only пакетів, `{'vivoxDomain': ''}` у `serverSettings` (`docs/research/client-analysis.md`), 64-розрядна ревізія `(rev >> 32, rev & 0xffffffff)`, `rid == requestID`, стріми на 130 танків і живий супервізор `wot-server serve`.
- Відкрите питання: у пісочниці Windows блокуються записи логів у папку клієнта (`python.log`) та рендеринг вікна DX9, тож фінальна інтерактивна перевірка візуального входу в ангар на реальному десктопі очікує запуску `WorldOfTanks.exe` користувачем.

## Джерела

- `docs/research/bw201-login-wire.md` — формат пакетів, RSA `LogOnParams`, `LoginReplyRecord`.
- `docs/research/bw201-baseapp-wire.md` — BaseApp UDP-канал, послідовність `enableEntities`, `createBasePlayer`.
- `docs/research/client-analysis.md` — контракт `Account`, `vivoxDomain`, `SyncController` і 130 танків.
- `docs/research/protocol-scout.md` — відповідність інтерфейсів у `WorldOfTanks.exe`.
