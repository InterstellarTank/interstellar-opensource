---
updated: 2026-11-11
sources: [docs/research/client-analysis.md]
---

# WoT 0.6.5 client (retail)

Ретейл-клієнт, під який будується сервер.

## Ідентифікація

- Інсталяція на цій машині: `C:\Users\qwerty\Desktop\World_of_Tanks`
- `version.xml`: `v.0.6.5`; `WOTLauncher.cfg`: client_ver `65.95961`, content_ver `65.95397`, en
- `WorldOfTanks.exe`, 12.2 МБ; усередині — шляхи збірки `d:\WoT\game_releases\beta_6.5\...` і
  повні таблиці імен інтерфейсів BigWorld (див. [[concepts/bigworld-protocol]]).

## Файли, важливі для сервера

- `res/scripts_config.xml` (packed XML) — адреса LoginApp
  (`login/host/url = login-master.worldoftanks.com:20016` в оригіналі) і `public_key_path`.
  Замінюється на `127.0.0.1:20016`; ключ `res/loginapp_wot.pubkey` (PEM RSA-2048, SPKI)
  замінюється нашим публічним ключем. Обидві зміни зворотні (`wot-server setup/restore-client`).
- `res/loginapp_wot.pubkey` — публічний ключ, яким клієнт шифрує LoginApp-запит (саме тому
  приватний ключ Wargaming не потрібен: підмінюємо пару).
- `res/scripts/**` — Python 2.6 (pyc magic `D1 F2 0D 0A`, build 2011-05-26/06-29): 220 `.pyc`.
  Це **специфікація клієнтської поведінки**; декомпільовано 219/220 у
  `.scratch/research/decompiled/`.
- `res/scripts/entity_defs/*.def` — BigWorld entity-описи (packed XML); `Account.def` визначає
  властивості та client/base-методи.
- `res/scripts/item_defs/vehicles/**` — 167 XML: дерево досліджень, танки, компоненти.
- `res/text/LC_MESSAGES/*.mo` — локалізація (назви танків клієнт знає сам; серверу не потрібні).

## Робочий стан

Клієнт успішно запускається нативно (Windows 11, DX9) і рендерить екран логіна
(`World of Tanks v.0.6.5`, поля Email/Password, Connect). Скріншоти та UI-автоматизація:
`.scratch/screenshots/`, `.scratch/tools/`.

## Джерела

- `docs/research/client-analysis.md` — контракт клієнта (детально).
- `docs/research/protocol-scout.md` — що саме клієнт шле по дроту.
