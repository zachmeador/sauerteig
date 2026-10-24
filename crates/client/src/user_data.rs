//! Local preferences and blueprint documents. Writes are serialized and atomic.
use serde::{Deserialize, Serialize};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    mpsc, Mutex,
};

static PENDING_WRITES: AtomicUsize = AtomicUsize::new(0);
static WRITE_ERRORS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Includes writes whose originating game or storage handle has been dropped.
pub fn pending_writes() -> bool {
    PENDING_WRITES.load(Ordering::Acquire) != 0
}

/// Report write failures even when their original storage handle no longer
/// exists. Taking the message lets the user retry or leave after seeing it.
pub fn take_write_error() -> Option<String> {
    let mut errors = WRITE_ERRORS
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if errors.is_empty() {
        None
    } else {
        Some(std::mem::take(&mut *errors).join("\n"))
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    pub invert_scroll: bool,
}

/// Launcher choices live separately from in-game settings so either can save
/// without replacing the other's changes.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct SessionPreferences {
    pub player_name: String,
    pub server: String,
    pub last_world: Option<String>,
}
impl Default for SessionPreferences {
    fn default() -> Self {
        Self {
            player_name: "Player".into(),
            server: String::new(),
            last_world: None,
        }
    }
}
impl SessionPreferences {
    pub fn load() -> Result<Self, String> {
        #[cfg(not(target_arch = "wasm32"))]
        let value = execute_named(&data_directory()?, "session", None)?;
        #[cfg(target_arch = "wasm32")]
        let value = browser_preferences()?
            .get_item("sauerteig-session")
            .map_err(|e| format!("Cannot read saved session: {e:?}"))?;
        value.map_or_else(
            || Ok(Self::default()),
            |value| {
                serde_json::from_str(&value).map_err(|e| format!("Cannot read saved session: {e}"))
            },
        )
    }

    pub fn save(&self) -> Result<(), String> {
        let value = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        #[cfg(not(target_arch = "wasm32"))]
        {
            execute_named(&data_directory()?, "session", Some(&value)).map(|_| ())
        }
        #[cfg(target_arch = "wasm32")]
        {
            browser_preferences()?
                .set_item("sauerteig-session", &value)
                .map_err(|e| format!("Cannot save session choices: {e:?}"))
        }
    }
}

#[cfg(target_arch = "wasm32")]
fn browser_preferences() -> Result<web_sys::Storage, String> {
    web_sys::window()
        .ok_or("Browser window unavailable")?
        .local_storage()
        .map_err(|e| format!("Local storage unavailable: {e:?}"))?
        .ok_or_else(|| "Local storage unavailable; session choices will not be remembered".into())
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
struct Completion {
    event: StorageEvent,
    writing: bool,
}
pub struct Storage {
    sender: mpsc::Sender<Completion>,
    receiver: mpsc::Receiver<Completion>,
    pending: [usize; 2],
    #[cfg(not(target_arch = "wasm32"))]
    directory: Result<std::path::PathBuf, String>,
}
impl Storage {
    pub fn new() -> Self {
        let mut storage = Self::empty();
        storage.initial_loads();
        storage
    }
    fn empty() -> Self {
        let (sender, receiver) = mpsc::channel();
        Self {
            sender,
            receiver,
            pending: [0; 2],
            #[cfg(not(target_arch = "wasm32"))]
            directory: data_directory(),
        }
    }
    fn initial_loads(&mut self) {
        for document in [Document::Settings, Document::Blueprints] {
            self.load(document);
        }
    }
    pub fn load(&mut self, document: Document) {
        self.submit(Job {
            document,
            value: None,
        });
    }
    pub fn save(&mut self, document: Document, value: String) {
        self.pending[document as usize] += 1;
        self.submit(Job {
            document,
            value: Some(value),
        });
    }
    fn submit(&self, job: Job) {
        // Submission is immediate. The process-wide queue owns work, so a game
        // replacement cannot discard saves or load ahead of an older write.
        if job.value.is_some() {
            PENDING_WRITES.fetch_add(1, Ordering::AcqRel);
        }
        #[cfg(not(target_arch = "wasm32"))]
        native::enqueue(job, self.sender.clone(), self.directory.clone());
        #[cfg(target_arch = "wasm32")]
        browser::enqueue(job, self.sender.clone());
    }
    pub fn poll(&mut self) -> Vec<StorageEvent> {
        let mut events = Vec::new();
        while let Ok(completion) = self.receiver.try_recv() {
            let document = match &completion.event {
                StorageEvent::Loaded(document, _)
                | StorageEvent::Saved(document)
                | StorageEvent::Failed(document, _) => *document,
            };
            if completion.writing {
                self.pending[document as usize] -= 1;
            }
            if matches!(completion.event, StorageEvent::Saved(_))
                && self.pending[document as usize] != 0
            {
                continue;
            }
            events.push(completion.event);
        }
        events
    }
}

fn complete(job: Job, sender: mpsc::Sender<Completion>, result: Result<Option<String>, String>) {
    let writing = job.value.is_some();
    if writing {
        if let Err(error) = &result {
            let message = format!("Could not save {}: {error}", job.document.key());
            let mut errors = WRITE_ERRORS
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if !errors.contains(&message) {
                errors.push(message);
            }
        }
    }
    let event = match result {
        Ok(value) if !writing => StorageEvent::Loaded(job.document, value),
        Ok(_) => StorageEvent::Saved(job.document),
        Err(error) => StorageEvent::Failed(job.document, error),
    };
    if let Err(error) = sender.send(Completion { event, writing }) {
        if let StorageEvent::Failed(document, message) = error.0.event {
            log::error!(
                "Local {} storage failed after its session ended: {message}",
                document.key()
            );
        }
    }
    if writing {
        PENDING_WRITES.fetch_sub(1, Ordering::AcqRel);
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn data_directory() -> Result<std::path::PathBuf, String> {
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

/// Wait for all earlier local jobs and take any unreported write failures.
/// Normal UI shutdown can first wait for `pending_writes()` to clear.
#[cfg(not(target_arch = "wasm32"))]
pub fn flush_pending() -> Result<(), String> {
    native::flush_pending()
}

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use super::*;
    use std::{path::PathBuf, sync::OnceLock};

    pub(super) enum Work {
        Document {
            job: Job,
            sender: mpsc::Sender<Completion>,
            directory: Result<PathBuf, String>,
        },
        Barrier(mpsc::Sender<Result<(), String>>),
        #[cfg(test)]
        Pause {
            entered: mpsc::Sender<()>,
            resume: mpsc::Receiver<()>,
        },
    }

    fn worker() -> Result<&'static mpsc::Sender<Work>, String> {
        static WORKER: OnceLock<Result<mpsc::Sender<Work>, String>> = OnceLock::new();
        WORKER
            .get_or_init(|| {
                let (sender, receiver) = mpsc::channel();
                std::thread::Builder::new()
                    .name("sauerteig-storage".into())
                    .spawn(move || {
                        while let Ok(work) = receiver.recv() {
                            match work {
                                Work::Document {
                                    job,
                                    sender,
                                    directory,
                                } => {
                                    let result =
                                        directory.and_then(|directory| execute(&directory, &job));
                                    complete(job, sender, result);
                                }
                                Work::Barrier(sender) => {
                                    let _ = sender.send(take_write_error().map_or(Ok(()), Err));
                                }
                                #[cfg(test)]
                                Work::Pause { entered, resume } => {
                                    let _ = entered.send(());
                                    let _ = resume.recv();
                                }
                            }
                        }
                    })
                    .map_err(|error| format!("Could not start local storage: {error}"))?;
                Ok(sender)
            })
            .as_ref()
            .map_err(Clone::clone)
    }

    pub(super) fn enqueue(
        job: Job,
        sender: mpsc::Sender<Completion>,
        directory: Result<PathBuf, String>,
    ) {
        let work = Work::Document {
            job,
            sender,
            directory,
        };
        let (work, error) = match worker() {
            Ok(worker) => match worker.send(work) {
                Ok(()) => return,
                Err(error) => (
                    error.0,
                    "Local storage worker stopped unexpectedly".to_string(),
                ),
            },
            Err(error) => (work, error),
        };
        if let Work::Document { job, sender, .. } = work {
            complete(job, sender, Err(error));
        }
    }

    pub(super) fn flush_pending() -> Result<(), String> {
        let (sender, receiver) = mpsc::channel();
        worker()?
            .send(Work::Barrier(sender))
            .map_err(|_| "Local storage worker stopped unexpectedly".to_string())?;
        receiver
            .recv()
            .map_err(|_| "Local storage did not finish saving".to_string())?
    }

    #[cfg(test)]
    pub(super) fn pause() -> mpsc::Sender<()> {
        let (entered, waiting) = mpsc::channel();
        let (resume, resumed) = mpsc::channel();
        worker()
            .unwrap()
            .send(Work::Pause {
                entered,
                resume: resumed,
            })
            .unwrap();
        waiting.recv().unwrap();
        resume
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn execute(directory: &std::path::Path, job: &Job) -> Result<Option<String>, String> {
    execute_named(directory, job.document.key(), job.value.as_deref())
}

#[cfg(not(target_arch = "wasm32"))]
fn execute_named(
    directory: &std::path::Path,
    key: &str,
    value: Option<&str>,
) -> Result<Option<String>, String> {
    use std::{fs, io::Write};
    let path = directory.join(format!("{key}.json"));
    if let Some(value) = value {
        fs::create_dir_all(directory).map_err(|e| e.to_string())?;
        let temporary = directory.join(format!("{key}.{}.tmp", std::process::id()));
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
mod browser {
    use super::*;
    use std::{cell::RefCell, collections::VecDeque};

    #[derive(Default)]
    struct Queue {
        running: bool,
        jobs: VecDeque<(Job, mpsc::Sender<Completion>)>,
    }

    thread_local! {
        static QUEUE: RefCell<Queue> = RefCell::new(Queue::default());
    }

    pub(super) fn enqueue(job: Job, sender: mpsc::Sender<Completion>) {
        let start = QUEUE.with_borrow_mut(|queue| {
            queue.jobs.push_back((job, sender));
            if queue.running {
                false
            } else {
                queue.running = true;
                true
            }
        });
        if !start {
            return;
        }
        wasm_bindgen_futures::spawn_local(async {
            loop {
                let next = QUEUE.with_borrow_mut(|queue| {
                    let next = queue.jobs.pop_front();
                    if next.is_none() {
                        queue.running = false;
                    }
                    next
                });
                let Some((job, sender)) = next else {
                    break;
                };
                let result = if let Some(value) = &job.value {
                    write_document(job.document.key(), value)
                        .await
                        .map(|_| None)
                } else {
                    read_document(job.document.key())
                        .await
                        .map(|value| value.as_string())
                };
                complete(job, sender, result.map_err(|error| format!("{error:?}")));
            }
        });
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) mod tests {
    use super::*;

    pub(crate) static QUEUE_TEST: Mutex<()> = Mutex::new(());

    pub(crate) fn storage_at(directory: &std::path::Path) -> Storage {
        let mut storage = Storage::empty();
        storage.directory = Ok(directory.to_owned());
        storage.initial_loads();
        storage
    }

    #[test]
    fn dropped_writer_finishes_before_the_next_session_loads() {
        let _serial = QUEUE_TEST.lock().unwrap();
        let _ = flush_pending();
        let directory =
            std::env::temp_dir().join(format!("sauerteig-storage-rejoin-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        let resume = native::pause();
        let mut first = storage_at(&directory);
        first.save(Document::Blueprints, "first blueprint".into());
        first.save(Document::Blueprints, "latest blueprint".into());
        drop(first);
        let mut next = storage_at(&directory);
        assert!(pending_writes());
        assert!(!directory.join("blueprints.json").exists());
        resume.send(()).unwrap();
        flush_pending().unwrap();
        let loaded = next.poll().into_iter().find_map(|event| match event {
            StorageEvent::Loaded(Document::Blueprints, value) => value,
            _ => None,
        });
        assert_eq!(loaded.as_deref(), Some("latest blueprint"));
        assert!(!pending_writes());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn exit_barrier_saves_without_a_live_handle_or_polling() {
        let _serial = QUEUE_TEST.lock().unwrap();
        let _ = flush_pending();
        let directory =
            std::env::temp_dir().join(format!("sauerteig-storage-exit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        let mut storage = storage_at(&directory);
        storage.save(Document::Settings, r#"{"invert_scroll":true}"#.into());
        storage.save(Document::Blueprints, "saved at exit".into());
        drop(storage);
        flush_pending().unwrap();
        assert_eq!(
            std::fs::read_to_string(directory.join("settings.json")).unwrap(),
            r#"{"invert_scroll":true}"#
        );
        assert_eq!(
            std::fs::read_to_string(directory.join("blueprints.json")).unwrap(),
            "saved at exit"
        );
        assert!(!pending_writes());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn failed_orphaned_write_is_reported_by_the_exit_barrier() {
        let _serial = QUEUE_TEST.lock().unwrap();
        let _ = flush_pending();
        let path =
            std::env::temp_dir().join(format!("sauerteig-storage-failure-{}", std::process::id()));
        std::fs::write(&path, b"a file cannot hold local documents").unwrap();
        let mut storage = storage_at(&path);
        storage.save(Document::Settings, "cannot be written".into());
        drop(storage);
        let error = flush_pending().unwrap_err();
        assert!(error.contains("Could not save settings"));
        assert!(take_write_error().is_none(), "a surfaced error is consumed");
        assert!(!pending_writes());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn launcher_choices_survive_reopening_without_replacing_game_settings() {
        let directory =
            std::env::temp_dir().join(format!("sauerteig-session-storage-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        execute_named(&directory, "settings", Some(r#"{"invert_scroll":true}"#)).unwrap();
        let choices = SessionPreferences {
            player_name: "Builder".into(),
            server: "ws://127.0.0.1:4321/ws".into(),
            last_world: Some("world-123".into()),
        };
        execute_named(
            &directory,
            "session",
            Some(&serde_json::to_string(&choices).unwrap()),
        )
        .unwrap();
        let reopened: SessionPreferences =
            serde_json::from_str(&execute_named(&directory, "session", None).unwrap().unwrap())
                .unwrap();
        assert_eq!(reopened, choices);
        assert_eq!(
            execute_named(&directory, "settings", None)
                .unwrap()
                .as_deref(),
            Some(r#"{"invert_scroll":true}"#)
        );
        assert_eq!(
            serde_json::from_str::<SessionPreferences>("{}").unwrap(),
            SessionPreferences::default()
        );
        assert!(serde_json::from_str::<SessionPreferences>("not valid json").is_err());
        std::fs::remove_dir_all(directory).unwrap();
    }

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
