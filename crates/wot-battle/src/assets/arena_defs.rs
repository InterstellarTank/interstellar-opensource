//! WoT 0.6.5 `scripts/arena_defs/` catalog parser.

use std::path::Path;

use wot065_data::xml::{self, XmlNode};

use crate::assets::AssetError;

/// Parsed definition for one battle map (`scripts/arena_defs/<name>.xml` merged
/// with `_common_.xml`).
#[derive(Debug, Clone, PartialEq)]
pub struct ArenaDef {
    pub id: i32,
    pub name: String,
    pub display_name_key: String,
    pub description_key: String,
    pub geometry: String,
    /// `(bottomLeft [x, z], upperRight [x, z])` in meters.
    pub bounding_box: ([f32; 2], [f32; 2]),
    pub min_players_in_team: u32,
    pub max_players_in_team: u32,
    pub round_length_secs: u32,
    pub arena_start_delay_secs: f32,
    pub kick_after_finish_wait_secs: f32,
    pub minimap_texture: Option<String>,
}

/// Catalog of all battle maps declared in `scripts/arena_defs/_list_.xml`.
#[derive(Debug, Clone)]
pub struct ArenaDefsCatalog {
    pub maps: Vec<ArenaDef>,
}

impl ArenaDefsCatalog {
    pub fn load(res_dir: &Path) -> Result<Self, AssetError> {
        let defs_dir = res_dir.join("scripts/arena_defs");
        let list_xml = xml::read_xml(&defs_dir.join("_list_.xml"))?;
        let common_xml = xml::read_xml(&defs_dir.join("_common_.xml"))?;

        let mut maps = Vec::new();
        for map_entry in &list_xml.children {
            let id = map_entry
                .child("id")
                .and_then(|n| n.parse_i64())
                .ok_or_else(|| AssetError::ArenaDef("missing <id> in _list_.xml".into()))?
                as i32;
            let name = map_entry
                .child("name")
                .map(|n| n.text().to_string())
                .filter(|s| !s.is_empty())
                .ok_or_else(|| AssetError::ArenaDef("missing <name> in _list_.xml".into()))?;
            let map_xml = xml::read_xml(&defs_dir.join(format!("{name}.xml")))?;

            let geometry = read_str_with_common(&map_xml, &common_xml, "geometry")?;
            let display_name_key = read_str_with_common(&map_xml, &common_xml, "name")?;
            let description_key = read_str_with_common(&map_xml, &common_xml, "description")?;
            let min_players_in_team =
                read_i64_with_common(&map_xml, &common_xml, "minPlayersInTeam")? as u32;
            let max_players_in_team =
                read_i64_with_common(&map_xml, &common_xml, "maxPlayersInTeam")? as u32;
            let round_length_secs =
                read_i64_with_common(&map_xml, &common_xml, "roundLength")? as u32;
            let arena_start_delay_secs =
                read_f64_with_common(&map_xml, &common_xml, "arenaStartDelay")? as f32;
            let kick_after_finish_wait_secs =
                read_f64_with_common(&map_xml, &common_xml, "kickAfterFinishWaitTime")? as f32;

            let bbox = map_xml
                .child("boundingBox")
                .ok_or_else(|| AssetError::ArenaDef(format!("{name}: missing <boundingBox>")))?;
            let bl = parse_vec2(
                bbox.child("bottomLeft")
                    .map(|n| n.text())
                    .ok_or_else(|| AssetError::ArenaDef(format!("{name}: missing bottomLeft")))?,
            )?;
            let ur = parse_vec2(
                bbox.child("upperRight")
                    .map(|n| n.text())
                    .ok_or_else(|| AssetError::ArenaDef(format!("{name}: missing upperRight")))?,
            )?;
            if bl[0] >= ur[0] || bl[1] >= ur[1] {
                return Err(AssetError::ArenaDef(format!(
                    "{name}: invalid boundingBox {bl:?}..{ur:?}"
                )));
            }

            let minimap_texture = map_xml
                .child("minimapConfig")
                .and_then(|mc| mc.child("spaceMap"))
                .map(|n| n.text().to_string())
                .filter(|s| !s.is_empty());

            maps.push(ArenaDef {
                id,
                name,
                display_name_key,
                description_key,
                geometry,
                bounding_box: (bl, ur),
                min_players_in_team,
                max_players_in_team,
                round_length_secs,
                arena_start_delay_secs,
                kick_after_finish_wait_secs,
                minimap_texture,
            });
        }

        Ok(Self { maps })
    }

    pub fn by_id(&self, id: i32) -> Option<&ArenaDef> {
        self.maps.iter().find(|m| m.id == id)
    }

    pub fn by_name(&self, name: &str) -> Option<&ArenaDef> {
        self.maps.iter().find(|m| m.name == name)
    }
}

fn read_str_with_common(map: &XmlNode, common: &XmlNode, key: &str) -> Result<String, AssetError> {
    map.child(key)
        .or_else(|| common.child(key))
        .map(|n| n.text().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AssetError::ArenaDef(format!("missing string key `{key}`")))
}

fn read_i64_with_common(map: &XmlNode, common: &XmlNode, key: &str) -> Result<i64, AssetError> {
    map.child(key)
        .and_then(|n| n.parse_i64())
        .or_else(|| common.child(key).and_then(|n| n.parse_i64()))
        .ok_or_else(|| AssetError::ArenaDef(format!("missing int key `{key}`")))
}

fn read_f64_with_common(map: &XmlNode, common: &XmlNode, key: &str) -> Result<f64, AssetError> {
    map.child(key)
        .and_then(|n| n.parse_f64())
        .or_else(|| common.child(key).and_then(|n| n.parse_f64()))
        .ok_or_else(|| AssetError::ArenaDef(format!("missing float key `{key}`")))
}

fn parse_vec2(text: &str) -> Result<[f32; 2], AssetError> {
    let mut parts = text.split_whitespace();
    let x: f32 = parts
        .next()
        .ok_or_else(|| AssetError::ArenaDef("missing x in vec2".into()))?
        .parse()
        .map_err(|e| AssetError::ArenaDef(format!("bad vec2 x: {e}")))?;
    let z: f32 = parts
        .next()
        .ok_or_else(|| AssetError::ArenaDef("missing z in vec2".into()))?
        .parse()
        .map_err(|e| AssetError::ArenaDef(format!("bad vec2 z: {e}")))?;
    Ok([x, z])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_synthetic_arena_defs_with_common_fallback() {
        let tmp = Path::new("target/test-tmp/arena_defs_synth");
        let defs_dir = tmp.join("scripts/arena_defs");
        std::fs::create_dir_all(&defs_dir).unwrap();
        std::fs::write(
            defs_dir.join("_list_.xml"),
            "<root><map><id>3</id><name>test_map</name></map></root>",
        )
        .unwrap();
        std::fs::write(
            defs_dir.join("_common_.xml"),
            "<root><minPlayersInTeam>1</minPlayersInTeam><maxPlayersInTeam>15</maxPlayersInTeam><roundLength>3600</roundLength><arenaStartDelay>30.0</arenaStartDelay><kickAfterFinishWaitTime>60.0</kickAfterFinishWaitTime></root>",
        )
        .unwrap();
        std::fs::write(
            defs_dir.join("test_map.xml"),
            "<root><name>#arenas:test/name</name><description>#arenas:test/desc</description><geometry>spaces/test_map</geometry><boundingBox><bottomLeft>-300.0 -300.0</bottomLeft><upperRight>400.0 400.0</upperRight></boundingBox><roundLength>900</roundLength><minimapConfig><spaceMap>spaces/test_map/mmap.dds</spaceMap></minimapConfig></root>",
        )
        .unwrap();

        let catalog = ArenaDefsCatalog::load(tmp);
        let _ = std::fs::remove_dir_all(tmp);
        let catalog = catalog.unwrap();
        assert_eq!(catalog.maps.len(), 1);
        let m = catalog.by_id(3).unwrap();
        assert_eq!(m.round_length_secs, 900);
        assert_eq!(m.min_players_in_team, 1);
        assert_eq!(m.max_players_in_team, 15);
        assert_eq!(m.bounding_box, ([-300.0, -300.0], [400.0, 400.0]));
    }
}
