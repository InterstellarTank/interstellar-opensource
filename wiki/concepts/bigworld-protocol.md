---
updated: 2026-11-11
sources: [docs/research/protocol-scout.md, docs/research/client-analysis.md]
---

# BigWorld protocol (2.0.1)

Мережевий шар, яким говорить клієнт WoT 0.6.5. Це **стоковий BigWorld Engine 2.0.1**; клієнт
2011 року не має жодних WoT-специфічних розширень на дроті (підтверджено таблицями імен
інтерфейсів у `WorldOfTanks.exe` — див. `docs/research/protocol-scout.md`).

## Транспорт

- Все поверх **Mercury**: свої фрейми в UDP-датаграмах (LoginApp — UDP), BaseApp — TCP-канал
  BigWorld з тим самим форматом пакетів.
- Пакет: `[u16 flags BE][messages…][footers…]`. Прапорці: `HAS_REQUESTS=0x1`, `HAS_PIGGYBACKS=0x2`,
  `HAS_ACKS=0x4`, `ON_CHANNEL=0x8`, `IS_RELIABLE=0x10`, `HAS_SEQUENCE_NUMBER=0x40`,
  `HAS_CHECKSUM=0x100`, `CREATE_CHANNEL=0x200` (на зовнішніх інтерфейсах заборонений).
- Footers пишуться «з кінця»: …seq/firstRequestOffset/acks/cumulativeAck/indexed/checksum.
- Повідомлення в bundle: `[id:u8][header][body]`; для VARIABLE-повідомлень header несе довжину
  (`lengthParam` 2 байти; `0xFFFF` → спеціальний 4-байтовий розмір на початку). Requests
  додатково несуть `[replyID:i32][nextRequestOffset:u16]` після заголовка.
- Reply: `id=0xFF` + `u32 BE len` + `replyID:i32` + дані.

## Шифрування

- **RSA-2048 OAEP-SHA1** (OpenSSL `RSA_PKCS1_OAEP_PADDING`, `partSize = modulus − 41 − 1 = 214`),
  блоки по 256 байт. Використовується лише для `LogOnParams`.
- **Blowfish ECB + chaining**: кожен блок (8 байт) перед шифруванням XOR-иться з попереднім
  *відкритим* блоком (IV=0) — захист від replay; при дешифруванні — навпаки.
- У кінець пакета дописується padding, `0xdeadbeef` (LE) і байт wastage; довжина кратна 8.
- Після логіна весь baseapp-канал шифрується Blowfish-ключем, який **генерує клієнт** і передає
  в `LogOnParams.encryptionKey`.

## Два застосунки, які бачить клієнт

**LoginApp** (`LoginInterface`): `login` id 0 (VARIABLE 2), `probe` id 1 (FIXED 0).
`LOGIN_VERSION = 56`; запит = `u32 version` + `bool encrypted` + RSA-зашифрований `LogOnParams`
(`u8 flags`, username, password, encryptionKey, optional 16B MD5 digest, `u32 nonce`).
Успіх = `u8 LOGGED_ON=1` + Blowfish-зашифрований `LoginReplyRecord{IPv4 Address, u32 sessionKey}`.

**BaseApp** (`BaseAppExtInterface`, 0-based): 0 `baseAppLogin`, 1 `authenticate`, 2–7
`avatarUpdate*`, 8 `switchInterface`, 9 `requestEntityUpdate`, 10 `enableEntities`,
11 `restoreClientAck`, 12 `disconnectClient`, 13 `entityMessage`.

**ClientInterface**: 0 `authenticate`, 5 `createBasePlayer`, 6 `createCellPlayer`, 8 `createEntity`,
9 `updateEntity`, …, 55 `resourceHeader`, 56 `resourceFragment`, 57 `loggedOff`; entity-методи
йдуть id `128..254` (старший біт 0x80 + індекс exposed client method).

## Джерела

- `docs/research/protocol-scout.md` — повний розбір з цитатами C++ файлів.
- Локальна копія BigWorld 2.0.1: `.scratch/research/protocol-scout/repos/bw201/`
  (`src/lib/connection/*.hpp`, `src/lib/network/{packet,encryption_filter,public_key_cipher,bundle,interface_element}.*`).
- Контракт клієнта: `docs/research/client-analysis.md`.
