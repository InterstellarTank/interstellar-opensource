# Overview

Еволюційний синтез проєкту **Rust-сервер для World of Tanks 0.6.5**: що ми знаємо, що вже
працює і які висновки тримаються купи.

## Project

Мета — ванільний ретейл-клієнт WoT 0.6.5 (2011, BigWorld Engine 2.0.1) заходить на власний
Rust-сервер і потрапляє в ангар, де видно **всі танки з файлів гри**.

Стан на 2026-11-13:

- клієнт запускається і рендерить екран логіна (перевірено нативно, DX9);
- деплой клієнта автоматизовано Rust-утилітою: `res/scripts_config.xml` перепаковано на
  `127.0.0.1:20016`, `res/loginapp_wot.pubkey` замінено нашим RSA-ключем (`src/deploy.rs`);
- протокол відновлено за публічним C++ кодом [[entities/bigworld-engine-2-0-1]] (`docs/research/bw201-login-wire.md`, `docs/research/bw201-baseapp-wire.md`) та декомпільованими 356 Python 2.6 скриптами (`docs/research/client-analysis.md`);
- усі 248 XML/`.def` файлів гри розпаковано з [[concepts/packed-xml]] (магія `45 4E A1 62`), round-trip біт-у-біт;
- зібрано каталог усіх 130 типів танків з 15-байтовими [[concepts/compact-descriptor]] (`docs/research/vehicle-catalog.json`);
- `crates/bw201`, `crates/wot065-data`, `crates/wot-baseapp` і `wot-server` (`[[analyses/rust-server-architecture]]`) зібрано й перевірено unit- та E2E UDP-тестами.

## Архітектура (short)

Клієнт говорить двома **UDP**-сокетами BigWorld/Mercury:

1. **LoginApp** (`:20016`, UDP) — приймає `LoginInterface::login` (`u32 version=57` + `u32 build_tag` + RSA-OAEP-SHA1 зашифрований `LogOnParams`, без `bool encrypted`), відповідає `LOGGED_ON` + Blowfish stream-зашифрований `LoginReplyRecord{base app address, session key}`.
2. **BaseApp** (`:20017`, UDP) — `baseAppLogin` + `authenticate`, початкові `ClientInterface` повідомлення, створення базової сутності `Account` (`ClientIndex = 0`, `serverSettings={'vivoxDomain': ''}`), виклик клієнтського методу `showGUI(pickled ctx)`, відповіді на `CMD_SYNC_DATA`/`SHOP`/`DOSSIERS`.
3. Дані ангара приходять не як entity properties, а як zlib+cPickle-2 потік (`RES_STREAM`, `rid == requestID`) при `rev=0` на `CMD_SYNC_DATA`; інвентар містить по одному [[concepts/compact-descriptor]] на кожен із 130 танків.

## Open questions

- Чи валідує клієнт `digest` (MD5 entity defs) у `LogOnParams` — наразі сервер його ігнорує.
- Підтвердити візуальний вхід в ангар зі 130 танками запуском `WorldOfTanks.exe` поза пісочницею (на інтерактивному робочому столі користувача).

## Latest findings

- У `BaseApp` (`UDP 20017`) критичні вимоги живого клієнта 0.6.5 включають: `{'vivoxDomain': ''}` у `serverSettings`, зовнішній `seqNum` навіть для ACK-пакетів (`FLAG_HAS_SEQUENCE_NUMBER`), 64-розрядне розбиття ревізії `(rev >> 32, rev & 0xffffffff)`, відповідність `rid == requestID` у `resourceHeader`/`resourceFragment` та `RES_FAILURE = -1` на непідтримувані `Account.doCmd*` запити.
- [[entities/wg-toolkit-rs]] (`vendor/wg-toolkit-rs/`) використано як референс для [[concepts/packed-xml]], тоді як дротовий шар 2.0.1 реалізовано напряму в `crates/bw201` (`docs/research/bw201-login-wire.md`).
