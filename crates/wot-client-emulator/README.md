# Емулятор клієнта WoT 0.6.5

`wot-client-emulator` підключається до запущеного сервера через LoginApp і BaseApp. Він відтворює UDP-поведінку клієнта, розбирає відповіді та повертає ненульовий код виходу, якщо перевірка не пройшла.

## Запуск

Потрібен запущений `wot-server serve` з ресурсами клієнта 0.6.5. `--game-dir` читає версію, список сутностей і публічний ключ з установленої гри та звіряє шлях до карти в бойовому тесті. Якщо сервер працює з окремим RSA-ключем, передай відповідний PEM у `--pub-key`. LoginApp за замовчуванням слухає UDP на `127.0.0.1:20016`. Іншу адресу можна вказати через `--login`.

```powershell
$game = 'C:\Users\qwerty\Documents\interstellar-client-065'
cargo run -p wot-client-emulator -- --game-dir $game hangar --repeat 5
cargo run -p wot-client-emulator -- --game-dir $game battle --map 3
cargo run -p wot-client-emulator -- --game-dir $game scenario crates/wot-client-emulator/examples/hangar.json --json
```

- `hangar` перевіряє RSA/Blowfish логін, `showGUI`, Account Sync і ревізію, дескриптори машин, shop cache, dossiers та `receiveServerStats`.
- `battle` ставить доступну машину в Random, отримує Avatar, geometry mapping і Vehicle через AoI, чекає старту та перевіряє рух і tracer власного пострілу. Після цього перевіряє повернення до того самого акаунта. `--map` перевіряє ID призначеної карти. `--wait-for-results` також перевіряє `onVehicleLeftArena` та лічильник боїв у dossier. Для повного бою задай відповідний таймаут, наприклад `--timeout 1200`.
- `scenario` виконує кроки з JSON, очікує відповіді або події та перевіряє отримані значення.

Кожен запуск генерує новий тестовий акаунт. `--username` використовує той самий акаунт між запусками. Для тестів, які змінюють економіку або стан машин, можна запустити сервер з окремим `--data-dir`. Після раннього виходу з Random сервер тримає машину заблокованою до підбиття результатів.

Для перевірки revision cache запускай сервер із даними AccountStore. Фікстури, які повертають той самий повний stream на кожен запит, викликають помилку перевірки.

## JSON-сценарії

Файл містить об’єкт із масивом `steps`. Повний приклад перевіряє callbacks статистики та черги. [hangar.json](examples/hangar.json)

| `action` | Поля та поведінка |
| --- | --- |
| `wait_hangar` | Чекає Account Sync і `showGUI`. |
| `command` | `id`, `args`, `expect`. Очікує зазначений код результату. `reply` доступний для перевірки, якщо відповідь мала pickle або resource stream. |
| `notify` | `id`, `args`. Надсилає команду, яка повідомляє результат через callback. |
| `base` | `method`, `payload_hex`. Викликає exposed BaseMethod. |
| `cell` | `entity` як числовий ID, `"player"` або `"own_vehicle"`, `method` як exposed index і `payload_hex`. |
| `wait_event` | `id` та опційні `phase` (`loading`, `hangar`, `arena`), `player_method`, `offset`, `prefix_hex`. Читає події, отримані після останнього надсилання або попереднього `wait_event`. `offset: 4` пропускає EntityID у повідомленні ClientMethod. |
| `wait_arena_period` | `period`, наприклад `3` для активного бою або `4` для підбиття результатів. |
| `move`, `aim`, `shoot`, `leave` | `move` приймає `flags`, `aim` приймає `point: [x, y, z]`. |
| `poll` | `ms`. Приймає пакети протягом заданого часу в межах `--timeout`. |
| `assert` | `source` (`account`, `shop`, `dossiers`, `show_gui`, `reply`), `path` і `equals`. Число в шляху позначає ключ словника або індекс масиву. |

`args` використовує одну з форм `{"int3":[0,0,0]}`, `{"int4":[0,0,0,0]}`, `{"str":"text"}`, `{"int2_str":[0,0,"text"]}` або `{"int_arr":[1,2]}`. За замовчуванням передається `int3` з трьома нулями. ID і формати можна звірити з поточним [protocol.rs](../wot-baseapp/src/protocol.rs).

Бібліотечний `Client` дозволяє керувати кількома сесіями у власному Rust-тесті та перевіряти стан Account, Avatar і AoI. Для перевірки графіки, звуку та Scaleform потрібен оригінальний клієнт.
