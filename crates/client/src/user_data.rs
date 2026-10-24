//! Local preferences and blueprint documents. Writes are serialized and atomic.
use serde::{Deserialize, Serialize};
use std::{collections::VecDeque, sync::mpsc};

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    pub invert_scroll: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Document {
    Settings,
    Blueprints,
}
impl Document {
    fn key(self) -> &'static str {
        match self {
            Self::Settings => "settings",
            Self::Blueprints => "blueprints",
        }
    }
}
pub enum StorageEvent {
    Loaded(Document, Option<String>),
    Saved(Document),
    Failed(Document, String),
}
struct Job {
    document: Document,
    value: Option<String>,
}
pub struct Storage {
    queue: VecDeque<Job>,
    sender: mpsc::Sender<StorageEvent>,
    receiver: mpsc::Receiver<StorageEvent>,
    busy: bool,
}
impl Storage {
    pub fn new() -> Self {
        let (sender, receiver) = mpsc::channel();
        let mut storage = Self {
            queue: VecDeque::new(),
            sender,
            receiver,
            busy: false,
        };
        for document in [Document::Settings, Document::Blueprints] {
            storage.queue.push_back(Job {
                document,
                value: None,
            });
        }
        storage
    }
    pub fn load(&mut self, document: Document) {
        self.queue.push_back(Job {
            document,
            value: None,
        });
    }
    pub fn save(&mut self, document: Document, value: String) {
        // Coalesce pending writes, but never reorder a write ahead of the initial read.
        if let Some(job) = self
            .queue
            .iter_mut()
            .rev()
            .find(|job| job.document == document && job.value.is_some())
        {
            job.value = Some(value);
        } else {
            self.queue.push_back(Job {
                document,
                value: Some(value),
            });
        }
    }
    pub fn poll(&mut self) -> Vec<StorageEvent> {
        let mut events = Vec::new();
        while let Ok(event) = self.receiver.try_recv() {
            self.busy = false;
            let superseded = match &event {
                StorageEvent::Saved(document) => self
                    .queue
                    .iter()
                    .any(|job| job.document == *document && job.value.is_some()),
                _ => false,
            };
            if !superseded {
                events.push(event);
            }
        }
        if !self.busy {
            if let Some(job) = self.queue.pop_front() {
                self.busy = true;
                launch(job, self.sender.clone());
            }
        }
        events
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn data_directory() -> Result<std::path::PathBuf, String> {
    use std::{env, path::PathBuf};
    if let Some(path) = env::var_os("SAUERTEIG_DATA_DIR") {
        return Ok(PathBuf::from(path));
    }
    #[cfg(target_os = "macos")]
    let path =
        env::var_os("HOME").map(|p| PathBuf::from(p).join("Library/Application Support/Sauerteig"));
    #[cfg(target_os = "windows")]
    let path = env::var_os("LOCALAPPDATA").map(|p| PathBuf::from(p).join("Sauerteig"));
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let path = env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|p| PathBuf::from(p).join(".local/share")))
        .map(|p| p.join("sauerteig"));
    path.ok_or_else(|| "Cannot locate your application data folder".into())
}

#[cfg(not(target_arch = "wasm32"))]
fn launch(job: Job, sender: mpsc::Sender<StorageEvent>) {
    std::thread::spawn(move || {
        let result = data_directory().and_then(|directory| execute(&directory, &job));
        let event = match result {
            Ok(value) if job.value.is_none() => StorageEvent::Loaded(job.document, value),
            Ok(_) => StorageEvent::Saved(job.document),
            Err(error) => StorageEvent::Failed(job.document, error),
        };
        let _ = sender.send(event);
    });
}

#[cfg(not(target_arch = "wasm32"))]
fn execute(directory: &std::path::Path, job: &Job) -> Result<Option<String>, String> {
    use std::{fs, io::Write};
    let path = directory.join(format!("{}.json", job.document.key()));
    if let Some(value) = &job.value {
        fs::create_dir_all(directory).map_err(|e| e.to_string())?;
        let temporary =
            directory.join(format!("{}.{}.tmp", job.document.key(), std::process::id()));
        let mut file = fs::File::create(&temporary).map_err(|e| e.to_string())?;
        file.write_all(value.as_bytes())
            .and_then(|_| file.sync_all())
            .map_err(|e| e.to_string())?;
        fs::rename(&temporary, &path).map_err(|e| e.to_string())?;
        Ok(None)
    } else {
        match fs::read_to_string(path) {
            Ok(value) => Ok(Some(value)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen(inline_js = r#"
function openSauerteig() {
  return new Promise((resolve, reject) => {
    const request = indexedDB.open('sauerteig', 1);
    request.onupgradeneeded = () => request.result.createObjectStore('documents');
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error);
    request.onblocked = () => reject(new Error('Close other Sauerteig tabs to update local storage'));
  });
}
export async function readSauerteig(key) {
  const db = await openSauerteig();
  try { return await new Promise((resolve, reject) => {
    const tx = db.transaction('documents', 'readonly');
    const request = tx.objectStore('documents').get(key);
    tx.oncomplete = () => resolve(request.result ?? null);
    tx.onabort = () => reject(tx.error || new Error('Local read failed'));
    tx.onerror = () => reject(tx.error);
  }); } finally { db.close(); }
}
export async function writeSauerteig(key, value) {
  const db = await openSauerteig();
  try { await new Promise((resolve, reject) => {
    const tx = db.transaction('documents', 'readwrite');
    tx.objectStore('documents').put(value, key);
    tx.oncomplete = resolve;
    tx.onabort = () => reject(tx.error || new Error('Local save failed'));
    tx.onerror = () => reject(tx.error);
  }); } finally { db.close(); }
}
"#)]
extern "C" {
    #[wasm_bindgen::prelude::wasm_bindgen(catch, js_name = readSauerteig)]
    async fn read_document(key: &str) -> Result<wasm_bindgen::JsValue, wasm_bindgen::JsValue>;
    #[wasm_bindgen::prelude::wasm_bindgen(catch, js_name = writeSauerteig)]
    async fn write_document(
        key: &str,
        value: &str,
    ) -> Result<wasm_bindgen::JsValue, wasm_bindgen::JsValue>;
}
#[cfg(target_arch = "wasm32")]
fn launch(job: Job, sender: mpsc::Sender<StorageEvent>) {
    wasm_bindgen_futures::spawn_local(async move {
        let result = if let Some(value) = &job.value {
            write_document(job.document.key(), value)
                .await
                .map(|_| None)
        } else {
            read_document(job.document.key())
                .await
                .map(|v| v.as_string())
        };
        let event = match result {
            Ok(value) if job.value.is_none() => StorageEvent::Loaded(job.document, value),
            Ok(_) => StorageEvent::Saved(job.document),
            Err(error) => StorageEvent::Failed(job.document, format!("{error:?}")),
        };
        let _ = sender.send(event);
    });
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    #[test]
    fn saves_survive_reopening_and_failed_reads_do_not_create_documents() {
        let directory =
            std::env::temp_dir().join(format!("sauerteig-storage-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        let read = Job {
            document: Document::Blueprints,
            value: None,
        };
        assert_eq!(execute(&directory, &read).unwrap(), None);
        let write = Job {
            document: Document::Blueprints,
            value: Some("first".into()),
        };
        execute(&directory, &write).unwrap();
        execute(
            &directory,
            &Job {
                value: Some("second".into()),
                ..write
            },
        )
        .unwrap();
        assert_eq!(
            execute(&directory, &read).unwrap().as_deref(),
            Some("second")
        );
        std::fs::remove_dir_all(directory).unwrap();
    }
}
