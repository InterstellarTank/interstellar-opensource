---
updated: 2026-11-11
sources: [docs/research/gamedata-analysis.md]
---

# Packed XML (BWXML)

Формат, у якому WoT 0.6.5 зберігає **всі** `res/scripts/**/*.xml` та `entity_defs/*.def`
(а також `engine_config.xml`, `scripts_config.xml`, `res/gui/*.xml`). Це не текстовий XML, а
бінарний «стиснутий» XML BigWorld.

## Формат

```
магія      4 байти   45 4E A1 62  ("EN\xa1b")
версія     1 байт    завжди 0 (у спостережених файлах)
словник    послідовність NUL-terminated UTF-8 рядків; порожній рядок = кінець
документ   один Element
```

Element:
```
u16 children_count
u32 self_data_descriptor
children_count * (u16 name_index, u32 data_descriptor)
self value data   (довжина = self.end_offset)
children data     (кожна довжина = child.end_offset - попередній)
```

Data descriptor: `(type << 28) | (end_offset & 0x0FFFFFFF)`; типи:
0=Element, 1=String, 2=Integer, 3=Vector(f32[]), 4=Boolean, 5=CompressedString (base64-in-raw).

## Кодек у нас

- Python-референс: `.scratch/research/bwxml/bwxml.py` (stdlib), перевірено round-trip на
  **248 файлах** гри біт-у-біт (`unpack-tree` / `verify-tree`).
- Rust-референс: вендорений `vendor/wg-toolkit-rs/wg-toolkit/src/pxml/` (MIT), та сама магія
  й формат; використовується з `src/deploy.rs` для патчу `scripts_config.xml`.
- Rust-крейт `crates/wot065-data` має власний рідер для даних танків.

## Навіщо це серверу

1. `scripts_config.xml` — єдине місце, де клієнт бере адресу LoginApp і шлях до публічного ключа;
   сервер має перепакувати його на `127.0.0.1:20016` (робить `wot-server setup`).
2. `entity_defs/*.def` — джерело істини для entity-таблиць: порядок property/method визначає
   числові id на дроті (використовується BaseApp).
3. `item_defs/vehicles/**` — каталог танків і stock-компонентів для інвентаря ангара.
4. `entities.xml` — порядок entity-типів (Account = 1-ша, отже type id 1).

## Джерела

- `docs/research/gamedata-analysis.md` — специфікація і статус розпакування.
- `.scratch/research/unpacked/` — усі 248 файлів у читабельному XML.
- `vendor/wg-toolkit-rs/wg-toolkit/src/pxml/{de,ser,value}.rs` — Rust-реалізація.
