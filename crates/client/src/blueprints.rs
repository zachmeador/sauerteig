//! Versioned local blueprint metadata. Storage and UI belong to the host.
use crate::editor::Clipboard;
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::BTreeSet;
use std::fmt;

pub const BLUEPRINT_VERSION: u32 = 3;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Blueprint {
    pub id: u64,
    pub name: String,
    pub folder: String,
    pub clipboard: Clipboard,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct BlueprintLibrary {
    version: u32,
    next_id: u64,
    entries: Vec<Blueprint>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BlueprintError {
    EmptyName,
    Missing,
    UnsupportedVersion(u32),
    InvalidData(String),
    IdExhausted,
}
impl fmt::Display for BlueprintError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyName => f.write_str("Give this blueprint a name."),
            Self::Missing => f.write_str("That blueprint no longer exists."),
            Self::UnsupportedVersion(version) => {
                write!(f, "Blueprint format version {version} is not supported.")
            }
            Self::InvalidData(message) => write!(f, "Invalid blueprint library: {message}"),
            Self::IdExhausted => {
                f.write_str("The blueprint library has exhausted its identifiers.")
            }
        }
    }
}
impl std::error::Error for BlueprintError {}

fn name(value: &str) -> Result<String, BlueprintError> {
    let value = value.trim();
    if value.is_empty() {
        Err(BlueprintError::EmptyName)
    } else {
        Ok(value.to_owned())
    }
}
fn folder(value: &str) -> String {
    value
        .replace('\\', "/")
        .split('/')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("/")
}

impl Default for BlueprintLibrary {
    fn default() -> Self {
        Self::new()
    }
}
impl BlueprintLibrary {
    pub fn new() -> Self {
        Self {
            version: BLUEPRINT_VERSION,
            next_id: 1,
            entries: Vec::new(),
        }
    }
    pub fn entries(&self) -> &[Blueprint] {
        &self.entries
    }
    pub fn get(&self, id: u64) -> Option<&Blueprint> {
        self.entries.iter().find(|entry| entry.id == id)
    }
    pub fn save(
        &mut self,
        label: &str,
        location: &str,
        clipboard: Clipboard,
    ) -> Result<u64, BlueprintError> {
        let name = name(label)?;
        let next = self
            .next_id
            .checked_add(1)
            .ok_or(BlueprintError::IdExhausted)?;
        let id = self.next_id;
        self.entries.push(Blueprint {
            id,
            name,
            folder: folder(location),
            clipboard,
        });
        self.next_id = next;
        Ok(id)
    }
    pub fn rename(&mut self, id: u64, label: &str) -> Result<(), BlueprintError> {
        let name = name(label)?;
        self.entries
            .iter_mut()
            .find(|entry| entry.id == id)
            .ok_or(BlueprintError::Missing)?
            .name = name;
        Ok(())
    }
    pub fn move_to(&mut self, id: u64, location: &str) -> Result<(), BlueprintError> {
        self.entries
            .iter_mut()
            .find(|entry| entry.id == id)
            .ok_or(BlueprintError::Missing)?
            .folder = folder(location);
        Ok(())
    }
    pub fn duplicate(&mut self, id: u64, label: Option<&str>) -> Result<u64, BlueprintError> {
        let source = self.get(id).ok_or(BlueprintError::Missing)?.clone();
        self.save(
            label.unwrap_or(&format!("{} copy", source.name)),
            &source.folder,
            source.clipboard,
        )
    }
    pub fn delete(&mut self, id: u64) -> Result<Blueprint, BlueprintError> {
        let index = self
            .entries
            .iter()
            .position(|entry| entry.id == id)
            .ok_or(BlueprintError::Missing)?;
        Ok(self.entries.remove(index))
    }
    pub fn load(&self, id: u64) -> Option<Clipboard> {
        self.get(id).map(|entry| entry.clipboard.clone())
    }
    /// Search name and folder, requiring every whitespace-separated term.
    /// A folder filter includes that folder and its descendants.
    pub fn search(&self, query: &str, location: Option<&str>) -> Vec<&Blueprint> {
        let terms: Vec<_> = query
            .to_lowercase()
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        let location = location.map(folder).map(|s| s.to_lowercase());
        let mut results: Vec<_> = self
            .entries
            .iter()
            .filter(|entry| {
                let entry_folder = entry.folder.to_lowercase();
                let in_folder = location.as_ref().is_none_or(|folder| {
                    folder.is_empty()
                        || entry_folder == *folder
                        || entry_folder.starts_with(&format!("{folder}/"))
                });
                let text = format!("{} {}", entry.name, entry.folder).to_lowercase();
                in_folder && terms.iter().all(|term| text.contains(term))
            })
            .collect();
        results.sort_by(|a, b| {
            a.folder
                .cmp(&b.folder)
                .then_with(|| a.name.cmp(&b.name))
                .then(a.id.cmp(&b.id))
        });
        results
    }
    pub fn to_json(&self) -> Result<String, BlueprintError> {
        serde_json::to_string(self).map_err(|error| BlueprintError::InvalidData(error.to_string()))
    }
    pub fn from_json(json: &str) -> Result<Self, BlueprintError> {
        serde_json::from_str(json).map_err(|error| BlueprintError::InvalidData(error.to_string()))
    }
}
impl<'de> Deserialize<'de> for BlueprintLibrary {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Raw {
            version: u32,
            next_id: u64,
            entries: Vec<Blueprint>,
        }
        let mut raw = Raw::deserialize(deserializer)?;
        if !(1..=BLUEPRINT_VERSION).contains(&raw.version) {
            return Err(serde::de::Error::custom(
                BlueprintError::UnsupportedVersion(raw.version),
            ));
        }
        let mut ids = BTreeSet::new();
        for entry in &mut raw.entries {
            if entry.id == 0 || entry.id >= raw.next_id || !ids.insert(entry.id) {
                return Err(serde::de::Error::custom(
                    "duplicate or invalid blueprint identifier",
                ));
            }
            entry.name = name(&entry.name).map_err(serde::de::Error::custom)?;
            entry.folder = folder(&entry.folder);
        }
        if raw.next_id == 0 {
            return Err(serde::de::Error::custom(
                "invalid next blueprint identifier",
            ));
        }
        Ok(Self {
            version: BLUEPRINT_VERSION,
            next_id: raw.next_id,
            entries: raw.entries,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        editor::EditSelection,
        regions::{RegionBox, RegionSet},
    };
    use sauerteig_core::{
        Cell, FineBox, MaterialVolume, ObjectKind, Vec3, VolumeMaterial, World, WorldObject,
    };
    fn captured() -> Clipboard {
        let mut world = World::empty();
        world.set([0, 0, 0], Some(Cell::solid(2)));
        let selection =
            RegionSet::from_boxes([RegionBox::new([0, 0, 0], [8, 8, 8]).unwrap()]).unwrap();
        Clipboard::copy_regions(&world, &selection).unwrap()
    }
    #[test]
    fn library_crud_search_and_independent_copies() {
        let mut library = BlueprintLibrary::new();
        let first = library
            .save("  Courtyard  ", "Architecture / Rooms", captured())
            .unwrap();
        let second = library.duplicate(first, None).unwrap();
        library.rename(second, "Courtyard night").unwrap();
        library.move_to(second, " Architecture/Exterior ").unwrap();
        assert_eq!(
            library
                .search("COURT exterior", Some("Architecture"))
                .iter()
                .map(|entry| entry.id)
                .collect::<Vec<_>>(),
            vec![second]
        );
        assert_eq!(library.get(first).unwrap().name, "Courtyard");
        let mut copy = library.load(first).unwrap();
        copy.rotate_y().unwrap();
        assert_ne!(copy, library.get(first).unwrap().clipboard);
        library.delete(first).unwrap();
        assert!(library.get(first).is_none());
        assert!(library.load(second).is_some());
        assert!(library.rename(second, "  ").is_err());
    }
    #[test]
    fn versioned_library_roundtrip_preserves_sparse_capture() {
        let mut library = BlueprintLibrary::new();
        library.save("Room", "Test", captured()).unwrap();
        let json = library.to_json().unwrap();
        assert_eq!(BlueprintLibrary::from_json(&json).unwrap(), library);
        let future = json.replacen("\"version\":3", "\"version\":4", 1);
        assert!(BlueprintLibrary::from_json(&future).is_err());
        let mut value: serde_json::Value = serde_json::from_str(&json).unwrap();
        let entry = value["entries"][0].clone();
        value["entries"].as_array_mut().unwrap().push(entry);
        assert!(BlueprintLibrary::from_json(&value.to_string()).is_err());
        let selection = EditSelection {
            min: [16, 0, 0],
            max: [24, 8, 8],
            axis: 1,
            sign: 1,
            grid: 8,
        };
        assert!(library
            .load(1)
            .unwrap()
            .plan_paste(&World::empty(), &selection)
            .is_ok());
    }

    #[test]
    fn library_migrates_old_versions_and_keeps_material_volumes() {
        let mut world = World::empty();
        world
            .replace_volumes(&[MaterialVolume {
                bounds: FineBox {
                    min: [0; 3],
                    max: [8; 3],
                },
                material: VolumeMaterial::Water,
            }])
            .unwrap();
        let selection = RegionSet::from_boxes([RegionBox::new([0; 3], [16; 3]).unwrap()]).unwrap();
        let mut library = BlueprintLibrary::new();
        library
            .save(
                "Pool",
                "Water",
                Clipboard::copy_regions(&world, &selection).unwrap(),
            )
            .unwrap();
        let json = library.to_json().unwrap();
        let restored = BlueprintLibrary::from_json(&json).unwrap();
        assert_eq!(restored, library);
        assert_eq!(restored.load(1).unwrap().volumes(), world.volumes());
        for version in [1, 2] {
            let mut old: serde_json::Value = serde_json::from_str(&json).unwrap();
            old["version"] = serde_json::json!(version);
            old["entries"][0]["clipboard"]
                .as_object_mut()
                .unwrap()
                .remove("volumes");
            let migrated = BlueprintLibrary::from_json(&old.to_string()).unwrap();
            assert_eq!(migrated.version, BLUEPRINT_VERSION);
            assert!(migrated.load(1).unwrap().volumes().is_empty());
        }
    }

    #[test]
    fn legacy_geometry_library_migrates_and_objects_survive_blueprint_storage() {
        let mut legacy = BlueprintLibrary::new();
        legacy.save("Old room", "", captured()).unwrap();
        let mut json = serde_json::to_value(&legacy).unwrap();
        json["version"] = serde_json::json!(1);
        json["entries"][0]["clipboard"]
            .as_object_mut()
            .unwrap()
            .remove("objects");
        let migrated = BlueprintLibrary::from_json(&json.to_string()).unwrap();
        assert_eq!(migrated.version, BLUEPRINT_VERSION);
        assert!(migrated.load(1).unwrap().objects().is_empty());

        let selection = RegionSet::from_boxes([RegionBox::new([0; 3], [16; 3]).unwrap()]).unwrap();
        let objects = vec![
            WorldObject {
                id: 9,
                kind: ObjectKind::Light,
                position: Vec3::new(0.5, 1.5, 0.5),
                rotation: 0.25,
            },
            WorldObject {
                id: 42,
                kind: ObjectKind::Spawn,
                position: Vec3::new(1.5, 0.0, 1.5),
                rotation: 1.0,
            },
        ];
        let mut clipboard =
            Clipboard::copy_regions_with_objects(&World::empty(), &selection, &objects).unwrap();
        clipboard.rotate_y().unwrap();
        clipboard.mirror(2).unwrap();
        let mut library = migrated;
        let id = library
            .save("Lit room", "Rooms", clipboard.clone())
            .unwrap();
        let restored = BlueprintLibrary::from_json(&library.to_json().unwrap()).unwrap();
        assert_eq!(restored, library);
        assert_eq!(restored.load(id).unwrap(), clipboard);
        assert_eq!(restored.load(id).unwrap().objects().len(), 2);
    }
}
