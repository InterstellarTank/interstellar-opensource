# Log

Хронологічний журнал вікі. Записи додаються в кінець; кожен починається з `## [YYYY-MM-DD] <операція> | <назва>`, де операція — `init`, `ingest`, `query` або `lint`.

## [2026-11-10] init | LLM Wiki scaffolded

## [2026-11-11] query | Як клієнт WoT 0.6.5 логіниться і як формується ангар з усіма танками?

Прочитано `docs/research/client-analysis.md`, `docs/research/protocol-scout.md` (чернетка),
`docs/research/gamedata-analysis.md` (чернетка), `.scratch/research/decompiled/_generated/README.md`.
Створено сторінки [[concepts/bigworld-protocol]], [[concepts/packed-xml]],
[[concepts/account-sync]], [[concepts/compact-descriptor]],
[[entities/wot-0-6-5-client]], [[entities/bigworld-engine-2-0-1]],
[[entities/wg-toolkit-rs]], [[analyses/rust-server-architecture]].
Питання: чи можна повний sync віддати одним `onCmdResponseExt` (без resource stream) —
попередньо так для CMD_SYNC_DATA, але shop/dossier потребують stream.

> Примітка: дослідницькі нотатки лежать у `docs/research/` і ще не перенесені в `raw/`
> (правило AGENTS.md: джерела в `raw/` додає людина). Після перенесення сторінки треба
> перелінкувати цитати на `raw/...`.

## [2026-11-13] lint | Узгодження [[analyses/rust-server-architecture]], [[overview]] та [[index]] із wire-спеком і реалізацією

Оновлено [[analyses/rust-server-architecture]] (`updated: 2026-11-13`): додано `[[wikilinks]]`,
оновлено схему `LoginApp` (`UDP 20016`, `version=57`, без `bool encrypted`) і `BaseApp` (`UDP 20017`,
`enableEntities` setup notifications, `serverSettings={'vivoxDomain': ''}`, 64-bit rev,
`rid == requestID`, `RES_FAILURE = -1`, мультиклієнтська таблиця за `SocketAddr`), позначено
суперечності зі старішими чернетками в [[concepts/bigworld-protocol]] і [[concepts/account-sync]].
Оновлено [[overview]] та список кандидатів у [[index]] (замінено неіснуючий `gamedata-analysis.md`
на `bw201-login-wire.md` та `bw201-baseapp-wire.md`).
Пропозиція людині: створити `GLOSSARY.md` (і за потреби `docs/adr/`) для канонічних термінів протоколу.
