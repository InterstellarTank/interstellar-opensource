use std::collections::HashMap;

use anyhow::{bail, ensure, Context, Result};
use bw201::stream::BinaryReader;
use flate2::{Decompress, FlushDecompress, Status};
use wot065_data::pickle::{self, Value};

const MAX_DOWNLOADS: usize = 16;
const MAX_COMPRESSED_BYTES: usize = 16 * 1024 * 1024;
const MAX_PICKLE_BYTES: usize = 64 * 1024 * 1024;

#[derive(Default)]
pub(crate) struct Downloads {
    pending: HashMap<i16, Download>,
}

#[derive(Default)]
struct Download {
    description: Option<(usize, i64)>,
    bytes: Vec<u8>,
    next_seq: u8,
    last: bool,
}

impl Downloads {
    pub(crate) fn header(&mut self, data: &[u8]) -> Result<Option<(i16, Value)>> {
        let mut reader = BinaryReader::new(data);
        let rid = reader.i16()?;
        let len = reader.packed_u32()? as usize;
        let description = reader.raw(len)?;
        ensure!(reader.is_empty(), "trailing bytes in resource {rid} header");
        let description = pickle::loads(description).context("invalid resource header pickle")?;
        let Value::Tuple(items) = description else {
            bail!("resource {rid} description must be a tuple");
        };
        let [Value::Int(len), Value::Int(crc)] = items.as_slice() else {
            bail!("resource {rid} description must contain a length and signed CRC32");
        };
        let len = usize::try_from(*len).context("invalid compressed resource length")?;
        ensure!(
            len <= MAX_COMPRESSED_BYTES && i32::try_from(*crc).is_ok(),
            "invalid length or CRC32 in resource {rid} header"
        );

        let stream = self.pending(rid)?;
        ensure!(
            stream.description.is_none(),
            "duplicate resource {rid} header"
        );
        ensure!(
            stream.bytes.len() <= len,
            "resource {rid} exceeds declared length"
        );
        stream.description = Some((len, *crc));
        self.finish(rid)
    }

    pub(crate) fn fragment(&mut self, data: &[u8]) -> Result<Option<(i16, Value)>> {
        let mut reader = BinaryReader::new(data);
        let rid = reader.i16()?;
        let seq = reader.u8()?;
        let flags = reader.u8()?;
        ensure!(flags <= 1, "invalid resource {rid} fragment flags {flags}");

        let stream = self.pending(rid)?;
        ensure!(!stream.last, "fragment after end of resource {rid}");
        ensure!(
            seq == stream.next_seq,
            "resource {rid} fragment sequence {seq}, expected {}",
            stream.next_seq
        );
        let limit = stream
            .description
            .map_or(MAX_COMPRESSED_BYTES, |(len, _)| len);
        ensure!(
            reader.remaining() <= limit - stream.bytes.len(),
            "resource {rid} exceeds compressed length limit"
        );
        stream.bytes.extend_from_slice(reader.rest());
        stream.next_seq = (seq + 1) % 255;
        stream.last = flags == 1;
        self.finish(rid)
    }

    fn pending(&mut self, rid: i16) -> Result<&mut Download> {
        ensure!(
            self.pending.len() < MAX_DOWNLOADS || self.pending.contains_key(&rid),
            "too many incomplete resource streams"
        );
        Ok(self.pending.entry(rid).or_default())
    }

    fn finish(&mut self, rid: i16) -> Result<Option<(i16, Value)>> {
        let stream = &self.pending[&rid];
        let Some((len, crc)) = stream.description.filter(|_| stream.last) else {
            return Ok(None);
        };
        let stream = self.pending.remove(&rid).expect("pending resource");
        ensure!(
            stream.bytes.len() == len,
            "resource {rid} declared {len} bytes, received {}",
            stream.bytes.len()
        );
        ensure!(
            pickle::signed_crc32(&stream.bytes) == crc,
            "resource {rid} CRC32 mismatch"
        );

        let raw = inflate_zlib(&stream.bytes, MAX_PICKLE_BYTES)
            .with_context(|| format!("invalid zlib data in resource {rid}"))?;
        let value =
            pickle::loads(&raw).with_context(|| format!("invalid pickle in resource {rid}"))?;
        Ok(Some((rid, value)))
    }
}

fn inflate_zlib(compressed: &[u8], max_bytes: usize) -> Result<Vec<u8>> {
    let mut decoder = Decompress::new(true);
    let mut output = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let start = decoder.total_in() as usize;
        let written_before = decoder.total_out();
        let status = decoder.decompress(&compressed[start..], &mut chunk, FlushDecompress::None)?;
        let written = (decoder.total_out() - written_before) as usize;
        ensure!(
            written <= max_bytes - output.len(),
            "decompressed resource exceeds size limit"
        );
        output.extend_from_slice(&chunk[..written]);
        if status == Status::StreamEnd {
            ensure!(
                decoder.total_in() as usize == compressed.len(),
                "trailing bytes after zlib stream"
            );
            return Ok(output);
        }
        ensure!(
            written != 0 || decoder.total_in() as usize != start,
            "incomplete zlib stream"
        );
    }
}

#[derive(Default)]
pub(crate) struct AccountCache {
    snapshot: Option<Value>,
}

impl AccountCache {
    pub(crate) fn revision(&self) -> Option<i64> {
        self.snapshot()?.get("rev")?.as_int()
    }

    pub(crate) fn snapshot(&self) -> Option<&Value> {
        self.snapshot.as_ref()
    }

    pub(crate) fn install(&mut self, mut full: Value) -> Result<()> {
        revision(&full, "rev")?;
        if let Value::Dict(entries) = &mut full {
            entries.retain(|(key, _)| key.as_bytes() != Some(b"prevRev"));
        }
        self.snapshot = Some(full);
        Ok(())
    }

    pub(crate) fn apply(&mut self, diff: Value) -> Result<bool> {
        let prev = revision(&diff, "prevRev")?;
        let rev = revision(&diff, "rev")?;
        ensure!(
            prev <= rev,
            "invalid Account revision change from {prev} to {rev}"
        );

        let Some(snapshot @ Value::Dict(entries)) = self.snapshot() else {
            return Ok(false);
        };
        let current = revision(snapshot, "rev")?;
        if rev <= current {
            return Ok(true);
        }
        if prev != current {
            return Ok(false);
        }

        let mut next = entries.clone();
        for (key, value) in into_dict(diff, "Account diff")? {
            match key.as_bytes() {
                Some(b"rev" | b"prevRev") => {}
                Some(b"inventory") => merge_sparse(
                    dict_entry(&mut next, key),
                    into_dict(value, "inventory diff")?,
                ),
                Some(b"stats") => {
                    merge_stats(dict_entry(&mut next, key), into_dict(value, "stats diff")?)?
                }
                Some(b"cache") => merge_cache(&mut next, into_dict(value, "cache diff")?)?,
                Some(b"account") => {
                    let account = dict_entry(&mut next, key);
                    for (key, value) in into_dict(value, "account metadata diff")? {
                        *entry(account, key) = value;
                    }
                }
                _ => *entry(&mut next, key) = value,
            }
        }
        *entry(&mut next, Value::str("rev")) = Value::Int(rev);
        self.snapshot = Some(Value::dict(next));
        Ok(true)
    }
}

type Dict = Vec<(Value, Value)>;

fn revision(value: &Value, key: &str) -> Result<i64> {
    match value.get(key) {
        Some(Value::Int(rev)) => Ok(*rev),
        _ => bail!("Account payload lacks integer {key}"),
    }
}

fn into_dict(value: Value, label: &str) -> Result<Dict> {
    match value {
        Value::Dict(items) => Ok(items),
        _ => bail!("{label} must be a dictionary"),
    }
}

fn entry(dict: &mut Dict, key: Value) -> &mut Value {
    if let Some(index) = dict.iter().position(|(existing, _)| *existing == key) {
        return &mut dict[index].1;
    }
    dict.push((key, Value::None));
    &mut dict.last_mut().expect("new dictionary entry").1
}

fn dict_entry(dict: &mut Dict, key: Value) -> &mut Dict {
    let value = entry(dict, key);
    if !matches!(value, Value::Dict(_)) {
        *value = Value::dict(Vec::new());
    }
    let Value::Dict(items) = value else {
        unreachable!()
    };
    items
}

fn merge_sparse(target: &mut Dict, changes: Dict) {
    for (key, value) in changes {
        match value {
            Value::None => target.retain(|(existing, _)| *existing != key),
            Value::Dict(nested) => merge_sparse(dict_entry(target, key), nested),
            _ => *entry(target, key) = value,
        }
    }
}

fn merge_stats(stats: &mut Dict, changes: Dict) -> Result<()> {
    for (key, value) in changes {
        match key.as_bytes() {
            Some(b"vehTypeXP") => {
                merge_sparse(dict_entry(stats, key), into_dict(value, "vehTypeXP")?)
            }
            Some(b"unlocks" | b"eliteVehicles" | b"doubleXPVehs") => {
                let Value::Set(additions) = value else {
                    bail!("growing Account stat must be a set");
                };
                let stored = entry(stats, key);
                if matches!(stored, Value::None) {
                    *stored = Value::set(Vec::new());
                }
                let Value::Set(members) = stored else {
                    bail!("cached growing Account stat must be a set");
                };
                for value in additions {
                    if !members.contains(&value) {
                        members.push(value);
                    }
                }
            }
            Some(name @ (b"unlocks_r" | b"eliteVehicles_r" | b"doubleXPVehs_r")) => {
                ensure!(
                    matches!(value, Value::Set(_)),
                    "replacement Account stat must be a set"
                );
                *entry(stats, Value::str(&name[..name.len() - 2])) = value;
            }
            _ => *entry(stats, key) = value,
        }
    }
    Ok(())
}

fn merge_cache(root: &mut Dict, changes: Dict) -> Result<()> {
    for (key, value) in changes {
        if key.as_bytes() == Some(b"vehsLock") {
            let mut locks = into_dict(value, "vehicle locks")?;
            for (_, reason) in &mut locks {
                match reason {
                    Value::Int(0) => *reason = Value::None,
                    Value::Int(_) | Value::None => {}
                    _ => bail!("vehicle lock reason must be an integer"),
                }
            }
            let inventory = dict_entry(root, Value::str("inventory"));
            let vehicles = dict_entry(inventory, Value::Int(1));
            merge_sparse(dict_entry(vehicles, Value::str("lock")), locks.clone());
            merge_sparse(
                dict_entry(dict_entry(root, Value::str("cache")), key),
                locks,
            );
        } else {
            *entry(dict_entry(root, Value::str("cache")), key) = value;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use bw201::stream::BinaryWriter;
    use wot065_data::ops::DiffBuilder;
    use wot065_data::pickle;

    use super::*;

    fn header(rid: i16, compressed: &[u8]) -> Vec<u8> {
        let mut writer = BinaryWriter::new();
        writer.i16(rid);
        writer.string_bytes(&pickle::stream_description(compressed));
        writer.as_slice().to_vec()
    }

    fn fragment(rid: i16, seq: u8, last: bool, bytes: &[u8]) -> Vec<u8> {
        let mut writer = BinaryWriter::new();
        writer.i16(rid);
        writer.u8(seq);
        writer.u8(u8::from(last));
        writer.raw(bytes);
        writer.as_slice().to_vec()
    }

    fn compress(value: &Value) -> Vec<u8> {
        pickle::zlib_compress(&pickle::dumps(value))
    }

    #[test]
    fn downloads_keep_interleaved_streams_independent_across_sequence_wrap() {
        let mut downloads = Downloads::default();
        let large = Value::str((0..=255u8).collect::<Vec<_>>());
        let bytes = compress(&large);
        assert!(bytes.len() > 255);
        assert_eq!(downloads.header(&header(1, &bytes)).unwrap(), None);

        let small = Value::Int(42);
        let small_bytes = compress(&small);
        for (idx, byte) in bytes.iter().enumerate() {
            let last = idx + 1 == bytes.len();
            let result = downloads
                .fragment(&fragment(1, (idx % 255) as u8, last, &[*byte]))
                .unwrap();
            if idx == 1 {
                assert_eq!(
                    downloads
                        .fragment(&fragment(-2, 0, true, &small_bytes))
                        .unwrap(),
                    None
                );
            }
            if idx == 2 {
                assert_eq!(
                    downloads.header(&header(-2, &small_bytes)).unwrap(),
                    Some((-2, small.clone()))
                );
            }
            assert_eq!(result, last.then(|| (1, large.clone())));
        }
    }

    #[test]
    fn downloads_reject_sequence_length_and_checksum_mismatches() {
        let compressed = compress(&Value::None);
        let mut corrupted = compressed.clone();
        corrupted[0] ^= 1;

        for (seq, bytes) in [
            (1, &compressed[..]),
            (0, &compressed[..2]),
            (0, &corrupted[..]),
        ] {
            let mut downloads = Downloads::default();
            downloads.header(&header(1, &compressed)).unwrap();
            assert!(downloads.fragment(&fragment(1, seq, true, bytes)).is_err());
        }
    }

    #[test]
    fn downloads_require_a_complete_zlib_frame_even_with_a_valid_crc() {
        let compressed = compress(&Value::None);
        let mut trailing = compressed.clone();
        trailing.push(0);

        for bytes in [&compressed[..compressed.len() - 1], &trailing[..]] {
            let mut downloads = Downloads::default();
            downloads.header(&header(1, bytes)).unwrap();
            assert!(downloads.fragment(&fragment(1, 0, true, bytes)).is_err());
        }
    }

    #[test]
    fn zlib_output_stops_at_the_configured_limit() {
        let raw = vec![0u8; 12_000];
        let compressed = pickle::zlib_compress(&raw);
        assert_eq!(inflate_zlib(&compressed, raw.len()).unwrap(), raw);
        assert!(inflate_zlib(&compressed, raw.len() - 1).is_err());
    }

    #[test]
    fn account_cache_reports_gaps_and_ignores_replayed_diffs() {
        let mut account = AccountCache::default();
        let mut diff = DiffBuilder::new();
        diff.credits = Some(100);
        assert!(!account.apply(diff.build(10, 11)).unwrap());

        let full = Value::sd(vec![
            ("rev", Value::Int(10)),
            ("stats", Value::sd(vec![("credits", Value::Int(75))])),
        ]);
        account.install(full.clone()).unwrap();
        assert!(!account.apply(diff.build(11, 12)).unwrap());
        assert_eq!(account.snapshot(), Some(&full));

        assert!(account.apply(diff.build(10, 11)).unwrap());
        diff.credits = Some(0);
        assert!(account.apply(diff.build(10, 11)).unwrap());
        assert!(account.apply(diff.build(8, 9)).unwrap());
        assert_eq!(account.revision(), Some(11));
        let stats = account.snapshot().unwrap().get("stats").unwrap();
        assert_eq!(stats.get("credits"), Some(&Value::Int(100)));
    }

    #[test]
    fn inventory_merges_nested_layouts_and_removes_tombstones() {
        let mut account = AccountCache::default();
        let other_layout = Value::tuple(vec![Value::Int(1), Value::Int(3)]);
        let old_ammo = Value::list(vec![Value::Int(8), Value::Int(10)]);
        let vehicles = Value::sd(vec![
            (
                "compDescr",
                Value::dict(vec![
                    (Value::Int(5), Value::str("keep")),
                    (Value::Int(7), Value::str("sell")),
                ]),
            ),
            (
                "shellsLayout",
                Value::dict(vec![(
                    Value::Int(5),
                    Value::dict(vec![(other_layout.clone(), old_ammo.clone())]),
                )]),
            ),
        ]);
        account
            .install(Value::sd(vec![
                ("rev", Value::Int(10)),
                ("inventory", Value::dict(vec![(Value::Int(1), vehicles)])),
            ]))
            .unwrap();

        let mut diff = DiffBuilder::new();
        diff.delete_vehicles.insert(7);
        diff.veh_shells_layout
            .insert(5, Some(vec![((1, 2), vec![(8, 101)])]));
        assert!(account.apply(diff.build(10, 11)).unwrap());
        let vehicles = account
            .snapshot()
            .unwrap()
            .get("inventory")
            .unwrap()
            .get_int(1)
            .unwrap();
        assert_eq!(
            vehicles.get("compDescr"),
            Some(&Value::dict(vec![(Value::Int(5), Value::str("keep"))]))
        );
        assert_eq!(
            vehicles.get("shellsLayout").unwrap().get_int(5),
            Some(&Value::dict(vec![
                (other_layout, old_ammo),
                (
                    Value::tuple(vec![Value::Int(1), Value::Int(2)]),
                    Value::list(vec![Value::Int(8), Value::Int(101)])
                ),
            ]))
        );
    }

    #[test]
    fn stats_diff_matches_set_xp_and_nullable_field_semantics() {
        let mut account = AccountCache::default();
        account
            .install(Value::sd(vec![
                ("rev", Value::Int(10)),
                (
                    "stats",
                    Value::sd(vec![
                        ("unlocks", Value::set(vec![Value::Int(1)])),
                        ("doubleXPVehs", Value::set(vec![Value::Int(2)])),
                        (
                            "vehTypeXP",
                            Value::dict(vec![
                                (Value::Int(1), Value::Int(50)),
                                (Value::Int(2), Value::Int(10)),
                            ]),
                        ),
                        (
                            "clanInfo",
                            Value::dict(vec![(Value::str("name"), Value::str("old"))]),
                        ),
                    ]),
                ),
            ]))
            .unwrap();

        assert!(account
            .apply(Value::sd(vec![
                ("prevRev", Value::Int(10)),
                ("rev", Value::Int(11)),
                (
                    "stats",
                    Value::sd(vec![
                        ("unlocks", Value::set(vec![Value::Int(1), Value::Int(2)])),
                        ("doubleXPVehs_r", Value::set(vec![Value::Int(3)])),
                        ("vehTypeXP", Value::dict(vec![(Value::Int(1), Value::None)])),
                        ("clanInfo", Value::None),
                    ])
                ),
            ]))
            .unwrap());
        let stats = account.snapshot().unwrap().get("stats").unwrap();
        assert_eq!(
            stats.get("unlocks"),
            Some(&Value::set(vec![Value::Int(1), Value::Int(2)]))
        );
        assert_eq!(
            stats.get("doubleXPVehs"),
            Some(&Value::set(vec![Value::Int(3)]))
        );
        assert_eq!(stats.get("doubleXPVehs_r"), None);
        assert_eq!(
            stats.get("vehTypeXP"),
            Some(&Value::dict(vec![(Value::Int(2), Value::Int(10))]))
        );
        assert_eq!(stats.get("clanInfo"), Some(&Value::None));
    }

    #[test]
    fn cache_lock_updates_change_the_vehicle_lock_map() {
        let mut account = AccountCache::default();
        account
            .install(Value::sd(vec![("rev", Value::Int(10))]))
            .unwrap();
        let mut locks = DiffBuilder::new();
        locks.veh_lock(5, 2).veh_lock(7, 4);
        assert!(account.apply(locks.build(10, 11)).unwrap());

        let mut clear = DiffBuilder::new();
        clear.veh_lock(5, 0);
        assert!(account.apply(clear.build(11, 12)).unwrap());
        let snapshot = account.snapshot().unwrap();
        let locks = snapshot
            .get("inventory")
            .unwrap()
            .get_int(1)
            .unwrap()
            .get("lock")
            .unwrap();
        assert_eq!(locks.get_int(5), None);
        assert_eq!(locks.get_int(7), Some(&Value::Int(4)));
        let cache_locks = snapshot.get("cache").unwrap().get("vehsLock").unwrap();
        assert_eq!(cache_locks.get_int(7), Some(&Value::Int(4)));
    }

    #[test]
    fn rejected_diff_leaves_the_snapshot_in_its_original_revision() {
        let mut account = AccountCache::default();
        let full = Value::sd(vec![("rev", Value::Int(10))]);
        account.install(full.clone()).unwrap();
        let malformed = Value::sd(vec![
            ("prevRev", Value::Int(10)),
            ("rev", Value::Int(11)),
            (
                "inventory",
                Value::dict(vec![(
                    Value::Int(6),
                    Value::dict(vec![(Value::Int(4), Value::Int(3))]),
                )]),
            ),
            ("stats", Value::sd(vec![("unlocks", Value::Int(4))])),
        ]);
        assert!(account.apply(malformed).is_err());
        assert_eq!(account.snapshot(), Some(&full));
    }
}
