---
updated: 2026-11-11
sources: [docs/research/protocol-scout.md]
---

# BigWorld Engine 2.0.1

Рушій, на якому побудований WoT 0.6.5 (травень–червень 2011). Його **публічне C++ джерело**
(зеркало `v2v3v4/BigWorld-Engine-2.0.1`) виявилося вирішальним: connection layer клієнта 0.6.5
збігається зі стоковим 2.0.1 1:1.

## Що дає джерело

- `src/lib/connection/`: `login_interface.hpp` (LOGIN_VERSION=56), `log_on_params.{hpp,cpp}`,
  `login_reply_record.hpp`, `rsa_stream_encoder.hpp`, `baseapp_ext_interface.hpp`,
  `client_interface.hpp`, `server_connection.cpp`, `login_handler.cpp`.
- `src/lib/network/`: `packet.{hpp,cpp}` (фреймінг, прапорці, footers), `bundle.cpp` (повідомлення,
  requests/replies), `interface_element.cpp` (кодування довжин), `encryption_filter.cpp`
  (Blowfish+XOR), `public_key_cipher.cpp` (RSA-OAEP, chunk 214), `binarytypes`.
- `src/lib/cstdmf/binary_stream.hpp` (packed int, рядки), `src/server/loginapp/*` (серверна логіка
  LoginApp), `src/lib/entitydef/*` (entity/property/method id).
- `src/client/connection_control.cpp` — точний порядок викликів клієнта при логіні.

## Локальна копія

`.scratch/research/protocol-scout/repos/bw201/` (read-only для нас; ліцензія BigWorld —
пропрієтарна, тому код **не** копіюється в репо: лише формати й поведінка відтворюються
в Rust-реалізації).

## Висновок

Реверс протоколу не потребує дизасемблювання бінарника: специфікація береться з джерел 2.0.1,
а валідація — з реального клієнта й таблиць інтерфейсів у `WorldOfTanks.exe`.

## Джерела

- `docs/research/protocol-scout.md`
- `.scratch/research/protocol-scout/repos/bw201/`
