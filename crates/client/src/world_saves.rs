//! Managed world names and paths. World contents belong to the server and stay opaque here.

use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

const METADATA_FILE: &str = "metadata.json";
const WORLD_FILE: &str = "world.json";
static NEXT_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldInfo {
    pub id: String,
    pub name: String,
}

pub struct WorldSaves {
    directory: PathBuf,
}

impl WorldSaves {
    /// `root` is the application data directory, shared with local preferences.
    pub fn new(root: PathBuf) -> Result<Self, String> {
        let directory = root.join("worlds");
        fs::create_dir_all(&directory)
            .map_err(|error| format!("Cannot create saved worlds folder: {error}"))?;
        Ok(Self { directory })
    }

    pub fn list(&self) -> Result<Vec<WorldInfo>, String> {
        let mut worlds = Vec::new();
        let entries = fs::read_dir(&self.directory)
            .map_err(|error| format!("Cannot read saved worlds: {error}"))?;
        for entry in entries {
            let entry = entry.map_err(|error| format!("Cannot read saved world: {error}"))?;
            if !entry
                .file_type()
                .map_err(|error| format!("Cannot inspect saved world: {error}"))?
                .is_dir()
            {
                continue;
            }
            let Some(id) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if !valid_id(&id) {
                continue;
            }
            // An interrupted allocation has no published metadata and is not a save.
            match fs::symlink_metadata(entry.path().join(METADATA_FILE)) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => log::warn!("Cannot inspect saved world {id}: {error}"),
                Ok(_) => match self.info(&id) {
                    Ok(info) => worlds.push(info),
                    Err(error) => log::warn!("Skipping saved world {id}: {error}"),
                },
            }
        }
        worlds.sort_by(|left, right| {
            left.name
                .to_lowercase()
                .cmp(&right.name.to_lowercase())
                .then_with(|| left.id.cmp(&right.id))
        });
        Ok(worlds)
    }

    /// A new world has no world file until the server generates and saves it.
    pub fn create(&self, name: &str) -> Result<WorldInfo, String> {
        self.create_with(name, |_| Ok(()))
    }

    /// The source must not be hosted, including in another app instance.
    pub fn duplicate(&self, id: &str, name: &str) -> Result<WorldInfo, String> {
        let source = self.world_path(id)?;
        let _source_lock = lock_world(&source)?;
        self.create_with(name, |directory| {
            match copy_world(&source, &directory.join(WORLD_FILE)) {
                // An unplayed new world is still a valid world to duplicate.
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                result => result,
            }
        })
    }

    /// Copy an existing world without changing, parsing, or moving the original.
    pub fn import(&self, source: &Path, name: &str) -> Result<WorldInfo, String> {
        self.create_with(name, |directory| {
            copy_world(source, &directory.join(WORLD_FILE))
        })
    }

    pub fn world_path(&self, id: &str) -> Result<PathBuf, String> {
        let directory = self.managed_directory(id)?;
        self.info(id)?;
        let path = directory.join(WORLD_FILE);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if !metadata.is_file() => {
                return Err("Saved world must be an ordinary file, not a link or folder".into());
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("Cannot inspect saved world: {error}")),
            Ok(_) => {}
        }
        Ok(path)
    }

    fn managed_directory(&self, id: &str) -> Result<PathBuf, String> {
        if !valid_id(id) {
            return Err("Invalid saved world ID".into());
        }
        let directory = self.directory.join(id);
        let metadata = fs::symlink_metadata(&directory)
            .map_err(|error| format!("Cannot find saved world: {error}"))?;
        if !metadata.is_dir() {
            return Err("Saved world must be a managed folder, not a link".into());
        }
        Ok(directory)
    }

    fn info(&self, id: &str) -> Result<WorldInfo, String> {
        let path = self.managed_directory(id)?.join(METADATA_FILE);
        if !fs::symlink_metadata(&path)
            .map_err(|error| format!("Cannot inspect saved world name: {error}"))?
            .is_file()
        {
            return Err("Saved world metadata must be an ordinary file".into());
        }
        let bytes =
            fs::read(&path).map_err(|error| format!("Cannot read saved world name: {error}"))?;
        let info: WorldInfo = serde_json::from_slice(&bytes)
            .map_err(|error| format!("Cannot read saved world metadata for {id}: {error}"))?;
        if info.id != id || clean_name(&info.name)? != info.name {
            return Err(format!("Invalid saved world metadata for {id}"));
        }
        Ok(info)
    }

    fn create_with(
        &self,
        name: &str,
        populate: impl FnOnce(&Path) -> io::Result<()>,
    ) -> Result<WorldInfo, String> {
        let name = clean_name(name)?;
        let (id, directory) = self.allocate()?;
        let info = WorldInfo { id, name };
        let result = (|| {
            populate(&directory).map_err(|error| format!("Cannot copy world: {error}"))?;
            let bytes = serde_json::to_vec_pretty(&info).map_err(|error| error.to_string())?;
            let temporary = directory.join("metadata.tmp");
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)
                .map_err(|error| format!("Cannot create saved world name: {error}"))?;
            file.write_all(&bytes)
                .and_then(|_| file.sync_all())
                .map_err(|error| format!("Cannot save world name: {error}"))?;
            fs::rename(temporary, directory.join(METADATA_FILE))
                .map_err(|error| format!("Cannot publish saved world: {error}"))?;
            Ok(info)
        })();
        if result.is_err() {
            // This is exclusively the new directory allocated by this operation.
            // No existing save or imported original is ever removed.
            let _ = fs::remove_dir_all(&directory);
        }
        result
    }

    fn allocate(&self) -> Result<(String, PathBuf), String> {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        loop {
            let sequence = NEXT_ID.fetch_add(1, Ordering::Relaxed);
            let id = format!("world-{timestamp:x}-{:x}-{sequence:x}", std::process::id());
            let directory = self.directory.join(&id);
            match fs::create_dir(&directory) {
                Ok(()) => return Ok((id, directory)),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(format!("Cannot create saved world: {error}")),
            }
        }
    }
}

/// Hosting and duplication share this lock. Keep the file after unlocking so
/// concurrent instances always lock the same inode; dropping it releases the lock.
pub(crate) fn lock_world(world_path: &Path) -> Result<File, String> {
    if let Some(parent) = world_path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let mut lock_path = world_path.as_os_str().to_os_string();
    lock_path.push(".host.lock");
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(PathBuf::from(lock_path))
        .map_err(|error| format!("Could not open this world's lock: {error}"))?;
    lock.try_lock().map_err(|error| {
        format!("This world may already be open in another Sauerteig window. Close its host and try again: {error}")
    })?;
    Ok(lock)
}

fn valid_id(id: &str) -> bool {
    id.starts_with("world-")
        && id.len() > "world-".len()
        && id.len() <= 96
        && id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn clean_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Give your world a name".into());
    }
    if name.chars().count() > 80 || name.chars().any(char::is_control) {
        return Err("Use a world name of 1–80 characters without control characters".into());
    }
    Ok(name.to_owned())
}

fn copy_world(source: &Path, destination: &Path) -> io::Result<()> {
    let mut input = File::open(source)?;
    if !input.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Choose a world file, not a folder",
        ));
    }
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    io::copy(&mut input, &mut output)?;
    output.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestDirectory(PathBuf);
    impl TestDirectory {
        fn new() -> Self {
            let sequence = NEXT_ID.fetch_add(1, Ordering::Relaxed);
            let directory = std::env::temp_dir().join(format!(
                "sauerteig-world-saves-{}-{}-{sequence}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir(&directory).unwrap();
            Self(directory)
        }
        fn saves(&self) -> WorldSaves {
            WorldSaves::new(self.0.clone()).unwrap()
        }
    }
    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn imports_and_duplicates_preserve_opaque_bytes_and_originals() {
        let directory = TestDirectory::new();
        let original = directory.0.join("foundry.json");
        // Deliberately not JSON: the launcher must not know the world schema.
        let bytes = b"future geometry\0\xff\n{\"version\":9000}";
        fs::write(&original, bytes).unwrap();
        let saves = directory.saves();
        let imported = saves.import(&original, "Foundry").unwrap();
        let duplicate = saves.duplicate(&imported.id, "Foundry copy").unwrap();
        let duplicate_path = saves.world_path(&duplicate.id).unwrap();
        assert_eq!(fs::read(&duplicate_path).unwrap(), bytes);
        fs::write(duplicate_path, b"changed copy").unwrap();
        assert_eq!(fs::read(&original).unwrap(), bytes);
        assert_eq!(
            fs::read(saves.world_path(&imported.id).unwrap()).unwrap(),
            bytes
        );
        assert_eq!(directory.saves().list().unwrap().len(), 2);
    }

    #[test]
    fn repeated_names_get_distinct_ids_and_unplayed_worlds_can_be_copied() {
        let directory = TestDirectory::new();
        let saves = directory.saves();
        let first = saves.create("  A world  ").unwrap();
        let second = saves.create("A world").unwrap();
        let third = saves.duplicate(&first.id, "A world").unwrap();
        assert_eq!(first.name, "A world");
        assert_ne!(first.id, second.id);
        assert_ne!(second.id, third.id);
        assert!(!saves.world_path(&first.id).unwrap().exists());
        assert_eq!(saves.list().unwrap().len(), 3);
    }

    #[test]
    fn invalid_names_and_paths_never_allocate_worlds() {
        let directory = TestDirectory::new();
        let saves = directory.saves();
        for name in ["", "  ", "bad\nname", &"a".repeat(81)] {
            assert!(saves.create(name).is_err());
        }
        for id in [
            "",
            "..",
            "../world-a",
            "world-a/../b",
            "world-a\\b",
            "/world-a",
        ] {
            assert!(saves.world_path(id).is_err());
            assert!(saves.duplicate(id, "Copy").is_err());
        }
        assert!(saves.list().unwrap().is_empty());
        assert_eq!(fs::read_dir(&saves.directory).unwrap().count(), 0);
    }

    #[test]
    fn failed_imports_remove_only_the_new_allocation() {
        let directory = TestDirectory::new();
        let saves = directory.saves();
        let existing = saves.create("Existing").unwrap();
        let existing_path = saves.world_path(&existing.id).unwrap();
        fs::write(&existing_path, "keep me").unwrap();
        assert!(saves
            .import(&directory.0.join("missing.json"), "Missing")
            .is_err());
        assert!(saves.import(&directory.0, "Folder").is_err());
        assert_eq!(saves.list().unwrap(), vec![existing]);
        assert_eq!(fs::read(existing_path).unwrap(), b"keep me");
        assert_eq!(fs::read_dir(&saves.directory).unwrap().count(), 1);
    }

    #[test]
    fn duplicate_refuses_an_active_host_and_succeeds_after_unlocking() {
        let directory = TestDirectory::new();
        let saves = directory.saves();
        let source = saves.create("Source").unwrap();
        let source_path = saves.world_path(&source.id).unwrap();
        fs::write(&source_path, b"before final save").unwrap();
        let host_lock = lock_world(&source_path).unwrap();
        let error = saves.duplicate(&source.id, "Copy").unwrap_err();
        assert!(error.contains("another Sauerteig window"));
        assert_eq!(saves.list().unwrap(), vec![source.clone()]);
        fs::write(&source_path, b"final save").unwrap();
        drop(host_lock);
        let copy = saves.duplicate(&source.id, "Copy").unwrap();
        assert_eq!(
            fs::read(saves.world_path(&copy.id).unwrap()).unwrap(),
            b"final save"
        );
        // Duplication also releases its lock on successful completion.
        assert!(lock_world(&source_path).is_ok());
    }

    #[test]
    fn broken_metadata_does_not_hide_healthy_saved_worlds() {
        let directory = TestDirectory::new();
        let saves = directory.saves();
        let healthy = saves.create("Healthy").unwrap();
        let broken = saves.create("Broken").unwrap();
        let metadata_path = saves.directory.join(&broken.id).join(METADATA_FILE);
        fs::write(&metadata_path, b"invalid metadata").unwrap();
        assert_eq!(saves.list().unwrap(), vec![healthy]);
        assert!(saves.world_path(&broken.id).is_err());
        assert_eq!(fs::read(metadata_path).unwrap(), b"invalid metadata");
    }

    #[cfg(unix)]
    #[test]
    fn hosting_paths_cannot_follow_links_out_of_managed_saves() {
        use std::os::unix::fs::symlink;
        let directory = TestDirectory::new();
        let saves = directory.saves();
        let world = saves.create("Linked").unwrap();
        let world_path = saves.world_path(&world.id).unwrap();
        let original = directory.0.join("original.json");
        fs::write(&original, "original").unwrap();
        symlink(&original, world_path).unwrap();
        assert!(saves.world_path(&world.id).is_err());
        assert!(saves.duplicate(&world.id, "Copy").is_err());
        symlink(&directory.0, saves.directory.join("world-external")).unwrap();
        assert!(saves.world_path("world-external").is_err());
        assert_eq!(fs::read(original).unwrap(), b"original");
    }
}
