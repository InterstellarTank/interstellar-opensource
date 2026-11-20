---
updated: 2026-11-11
sources: [docs/research/client-analysis.md, docs/research/vehicle-catalog.json]
---

# Compact descriptor

У 0.6.5 предмети (танки, модулі, снаряди) на дроті та в інвентарі ідентифікуються
«компактними дескрипторами» — числами/бінарними блобами фіксованого формату.

## Тип предмета

`common/items/__init__.py`: 1 vehicle, 2 chassis, 3 turret, 4 gun, 5 engine, 6 fuelTank, 7 radio,
8 tankman, 9 optionalDevice, 10 shell, 11 equipment.

## `vehTypeCompDescr` (int32)

Використовується в stats (`vehTypeXP`, `unlocks`, `eliteVehicles`, `doubleXPVehs`):

```
vehTypeCompDescr = (vehicleTypeID << 8) | (nationID << 4) | 1
```

Нації: ussr = 0, germany = 1, usa = 2. Приклад: germany PzIV (typeID 0) → `17`.

## Інвентарний `compDescr` танка (15 байт)

Саме його читає ангар (`inventory[1]['compDescr'][vehInvID]`):

```
struct.pack('<2B4H', 1 | (nationID << 4), vehicleTypeID, chassisID, engineID,
            fuelTankID, radioID)
+ struct.pack('<2H', turretID, gunID)
+ b'\x00'                       # flags: optional devices/emblem/horn/camo
```

Усі 130 танків 0.6.5 мають рівно одну turret position, тому довжина завжди 15 байт.
Джерело: `common/items/vehicles.py` (`makeCompactDescr`, `__initFromCompactDescr`).

## Каталог

Повний каталог з обома кодуваннями: `.scratch/research/decompiled/_generated/vehicles_catalog.json`
(+ `.csv`), 130 типів, перевірено проти розпакованих `item_defs`. Rust-версія каталогу —
`crates/wot065-data`.

## Джерела

- `docs/research/client-analysis.md` (§F).
- `docs/research/vehicle-catalog.json` — каталог, згенерований з файлів гри.
