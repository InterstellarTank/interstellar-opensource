//! WoT 0.6.5 `scripts/destructibles.xml` parser and BigWorld chunk ID helpers.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use glam::Vec3;
use wot065_data::xml::{self, XmlNode};

use crate::assets::AssetError;

pub const DESTR_TYPE_TREE: u8 = 0;
pub const DESTR_TYPE_FALLING_ATOM: u8 = 1;
pub const DESTR_TYPE_FRAGILE: u8 = 2;
pub const DESTR_TYPE_STRUCTURE: u8 = 3;

pub const DESTRUCTIBLE_MATKIND_NORMAL_MIN: u8 = 73;
pub const DESTRUCTIBLE_MATKIND_NORMAL_MAX: u8 = 86;
pub const DESTRUCTIBLE_MATKIND_DAMAGED_MIN: u8 = 87;

/// Physical topple parameters from `<physicParams>` (7 floats).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DestructiblePhysicParams {
    pub mass: f32,
    pub height: f32,
    pub spring_angle: f32,
    pub spring_stiffness: f32,
    pub spring_resist: f32,
    pub air_resist: f32,
    pub bury_depth: f32,
}

/// One destructible sub-module on a multi-part structure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructureModule {
    pub mat_kind: u8,
    pub destroyed_mat_kind: u8,
    pub surface: String,
    pub module_id: u16,
    pub health: i32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DestructibleKind {
    Fragile {
        health: i32,
        kinetic_damage_correction: f32,
    },
    FallingAtom {
        health: i32,
        kinetic_damage_correction: f32,
        physic_params: DestructiblePhysicParams,
    },
    Tree {
        health: i32,
        density: f32,
        kinetic_damage_correction: f32,
        physic_params: DestructiblePhysicParams,
    },
    Structure {
        modules: BTreeMap<u8, StructureModule>,
        destroy_depends: BTreeMap<u8, Vec<u8>>,
        state_presets: BTreeMap<String, Vec<u8>>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct DestructibleDesc {
    pub id: usize,
    pub filename: String,
    pub kind: DestructibleKind,
}

/// Catalog loaded from `scripts/destructibles.xml`.
#[derive(Debug, Clone)]
pub struct DestructiblesCatalog {
    pub default_lifetime_effect_chance: f32,
    pub unit_vehicle_mass: f32,
    pub entries: Vec<DestructibleDesc>,
    by_filename: BTreeMap<String, usize>,
}

impl DestructiblesCatalog {
    pub fn load(res_dir: &Path) -> Result<Self, AssetError> {
        let bytes = std::fs::read(res_dir.join("scripts/destructibles.xml"))?;
        Self::parse(&bytes)
    }

    pub fn parse(bytes: &[u8]) -> Result<Self, AssetError> {
        let root = xml::parse_xml_bytes(bytes)?;
        let default_lifetime_effect_chance = root
            .child("defaultLifetimeEffectChance")
            .and_then(|n| n.parse_f64())
            .unwrap_or(1.0) as f32;
        let unit_vehicle_mass = root
            .child("unitVehicleMass")
            .and_then(|n| n.parse_f64())
            .unwrap_or(28_000.0) as f32;

        let mut entries = Vec::new();
        let mut by_filename = BTreeMap::new();

        // Preserve DestructiblesCache.py order: fragiles -> fallingAtoms -> trees -> structures.
        if let Some(sec) = root.child("fragiles") {
            for child in &sec.children {
                let id = entries.len();
                let desc = parse_fragile(id, child)?;
                by_filename.insert(desc.filename.clone(), id);
                entries.push(desc);
            }
        }
        if let Some(sec) = root.child("fallingAtoms") {
            for child in &sec.children {
                let id = entries.len();
                let desc = parse_falling_atom(id, child)?;
                by_filename.insert(desc.filename.clone(), id);
                entries.push(desc);
            }
        }
        if let Some(sec) = root.child("trees") {
            for child in &sec.children {
                let id = entries.len();
                let desc = parse_tree(id, child)?;
                by_filename.insert(desc.filename.clone(), id);
                entries.push(desc);
            }
        }
        if let Some(sec) = root.child("structures") {
            for child in &sec.children {
                let id = entries.len();
                let desc = parse_structure(id, child)?;
                by_filename.insert(desc.filename.clone(), id);
                entries.push(desc);
            }
        }

        Ok(Self {
            default_lifetime_effect_chance,
            unit_vehicle_mass,
            entries,
            by_filename,
        })
    }

    pub fn by_id(&self, id: usize) -> Option<&DestructibleDesc> {
        self.entries.get(id)
    }

    pub fn by_filename(&self, filename: &str) -> Option<&DestructibleDesc> {
        if let Some(&id) = self.by_filename.get(filename) {
            return self.entries.get(id);
        }
        let normalized = normalize_lod0(filename);
        self.by_filename
            .get(&normalized)
            .and_then(|&id| self.entries.get(id))
    }
}

fn parse_fragile(id: usize, node: &XmlNode) -> Result<DestructibleDesc, AssetError> {
    let filename = read_filename(node)?;
    let health = node.child("health").and_then(|n| n.parse_i64()).unwrap_or(0) as i32;
    let kinetic_damage_correction = node
        .child("kineticDamageCorrection")
        .and_then(|n| n.parse_f64())
        .unwrap_or(0.0) as f32;
    Ok(DestructibleDesc {
        id,
        filename,
        kind: DestructibleKind::Fragile {
            health,
            kinetic_damage_correction,
        },
    })
}

fn parse_falling_atom(id: usize, node: &XmlNode) -> Result<DestructibleDesc, AssetError> {
    let filename = read_filename(node)?;
    let health = node.child("health").and_then(|n| n.parse_i64()).unwrap_or(0) as i32;
    let kinetic_damage_correction = node
        .child("kineticDamageCorrection")
        .and_then(|n| n.parse_f64())
        .unwrap_or(0.0) as f32;
    let physic_params = parse_physic_params(node, &filename)?;
    Ok(DestructibleDesc {
        id,
        filename,
        kind: DestructibleKind::FallingAtom {
            health,
            kinetic_damage_correction,
            physic_params,
        },
    })
}

fn parse_tree(id: usize, node: &XmlNode) -> Result<DestructibleDesc, AssetError> {
    let filename = read_filename(node)?;
    let health = node.child("health").and_then(|n| n.parse_i64()).unwrap_or(0) as i32;
    let density = node
        .child("density")
        .and_then(|n| n.parse_f64())
        .unwrap_or(0.0) as f32;
    let kinetic_damage_correction = node
        .child("kineticDamageCorrection")
        .and_then(|n| n.parse_f64())
        .unwrap_or(0.0) as f32;
    let physic_params = parse_physic_params(node, &filename)?;
    Ok(DestructibleDesc {
        id,
        filename,
        kind: DestructibleKind::Tree {
            health,
            density,
            kinetic_damage_correction,
            physic_params,
        },
    })
}

fn parse_structure(id: usize, node: &XmlNode) -> Result<DestructibleDesc, AssetError> {
    let filename = read_filename(node)?;
    let modules_sec = node
        .child("modules")
        .ok_or_else(|| AssetError::Destructibles(format!("{filename}: missing <modules>")))?;

    let mut raw_by_mod_id: BTreeMap<u16, (String, i32, Vec<u16>)> = BTreeMap::new();
    for mod_node in &modules_sec.children {
        let Some(mat_name) = mod_node.child("matName").map(|n| n.text()) else {
            continue;
        };
        let Some((surface, mod_id, deps)) = parse_material_name(mat_name) else {
            continue;
        };
        let health = mod_node
            .child("health")
            .and_then(|n| n.parse_i64())
            .unwrap_or(0) as i32;
        raw_by_mod_id.insert(mod_id, (surface, health, deps));
    }

    let max_modules = (DESTRUCTIBLE_MATKIND_NORMAL_MAX - DESTRUCTIBLE_MATKIND_NORMAL_MIN) as usize;
    let sorted_ids: Vec<u16> = raw_by_mod_id.keys().copied().take(max_modules).collect();

    let mut id_to_mat_kind = BTreeMap::new();
    let mut modules = BTreeMap::new();
    for (idx, &mod_id) in sorted_ids.iter().enumerate() {
        let normal_mat = DESTRUCTIBLE_MATKIND_NORMAL_MIN + idx as u8;
        let destroyed_mat = DESTRUCTIBLE_MATKIND_DAMAGED_MIN + idx as u8;
        id_to_mat_kind.insert(mod_id, normal_mat);
        let (surface, health, _) = &raw_by_mod_id[&mod_id];
        modules.insert(
            normal_mat,
            StructureModule {
                mat_kind: normal_mat,
                destroyed_mat_kind: destroyed_mat,
                surface: surface.clone(),
                module_id: mod_id,
                health: *health,
            },
        );
    }

    let mut direct_deps: BTreeMap<u8, Vec<u8>> = BTreeMap::new();
    for &mod_id in &sorted_ids {
        let root_mat = id_to_mat_kind[&mod_id];
        let deps = raw_by_mod_id[&mod_id]
            .2
            .iter()
            .filter_map(|dep_id| id_to_mat_kind.get(dep_id).copied())
            .collect();
        direct_deps.insert(root_mat, deps);
    }

    let mut destroy_depends = BTreeMap::new();
    for &root_mat in direct_deps.keys() {
        let mut visited = BTreeSet::new();
        let mut stack = vec![root_mat];
        while let Some(cur) = stack.pop() {
            if !visited.insert(cur) {
                continue;
            }
            if let Some(nexts) = direct_deps.get(&cur) {
                stack.extend(nexts.iter().copied());
            }
        }
        visited.remove(&root_mat);
        if !visited.is_empty() {
            destroy_depends.insert(root_mat, visited.into_iter().collect());
        }
    }

    let mut state_presets = BTreeMap::new();
    state_presets.insert("undamaged".to_string(), Vec::new());
    state_presets.insert(
        "destroyed".to_string(),
        modules.keys().copied().collect(),
    );
    if let Some(states_sec) = node.child("states") {
        for st_node in &states_sec.children {
            let name = st_node
                .child("name")
                .map(|n| n.text().to_string())
                .unwrap_or_default();
            if name.is_empty() {
                continue;
            }
            let mat_list = st_node
                .child("destroyedMaterials")
                .map(|n| n.text())
                .unwrap_or("");
            let mats = mat_list
                .split_whitespace()
                .filter_map(|m| {
                    let (_, mod_id, _) = parse_material_name(m)?;
                    id_to_mat_kind.get(&mod_id).copied()
                })
                .collect();
            state_presets.insert(name, mats);
        }
    }

    Ok(DestructibleDesc {
        id,
        filename,
        kind: DestructibleKind::Structure {
            modules,
            destroy_depends,
            state_presets,
        },
    })
}

fn parse_material_name(mat_name: &str) -> Option<(String, u16, Vec<u16>)> {
    let mut parts = mat_name.split('_');
    let _kind = parts.next()?;
    let surface = parts.next()?.to_string();
    let mod_id: u16 = parts.next()?.parse().ok()?;
    let mut deps = Vec::new();
    for p in parts {
        if let Ok(d) = p.parse::<u16>() {
            deps.push(d);
        }
    }
    Some((surface, mod_id, deps))
}

fn read_filename(node: &XmlNode) -> Result<String, AssetError> {
    node.child("filename")
        .map(|n| n.text().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AssetError::Destructibles("missing <filename> in destructible entry".into()))
}

fn parse_physic_params(
    node: &XmlNode,
    filename: &str,
) -> Result<DestructiblePhysicParams, AssetError> {
    let text = node
        .child("physicParams")
        .map(|n| n.text())
        .ok_or_else(|| AssetError::Destructibles(format!("{filename}: missing <physicParams>")))?;
    let mut vals = [0.0f32; 7];
    let mut count = 0usize;
    for tok in text.split_whitespace() {
        if count >= 7 {
            return Err(AssetError::Destructibles(format!(
                "{filename}: too many floats in <physicParams>"
            )));
        }
        vals[count] = tok.parse().map_err(|e| {
            AssetError::Destructibles(format!("{filename}: bad float in <physicParams>: {e}"))
        })?;
        count += 1;
    }
    if count != 7 {
        return Err(AssetError::Destructibles(format!(
            "{filename}: expected 7 floats in <physicParams>, got {count}"
        )));
    }
    Ok(DestructiblePhysicParams {
        mass: vals[0],
        height: vals[1],
        spring_angle: vals[2],
        spring_stiffness: vals[3],
        spring_resist: vals[4],
        air_resist: vals[5],
        bury_depth: vals[6],
    })
}

fn normalize_lod0(path: &str) -> String {
    if let Some(pos) = path.find("/lod") {
        let rest = &path[pos + 4..];
        if let Some(slash) = rest.find('/') {
            return format!("{}/lod0/{}", &path[..pos], &rest[slash + 1..]);
        }
    }
    path.to_string()
}

/// Encode `(grid_x, grid_z)` into BigWorld's 16-bit `AreaDestructibles` chunk ID.
pub fn chunk_id_from_indexes(grid_x: i32, grid_z: i32) -> u16 {
    (((grid_x + 127) as u16) << 8) | ((grid_z + 127) as u16 & 0xff)
}

/// Decode a 16-bit `AreaDestructibles` chunk ID into `(grid_x, grid_z)`.
pub fn chunk_indexes_from_id(chunk_id: u16) -> (i32, i32) {
    ((chunk_id >> 8) as i32 - 127, (chunk_id & 0xff) as i32 - 127)
}

/// Compute `AreaDestructibles` chunk ID for a world position in meters.
pub fn chunk_id_from_position(pos: Vec3) -> u16 {
    let gx = (pos.x * 0.01).floor() as i32;
    let gz = (pos.z * 0.01).floor() as i32;
    chunk_id_from_indexes(gx, gz)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_synthetic_destructibles_and_module_dependency_closure() {
        let xml = br#"<root>
            <defaultLifetimeEffectChance>1.0</defaultLifetimeEffectChance>
            <unitVehicleMass>28000</unitVehicleMass>
            <fragiles><entry><filename>content/f.model</filename><health>50</health></entry></fragiles>
            <fallingAtoms><entry><filename>content/p.model</filename><health>20</health><physicParams>400 10 0.03 300000 10000 0 0.1</physicParams></entry></fallingAtoms>
            <trees><entry><filename>speedtree/t.spt</filename><health>40000</health><density>0.4</density><physicParams>800 10 0.26 20000 4000 20 0.15</physicParams></entry></trees>
            <structures>
                <entry>
                    <filename>content/s.model</filename>
                    <modules>
                        <module><matName>n_wood_0_1</matName><health>80</health></module>
                        <module><matName>n_wood_1_2</matName><health>60</health></module>
                        <module><matName>n_wood_2</matName><health>40</health></module>
                    </modules>
                </entry>
            </structures>
        </root>"#;
        let cat = DestructiblesCatalog::parse(xml).unwrap();
        assert_eq!(cat.entries.len(), 4);
        assert!((cat.unit_vehicle_mass - 28000.0).abs() < 1e-4);
        let s = cat.by_filename("content/s.model").unwrap();
        match &s.kind {
            DestructibleKind::Structure {
                modules,
                destroy_depends,
                ..
            } => {
                assert_eq!(modules.len(), 3);
                assert_eq!(destroy_depends.get(&73).unwrap(), &vec![74, 75]);
            }
            other => panic!("expected structure, got {other:?}"),
        }
    }

    #[test]
    fn chunk_id_encoding_matches_bigworld() {
        let cid = chunk_id_from_indexes(0, -3);
        assert_eq!(chunk_indexes_from_id(cid), (0, -3));
        assert_eq!(chunk_id_from_position(Vec3::new(12.0, 0.0, -250.0)), cid);
    }
}
