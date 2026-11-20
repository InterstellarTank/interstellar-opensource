---
updated: 2026-11-11
sources: [docs/research/client-analysis.md]
---

# Account sync (0.6.5)

У WoT 0.6.5 гроші, досвід, слоти й **увесь інвентар танків** не передаються як entity properties.
Вони приходять одним pickled-блобом у відповідь на клієнтську команду `CMD_SYNC_DATA`.

## Потік

1. Сервер створює базову сутність `Account` (тип 1), надсилає client-видимі властивості
   `name`, `normalizedName` (STRING) і `serverSettings` (PYTHON-дикт, обов'язково
   `'vivoxDomain': ''`).
2. Сервер викликає `Account.showGUI(pickle({'databaseID': 1, 'serverUTC': <epoch>}))` —
   клієнт показує лоадинг/лobby.
3. Клієнт у `onBecomePlayer()` надсилає base-метод `doCmdInt3(rid, cmd, a0, a1, a2)`:
   - `rid, 100 (CMD_SYNC_DATA), revHi, revLo, 0`
   - `rid, 300 (CMD_SYNC_SHOP), shopRev, 0, 0`
   - `rid, 600 (CMD_SYNC_DOSSIERS), ver, maxChangeTime, 0`
4. Відповіді:
   - **CMD_SYNC_DATA**: `onCmdResponseExt(rid, RES_SUCCESS=0, pickle(full_diff))` — одноразово,
     без стріму (клієнтський `SyncController` приймає будь-який `resultID != RES_STREAM`).
     `full_diff` мусить містити `prevRev: 0` (поточна ревізія клієнта) і `rev: N`.
   - **CMD_SYNC_SHOP**: обов'язково `onCmdResponse(rid, RES_STREAM=1)` + BigWorld resource stream
     (id=rid): payload = `zlib.compress(pickle(shop_cache))`, desc = `pickle((len, crc32))`.
     Shop кеш ставиться лише з callback'а завершення стріму.
   - **CMD_SYNC_DOSSIERS**: так само стрім, payload `zlib(pickle((version:int, [])))`.
5. Кожна відповідь має повернути той самий `requestID` (клієнт алокує їх з 120).

## Структура full_diff

```
{
  'rev': int, 'prevRev': 0,
  'inventory': {
      1:  { 'compDescr': {vehInvID: <15-byte blob>}, 'shells':{}, 'shellsLayout':{},
            'crew':{}, 'repair':{}, 'eqsLayout':{}, 'eqs':{}, 'settings':{}, 'lock':{} },
      8:  {'compDescr':{}, 'vehicle':{}},
      9: {}, 10: {}, 11: {},
  },
  'stats': {credits, gold, slots, berths, freeXP, dossier, clanInfo, vehTypeXP, unlocks,
            eliteVehicles, doubleXPVehs, ...},
  'cache': {'battlesTillCaptcha': 0},
  'account': {clanDBID, attrs, premiumExpiryTime, autoBanTime, restrictions},
  'offers': {'in': {}, 'out': {}},
}
```

`vehInvID -> compDescr` — саме те, що будує список танків в ангарі (`Hangar.py` через
`Inventory.getItems(1)`); невалідний descriptor танк **тихо відкидається**, тому всі 130 типів
мають бути валідними (див. [[concepts/compact-descriptor]]).

## Джерела

- `docs/research/client-analysis.md` — повний контракт з посиланнями на декомпільовані файли.
- Референсні байт-точні payload'и: `.scratch/research/decompiled/_generated/`.
- Декомпільовано: `client/Account.py`, `client/AccountSyncData.py`, `client/SyncController.py`,
  `client/account_helpers/*.py`, `common/AccountCommands.py`.
