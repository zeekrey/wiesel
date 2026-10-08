//! Private, best-effort local diagnostics. No API accepts application text, errors,
//! credentials, URLs, settings content, or remote payloads. Request model IDs and
//! raw gateway error types (bounded to 256 UTF-8 bytes) are the only string metadata.
//! Error types are server-controlled and may contain sensitive text.
//!
//! Call `Diagnostics::init` off the UI thread and keep a cloneable handle. `record`
//! only tries a bounded queue; a false return means the event was dropped. Clear,
//! flush, and shutdown enqueue barriers without waiting; wait on their `Completion`
//! off the UI thread. Dropping all handles also drains the queue and closes the writer.
//! Clear refuses to delete another session's active file. Retention runs at init,
//! write, and flush barriers, not on an idle timer. It prunes inactive files older
//! than seven days and caps recognized logs at 20 MiB. Active files are protected;
//! if they prevent the byte budget, new events are dropped instead of deleting them.

use serde::{Serialize, Serializer};
use std::{
    ffi::{CStr, CString, OsStr},
    fs::{File, Metadata},
    io::{self, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    path::{Component, Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const ROTATE_BYTES: u64 = 2 * 1024 * 1024;
const RETAIN_BYTES: u64 = 20 * 1024 * 1024;
const RETAIN_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const QUEUE_CAPACITY: usize = 256;
const LOCK_NAME: &str = ".wiesel-diagnostics.lock";

/// Closed categories; never substitute application data for one of these names.
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Application,
    Authentication,
    Models,
    Request,
    Settings,
    Keychain,
    Hotkey,
    Selection,
    Accessibility,
    Diagnostics,
}

/// Safe lifecycle labels, not arbitrary messages.
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    Started,
    Succeeded,
    Failed,
    Cancelled,
    Rejected,
}

/// Allowlisted failure classifications. Map raw errors outside this module;
/// never pass their display text or source chains to the logger.
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureCode {
    Unauthorized,
    AccessDenied,
    AllowanceExhausted,
    Conflict,
    RateLimited,
    Server,
    Transport,
    Timeout,
    InvalidResponse,
    NoText,
    InputTooLarge,
    Storage,
    Permission,
    Unavailable,
    UnknownOutcome,
}

/// Closed operation stages, never a caller-provided label.
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    ValidateCredential,
    BuildClient,
    BuildRequest,
    Send,
    DecodeResponse,
    ParseResponse,
    ReadStream,
    OpenFolder,
    ClearLogs,
}

/// A bounded model identifier, not arbitrary text. Invalid identifiers are omitted.
#[derive(Clone, Copy, Debug)]
pub struct ModelId {
    bytes: [u8; 128],
    len: u8,
}
impl ModelId {
    pub fn parse(value: &str) -> Option<Self> {
        if value.is_empty()
            || value.len() > 128
            || value.contains("//")
            || ["wd_", "sk-", "sk_"]
                .iter()
                .any(|prefix| value.starts_with(prefix))
            || !value.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'_' | b'.' | b':')
            })
        {
            return None;
        }
        let mut bytes = [0; 128];
        bytes[..value.len()].copy_from_slice(value.as_bytes());
        Some(Self {
            bytes,
            len: value.len() as u8,
        })
    }
}
impl Serialize for ModelId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // Construction permits ASCII only.
        serializer.serialize_str(
            std::str::from_utf8(&self.bytes[..usize::from(self.len)])
                .map_err(serde::ser::Error::custom)?,
        )
    }
}

/// Original decoded error.type, without normalization, filtering or truncation.
/// Oversized values are omitted. JSON serialization escapes newlines/control bytes.
#[derive(Clone, Serialize)]
pub struct GatewayErrorType(Box<str>);
impl GatewayErrorType {
    pub fn parse(value: &str) -> Option<Self> {
        (value.len() <= 256).then(|| Self(value.into()))
    }
}
impl std::fmt::Debug for GatewayErrorType {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Preserve raw type only in the explicit JSONL field, not error debug chains.
        formatter.write_str("GatewayErrorType([redacted])")
    }
}

/// A bounded diagnostic event, without arbitrary extensible metadata.
#[derive(Clone, Debug, Serialize)]
pub struct Event {
    category: Category,
    kind: EventKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    failure_code: Option<FailureCode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stage: Option<Stage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    http_status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    elapsed_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model_id: Option<ModelId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_type: Option<GatewayErrorType>,
}

impl Event {
    /// Make an allowlisted lifecycle event.
    pub fn new(category: Category, kind: EventKind) -> Self {
        Self {
            category,
            kind,
            failure_code: None,
            stage: None,
            http_status: None,
            elapsed_ms: None,
            model_id: None,
            error_type: None,
        }
    }

    /// Make a failed event with a safe classification, never an error string.
    pub fn failure(category: Category, code: FailureCode) -> Self {
        Self {
            category,
            kind: EventKind::Failed,
            failure_code: Some(code),
            stage: None,
            http_status: None,
            elapsed_ms: None,
            model_id: None,
            error_type: None,
        }
    }

    pub fn with_model_id(mut self, model_id: Option<ModelId>) -> Self {
        self.model_id = model_id;
        self
    }

    pub fn with_error_type(mut self, error_type: Option<GatewayErrorType>) -> Self {
        self.error_type = error_type;
        self
    }

    /// Add a closed stage and elapsed action time measured by the caller.
    pub fn at_stage(mut self, stage: Stage, elapsed_ms: u64) -> Self {
        self.stage = Some(stage);
        self.elapsed_ms = Some(elapsed_ms);
        self
    }

    /// Add an HTTP status only if it is in the standardized 100..=599 range.
    /// Invalid numbers are omitted; this field cannot carry arbitrary data.
    pub fn with_http_status(mut self, status: u16) -> Self {
        self.http_status = (100..=599).contains(&status).then_some(status);
        self
    }
}

/// Opaque diagnostic-only action counter, generated by `new_action_id`.
/// IDs are scoped to the session; they are not gateway/account/request identifiers.
#[derive(Clone, Copy, Debug)]
pub struct ActionId(u64);

impl Serialize for ActionId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(&format_args!("{:016x}", self.0))
    }
}

/// Fixed local error classifications; no raw OS error text or paths are retained.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DiagnosticError {
    #[error("The diagnostic home directory is unavailable or invalid")]
    InvalidHome,
    #[error("An unsafe diagnostic directory or file was refused")]
    UnsafePath,
    #[error("Diagnostic storage I/O failed ({0:?})")]
    Io(io::ErrorKind),
    #[error("Cannot generate a diagnostic session identifier")]
    Entropy,
    #[error("The diagnostic queue is full; retry off the UI thread")]
    QueueFull,
    #[error("The diagnostic worker has stopped")]
    WorkerStopped,
    #[error("Another diagnostic session is active; close it before clearing logs")]
    OtherSessionActive,
    #[error("Cannot retain another diagnostic event within the storage budget")]
    StorageBudget,
    #[error("Cannot serialize a diagnostic event")]
    Serialization,
}

impl From<io::Error> for DiagnosticError {
    fn from(error: io::Error) -> Self {
        Self::Io(error.kind())
    }
}

/// A queued operation's completion. Waiting does filesystem work only in the
/// writer, but blocks the caller: use a background task, never the UI thread.
pub struct Completion(Receiver<Result<(), DiagnosticError>>);

impl Completion {
    /// Wait for the barrier and receive its genuine result (including write errors).
    pub fn wait(self) -> Result<(), DiagnosticError> {
        self.0.recv().map_err(|_| DiagnosticError::WorkerStopped)?
    }

    /// Poll without waiting. A returned result consumes the completion message.
    pub fn try_wait(&self) -> Option<Result<(), DiagnosticError>> {
        match self.0.try_recv() {
            Ok(result) => Some(result),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err(DiagnosticError::WorkerStopped)),
        }
    }
}

/// A cloneable bounded-queue handle. Init/folder access are fallible synchronous
/// filesystem operations and must run off the UI thread. Recording never waits
/// for the writer; successful enqueue does not promise durable storage.
#[derive(Clone)]
pub struct Diagnostics {
    sender: SyncSender<Command>,
    next_action: Arc<AtomicU64>,
    directory: Arc<Directory>,
}

impl Diagnostics {
    /// Ensure `~/Library/Logs/Wiesel`, initialize retention, and start one writer.
    /// Failure must not prevent the application from starting or doing its work.
    pub fn init() -> Result<Self, DiagnosticError> {
        let home = std::env::var_os("HOME").ok_or(DiagnosticError::InvalidHome)?;
        let path = PathBuf::from(home).join("Library/Logs/Wiesel");
        Self::init_at(&path, Limits::default())
    }

    fn init_at(path: &Path, limits: Limits) -> Result<Self, DiagnosticError> {
        let directory = Arc::new(Directory::ensure(path)?);
        let mut bytes = [0; 16];
        getrandom::fill(&mut bytes).map_err(|_| DiagnosticError::Entropy)?;
        let session = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        let mut writer = Writer::new(directory.clone(), session, limits)?;
        let (sender, receiver) = mpsc::sync_channel(QUEUE_CAPACITY);
        thread::Builder::new()
            .name("wiesel-diagnostics".into())
            .spawn(move || writer.run(receiver))?;
        Ok(Self {
            sender,
            next_action: Arc::new(AtomicU64::new(1)),
            directory,
        })
    }

    /// Generate an action ID without accepting caller-provided data.
    pub fn new_action_id(&self) -> ActionId {
        ActionId(self.next_action.fetch_add(1, Ordering::Relaxed))
    }

    /// Best-effort nonblocking enqueue. False means full/stopped; ignore it in
    /// application workflows. Only a closed event and optional opaque ID are accepted.
    pub fn record(&self, event: Event, action_id: Option<ActionId>) -> bool {
        self.sender
            .try_send(Command::Record(Record {
                timestamp: timestamp_ms(),
                action_id,
                event,
            }))
            .is_ok()
    }

    /// Enqueue a clear barrier. Records before it are drained then removed; records
    /// after it go to a new file. Other active sessions cause a reported refusal.
    /// Failed clears still reopen storage so later logging can recover.
    pub fn clear(&self) -> Result<Completion, DiagnosticError> {
        self.barrier(Operation::Clear)
    }

    /// Enqueue a durability barrier. Reports the first write error since the last
    /// flush, as well as sync/retention errors. No unbounded control queue is used.
    pub fn flush(&self) -> Result<Completion, DiagnosticError> {
        self.barrier(Operation::Flush)
    }

    /// Enqueue final flush/close. The completion confirms the worker has closed
    /// its file; all handle clones are then stopped. Do not record after shutdown.
    pub fn shutdown(&self) -> Result<Completion, DiagnosticError> {
        self.barrier(Operation::Shutdown)
    }

    fn barrier(&self, operation: Operation) -> Result<Completion, DiagnosticError> {
        let (sender, receiver) = mpsc::sync_channel(1);
        self.sender
            .try_send(Command::Barrier(operation, sender))
            .map_err(|error| match error {
                TrySendError::Full(_) => DiagnosticError::QueueFull,
                TrySendError::Disconnected(_) => DiagnosticError::WorkerStopped,
            })?;
        Ok(Completion(receiver))
    }

    /// Verify/ensure the private directory and return its path for Finder.
    /// Run off the UI thread. Refuses a path replaced since initialization rather
    /// than opening an unrelated folder or silently redirecting the writer.
    pub fn log_directory(&self) -> Result<PathBuf, DiagnosticError> {
        self.directory.verify_path()?;
        Ok(self.directory.path.clone())
    }
}

#[derive(Serialize)]
struct Record {
    /// Unix epoch milliseconds, not locale-dependent wall-clock text.
    timestamp: u64,
    action_id: Option<ActionId>,
    #[serde(flatten)]
    event: Event,
}

#[derive(Serialize)]
struct Envelope<'a> {
    schema_version: u8,
    app_version: &'static str,
    session_id: &'a str,
    #[serde(flatten)]
    record: &'a Record,
}

fn timestamp_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

enum Command {
    Record(Record),
    Barrier(Operation, SyncSender<Result<(), DiagnosticError>>),
}

enum Operation {
    Clear,
    Flush,
    Shutdown,
}

#[derive(Clone, Copy)]
struct Limits {
    rotate: u64,
    retain: u64,
    age: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            rotate: ROTATE_BYTES,
            retain: RETAIN_BYTES,
            age: RETAIN_AGE,
        }
    }
}

struct ActiveFile {
    name: CString,
    file: File,
    bytes: u64,
    opened: SystemTime,
}

struct Writer {
    directory: Arc<Directory>,
    session: String,
    sequence: u64,
    active: Option<ActiveFile>,
    limits: Limits,
    pending_error: Option<DiagnosticError>,
}

impl Writer {
    fn new(
        directory: Arc<Directory>,
        session: String,
        limits: Limits,
    ) -> Result<Self, DiagnosticError> {
        let mut writer = Self {
            directory,
            session,
            sequence: 0,
            active: None,
            limits,
            pending_error: None,
        };
        let directory = writer.directory.clone();
        let _guard = directory.lock()?;
        writer.prune(0)?;
        writer.open_active()?;
        Ok(writer)
    }

    fn run(&mut self, receiver: Receiver<Command>) {
        while let Ok(command) = receiver.recv() {
            match command {
                Command::Record(record) => {
                    if let Err(error) = self.write(record) {
                        self.pending_error.get_or_insert(error);
                    }
                }
                Command::Barrier(operation, reply) => {
                    let shutdown = matches!(operation, Operation::Shutdown);
                    let result = match operation {
                        Operation::Clear => self.clear(),
                        Operation::Flush | Operation::Shutdown => self.flush(),
                    };
                    if shutdown {
                        self.active = None;
                    }
                    let _ = reply.try_send(result);
                    if shutdown {
                        return;
                    }
                }
            }
        }
        let _ = self.flush();
        self.active = None;
    }

    fn open_active(&mut self) -> Result<(), DiagnosticError> {
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or(DiagnosticError::StorageBudget)?;
        let name = cstring(OsStr::new(&format!(
            "wiesel-v1-{}-{:016x}.jsonl",
            self.session, self.sequence
        )))?;
        let file = self
            .directory
            .open(&name, libc::O_RDWR | libc::O_CREAT | libc::O_EXCL)?;
        file.lock()?;
        self.active = Some(ActiveFile {
            name,
            file,
            bytes: 0,
            opened: SystemTime::now(),
        });
        Ok(())
    }

    fn write(&mut self, record: Record) -> Result<(), DiagnosticError> {
        let mut line = serde_json::to_vec(&Envelope {
            schema_version: 1,
            app_version: env!("CARGO_PKG_VERSION"),
            session_id: &self.session,
            record: &record,
        })
        .map_err(|_| DiagnosticError::Serialization)?;
        line.push(b'\n');
        let directory = self.directory.clone();
        let _guard = directory.lock()?;
        if let Some(active) = &self.active
            && (active.bytes + line.len() as u64 > self.limits.rotate
                || active.opened.elapsed().unwrap_or_default() > self.limits.age)
        {
            active.file.sync_data()?;
            self.active = None;
        }
        self.prune(line.len() as u64)?;
        if self.active.is_none() {
            self.open_active()?;
        }
        if let Some(active) = &mut self.active {
            if let Err(error) = active.file.write_all(&line) {
                // A partial record must not poison the next JSONL write. Close this
                // file even if truncation also fails; the next event gets a fresh one.
                let _ = active.file.set_len(active.bytes);
                self.active = None;
                return Err(error.into());
            }
            active.bytes += line.len() as u64;
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<(), DiagnosticError> {
        let result = (|| {
            let directory = self.directory.clone();
            let _guard = directory.lock()?;
            if let Some(active) = &self.active {
                active.file.sync_data()?;
            }
            self.prune(0)
        })();
        match self.pending_error.take() {
            Some(error) => Err(error),
            None => result,
        }
    }

    fn clear(&mut self) -> Result<(), DiagnosticError> {
        let directory = self.directory.clone();
        let _guard = directory.lock()?;
        let result = (|| {
            let candidates = self.directory.candidates()?;
            // Check every foreign file before closing/deleting our own. The global
            // lock prevents new sessions/rotations from appearing during this scan.
            let mut locked = Vec::new();
            for candidate in candidates {
                if self
                    .active
                    .as_ref()
                    .is_some_and(|active| active.name == candidate.name)
                {
                    continue;
                }
                let file = self.directory.open_candidate(&candidate)?;
                match file.try_lock() {
                    Ok(()) => locked.push((candidate, file)),
                    Err(std::fs::TryLockError::WouldBlock) => {
                        return Err(DiagnosticError::OtherSessionActive);
                    }
                    Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
                }
            }
            if let Some(active) = self.active.take() {
                let sync_result = active.file.sync_data();
                // Close before unlink; no writes can run concurrently in this worker.
                let candidate = Candidate::from_metadata(active.name, &active.file.metadata()?);
                drop(active.file);
                self.directory.remove(&candidate)?;
                sync_result?;
            }
            let mut first_error = None;
            for (candidate, _file) in locked {
                if let Err(error) = self.directory.remove(&candidate) {
                    first_error.get_or_insert(error);
                }
            }
            first_error.map_or(Ok(()), Err)
        })();
        // Recovery is attempted even when unlink or sync failed. No deleted/open
        // descriptor is kept, and a future record retries if reopening fails.
        let reopen = if self.active.is_none() {
            self.open_active()
        } else {
            Ok(())
        };
        let result = result.and(reopen);
        if result.is_ok() {
            self.pending_error = None;
        }
        result
    }

    fn prune(&self, reserve: u64) -> Result<(), DiagnosticError> {
        let mut candidates = self.directory.candidates()?;
        candidates.sort_by_key(|candidate| candidate.modified);
        let mut total = candidates.iter().try_fold(reserve, |total, candidate| {
            total
                .checked_add(candidate.bytes)
                .ok_or(DiagnosticError::StorageBudget)
        })?;
        let now = SystemTime::now();
        for candidate in candidates {
            let expired =
                now.duration_since(candidate.modified).unwrap_or_default() > self.limits.age;
            if !expired && total <= self.limits.retain {
                continue;
            }
            if self
                .active
                .as_ref()
                .is_some_and(|active| active.name == candidate.name)
            {
                continue;
            }
            let file = self.directory.open_candidate(&candidate)?;
            match file.try_lock() {
                Ok(()) => {
                    self.directory.remove(&candidate)?;
                    total -= candidate.bytes;
                }
                Err(std::fs::TryLockError::WouldBlock) => continue,
                Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
            }
        }
        if total > self.limits.retain {
            return Err(DiagnosticError::StorageBudget);
        }
        Ok(())
    }
}

struct Candidate {
    name: CString,
    device: u64,
    inode: u64,
    bytes: u64,
    modified: SystemTime,
}

impl Candidate {
    fn from_metadata(name: CString, metadata: &Metadata) -> Self {
        Self {
            name,
            device: metadata.dev(),
            inode: metadata.ino(),
            bytes: metadata.len(),
            modified: metadata.modified().unwrap_or(UNIX_EPOCH),
        }
    }
}

/// All operations are relative to this pinned, no-follow directory descriptor.
/// No pathname traversal can redirect writes/deletions after initialization.
struct Directory {
    file: File,
    path: PathBuf,
}

impl Directory {
    fn ensure(path: &Path) -> Result<Self, DiagnosticError> {
        Self::open_path(path, true)
    }

    fn open_path(path: &Path, ensure: bool) -> Result<Self, DiagnosticError> {
        if !path.is_absolute() {
            return Err(DiagnosticError::InvalidHome);
        }
        let root = cstring(OsStr::new("/"))?;
        // SAFETY: root is a NUL-terminated pathname; open returns an owned fd.
        let mut directory = file_from_fd(unsafe {
            libc::open(
                root.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
            )
        })?;
        let components: Vec<_> = path.components().collect();
        for (index, component) in components.iter().enumerate() {
            let name = match component {
                Component::RootDir => continue,
                Component::Normal(name) => cstring(name)?,
                _ => return Err(DiagnosticError::UnsafePath),
            };
            let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
            // SAFETY: fd is live and name is NUL-terminated, single-component.
            let mut fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags) };
            if ensure && fd == -1 && io::Error::last_os_error().kind() == io::ErrorKind::NotFound {
                // SAFETY: mkdirat cannot follow a final symlink and uses the pinned parent.
                if unsafe { libc::mkdirat(directory.as_raw_fd(), name.as_ptr(), 0o700) } == -1
                    && io::Error::last_os_error().kind() != io::ErrorKind::AlreadyExists
                {
                    return Err(io::Error::last_os_error().into());
                }
                // SAFETY: same single-component no-follow open after creation.
                fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags) };
            }
            directory = file_from_fd(fd)?;
            if index == components.len() - 1 {
                let metadata = directory.metadata()?;
                if metadata.uid() != current_uid() || !metadata.is_dir() {
                    return Err(DiagnosticError::UnsafePath);
                }
                // SAFETY: chmod applies only to our verified owned directory fd.
                if ensure && unsafe { libc::fchmod(directory.as_raw_fd(), 0o700) } == -1 {
                    return Err(io::Error::last_os_error().into());
                }
            }
        }
        Ok(Self {
            file: directory,
            path: path.to_owned(),
        })
    }

    fn verify_path(&self) -> Result<(), DiagnosticError> {
        let reopened = Self::open_path(&self.path, false)?;
        if !same_file(&self.file.metadata()?, &reopened.file.metadata()?) {
            return Err(DiagnosticError::UnsafePath);
        }
        // SAFETY: identity was checked before changing permissions; this cannot
        // modify a replacement/unrelated directory at the original pathname.
        if unsafe { libc::fchmod(self.file.as_raw_fd(), 0o700) } == -1 {
            return Err(io::Error::last_os_error().into());
        }
        Ok(())
    }

    fn open(&self, name: &CStr, flags: i32) -> Result<File, DiagnosticError> {
        // SAFETY: names are internal single-component names; fd remains pinned.
        let file = file_from_fd(unsafe {
            libc::openat(
                self.file.as_raw_fd(),
                name.as_ptr(),
                flags | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
                0o600,
            )
        })?;
        if !owned_regular(&file.metadata()?) {
            return Err(DiagnosticError::UnsafePath);
        }
        Ok(file)
    }

    fn lock(&self) -> Result<DirectoryGuard, DiagnosticError> {
        let name = cstring(OsStr::new(LOCK_NAME))?;
        // Never unlink the lock file: all processes must lock the same inode.
        let file = self.open(&name, libc::O_RDWR | libc::O_CREAT)?;
        file.lock()?;
        Ok(DirectoryGuard(file))
    }

    fn candidates(&self) -> Result<Vec<Candidate>, DiagnosticError> {
        // Open a separate description, not dup: directory offsets must not be shared.
        let dot = cstring(OsStr::new("."))?;
        // SAFETY: fd and dot are valid; fdopendir owns the successful open's fd.
        let fd = unsafe {
            libc::openat(
                self.file.as_raw_fd(),
                dot.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
            )
        };
        if fd == -1 {
            return Err(io::Error::last_os_error().into());
        }
        // SAFETY: fd is a newly owned directory descriptor.
        let pointer = unsafe { libc::fdopendir(fd) };
        if pointer.is_null() {
            // SAFETY: fdopendir failed and did not take ownership.
            unsafe { libc::close(fd) };
            return Err(io::Error::last_os_error().into());
        }
        let stream = DirectoryStream(pointer);
        let mut candidates = Vec::new();
        loop {
            set_errno(0);
            // SAFETY: stream is live and exclusively used; entry lasts until next call.
            let entry = unsafe { libc::readdir(stream.0) };
            if entry.is_null() {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(0) {
                    return Err(error.into());
                }
                break;
            }
            // SAFETY: POSIX dirent names are NUL-terminated, used before next readdir.
            let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
            if !recognized_name(name.to_bytes()) {
                continue;
            }
            let metadata = self.stat(name)?;
            if metadata.st_mode & libc::S_IFMT != libc::S_IFREG
                || metadata.st_uid != current_uid()
                || metadata.st_nlink != 1
                || metadata.st_mode & 0o777 != 0o600
            {
                continue;
            }
            let file = self.open(name, libc::O_RDWR)?;
            let actual = file.metadata()?;
            if actual.ino() != metadata.st_ino || actual.dev() != metadata.st_dev as u64 {
                return Err(DiagnosticError::UnsafePath);
            }
            candidates.push(Candidate::from_metadata(name.to_owned(), &actual));
        }
        Ok(candidates)
    }

    fn stat(&self, name: &CStr) -> Result<libc::stat, DiagnosticError> {
        let mut stat = std::mem::MaybeUninit::uninit();
        // SAFETY: live directory, valid name, writable stat; final symlink is not followed.
        if unsafe {
            libc::fstatat(
                self.file.as_raw_fd(),
                name.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } == -1
        {
            return Err(io::Error::last_os_error().into());
        }
        // SAFETY: successful fstatat initialized the structure.
        Ok(unsafe { stat.assume_init() })
    }

    fn open_candidate(&self, candidate: &Candidate) -> Result<File, DiagnosticError> {
        let file = self.open(&candidate.name, libc::O_RDWR)?;
        let metadata = file.metadata()?;
        if metadata.dev() != candidate.device || metadata.ino() != candidate.inode {
            return Err(DiagnosticError::UnsafePath);
        }
        Ok(file)
    }

    fn remove(&self, candidate: &Candidate) -> Result<(), DiagnosticError> {
        let stat = self.stat(&candidate.name)?;
        if stat.st_dev as u64 != candidate.device
            || stat.st_ino != candidate.inode
            || stat.st_mode & libc::S_IFMT != libc::S_IFREG
            || stat.st_nlink != 1
            || stat.st_uid != current_uid()
            || stat.st_mode & 0o777 != 0o600
        {
            return Err(DiagnosticError::UnsafePath);
        }
        // SAFETY: unlinkat never follows symlinks and only touches the pinned directory.
        // App processes coordinate under the directory lock; a hostile same-UID
        // process can still race a final rename, but cannot redirect deletion outside it.
        if unsafe { libc::unlinkat(self.file.as_raw_fd(), candidate.name.as_ptr(), 0) } == -1 {
            return Err(io::Error::last_os_error().into());
        }
        Ok(())
    }
}

struct DirectoryGuard(File);
impl Drop for DirectoryGuard {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

struct DirectoryStream(*mut libc::DIR);
impl Drop for DirectoryStream {
    fn drop(&mut self) {
        // SAFETY: this stream uniquely owns a successful fdopendir result.
        unsafe { libc::closedir(self.0) };
    }
}

fn cstring(value: &OsStr) -> Result<CString, DiagnosticError> {
    CString::new(value.as_bytes()).map_err(|_| DiagnosticError::UnsafePath)
}

fn file_from_fd(fd: i32) -> Result<File, DiagnosticError> {
    if fd == -1 {
        return Err(io::Error::last_os_error().into());
    }
    // SAFETY: all callers pass newly opened, otherwise unowned successful fds.
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn current_uid() -> u32 {
    // SAFETY: getuid has no arguments or pointer requirements.
    unsafe { libc::getuid() }
}

fn owned_regular(metadata: &Metadata) -> bool {
    metadata.is_file()
        && metadata.uid() == current_uid()
        && metadata.nlink() == 1
        && metadata.mode() & 0o777 == 0o600
}

fn same_file(left: &Metadata, right: &Metadata) -> bool {
    left.dev() == right.dev() && left.ino() == right.ino()
}

fn recognized_name(name: &[u8]) -> bool {
    let Some(rest) = name.strip_prefix(b"wiesel-v1-") else {
        return false;
    };
    let Some(rest) = rest.strip_suffix(b".jsonl") else {
        return false;
    };
    rest.len() == 49
        && rest[32] == b'-'
        && rest[..32]
            .iter()
            .chain(&rest[33..])
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
}

fn set_errno(value: i32) {
    // SAFETY: these functions return the calling thread's valid errno pointer.
    #[cfg(target_os = "macos")]
    unsafe {
        *libc::__error() = value;
    }
    #[cfg(target_os = "linux")]
    unsafe {
        *libc::__errno_location() = value;
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::fs::{self, DirBuilder, FileTimes, OpenOptions, Permissions};
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt, symlink};

    pub(crate) struct Temp {
        root: PathBuf,
        pub(crate) logs: PathBuf,
    }

    impl Temp {
        pub(crate) fn new() -> Self {
            let mut bytes = [0; 16];
            getrandom::fill(&mut bytes).unwrap();
            let suffix: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
            // macOS /var is a symlink. Canonicalize only this test-owned base;
            // production deliberately refuses symlinked paths.
            let root = std::env::temp_dir()
                .canonicalize()
                .unwrap()
                .join(format!("wiesel-diagnostics-test-{suffix}"));
            DirBuilder::new().mode(0o700).create(&root).unwrap();
            let logs = root.join("logs");
            Self { root, logs }
        }

        pub(crate) fn logger(&self) -> Diagnostics {
            Diagnostics::init_at(&self.logs, Limits::default()).unwrap()
        }

        fn owned_log(&self, sequence: u64, content: &[u8]) -> PathBuf {
            let path = self.logs.join(format!(
                "wiesel-v1-00000000000000000000000000000000-{sequence:016x}.jsonl"
            ));
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(0o600)
                .open(&path)
                .unwrap();
            file.write_all(content).unwrap();
            path
        }
    }

    impl Drop for Temp {
        fn drop(&mut self) {
            if self.logs.is_dir() {
                let _ = fs::set_permissions(&self.logs, Permissions::from_mode(0o700));
            }
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn event() -> Event {
        Event::new(Category::Application, EventKind::Started)
    }

    fn log_paths(path: &Path) -> Vec<PathBuf> {
        fs::read_dir(path)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.is_file() && recognized_name(path.file_name().unwrap().as_bytes()))
            .collect()
    }

    pub(crate) fn records(path: &Path) -> Vec<serde_json::Value> {
        log_paths(path)
            .iter()
            .flat_map(|path| {
                fs::read_to_string(path)
                    .unwrap()
                    .lines()
                    .map(|line| serde_json::from_str(line).unwrap())
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    #[test]
    fn record_writes_parseable_closed_schema_with_diagnostic_ids() {
        let temp = Temp::new();
        let logger = temp.logger();
        let before = timestamp_ms();
        let action = logger.new_action_id();
        assert!(logger.record(
            Event::failure(Category::Request, FailureCode::Unauthorized),
            Some(action)
        ));
        logger.flush().unwrap().wait().unwrap();
        logger.shutdown().unwrap().wait().unwrap();
        let events = records(&temp.logs);
        assert_eq!(events.len(), 1);
        let value = &events[0];
        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["app_version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(value["action_id"], "0000000000000001");
        assert_eq!(value["category"], "request");
        assert_eq!(value["kind"], "failed");
        assert_eq!(value["failure_code"], "unauthorized");
        assert!(value["timestamp"].as_u64().unwrap() >= before);
        assert_eq!(value["session_id"].as_str().unwrap().len(), 32);
        assert_eq!(value.as_object().unwrap().len(), 8);
    }

    #[test]
    fn event_serializes_only_closed_stage_numeric_status_and_elapsed_fields() {
        let event = Event::failure(Category::Request, FailureCode::Server)
            .at_stage(Stage::DecodeResponse, 125)
            .with_http_status(503);
        assert_eq!(
            serde_json::to_value(event).unwrap(),
            serde_json::json!({
                "category": "request", "kind": "failed", "failure_code": "server",
                "stage": "decode_response", "http_status": 503, "elapsed_ms": 125,
            })
        );
    }

    #[test]
    fn request_metadata_serializes_validated_model_and_known_error_type() {
        let event = Event::failure(Category::Request, FailureCode::UnknownOutcome)
            .with_model_id(ModelId::parse("openai/gpt-4o-mini"))
            .with_error_type(GatewayErrorType::parse("invalid_request_error"));
        let value = serde_json::to_value(event).unwrap();
        assert_eq!(value["model_id"], "openai/gpt-4o-mini");
        assert_eq!(value["error_type"], "invalid_request_error");
    }

    #[test]
    fn model_metadata_rejects_content_urls_credentials_and_oversized_identifiers() {
        for model in [
            "",
            "private prompt",
            "https://wiesel.run",
            "wd_private",
            "sk-private",
            "sk_private",
            "model\nprivate",
            "model?token=private",
            "模型",
        ] {
            assert!(ModelId::parse(model).is_none(), "{model:?}");
        }
        assert!(ModelId::parse(&"m".repeat(129)).is_none());
        assert!(ModelId::parse(&"m".repeat(128)).is_some());
    }

    #[test]
    fn raw_remote_error_type_preserves_unknown_unicode_and_control_characters() {
        let raw = "new.gateway/error:模型\n\r\t\u{0000}";
        let value = serde_json::to_value(
            Event::failure(Category::Request, FailureCode::UnknownOutcome)
                .with_error_type(GatewayErrorType::parse(raw)),
        )
        .unwrap();
        assert_eq!(value["error_type"], raw);
        let line = serde_json::to_string(&value).unwrap();
        assert_eq!(line.lines().count(), 1);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&line).unwrap()["error_type"],
            raw
        );
    }

    #[test]
    fn raw_error_type_size_bound_omits_without_truncating() {
        assert!(GatewayErrorType::parse(&"x".repeat(256)).is_some());
        assert!(GatewayErrorType::parse(&"x".repeat(257)).is_none());
        assert!(GatewayErrorType::parse(&"é".repeat(129)).is_none());
        assert_eq!(
            serde_json::to_value(GatewayErrorType::parse("")).unwrap(),
            ""
        );
    }

    #[test]
    fn event_omits_invalid_http_status_numbers() {
        for status in [0, 99, 600, u16::MAX] {
            let value = serde_json::to_value(event().with_http_status(status)).unwrap();
            assert!(value.get("http_status").is_none());
        }
        for status in [100, 200, 599] {
            assert_eq!(
                serde_json::to_value(event().with_http_status(status)).unwrap()["http_status"],
                status
            );
        }
    }

    #[test]
    fn init_enforces_private_directory_and_file_permissions() {
        let temp = Temp::new();
        fs::create_dir(&temp.logs).unwrap();
        fs::set_permissions(&temp.logs, Permissions::from_mode(0o755)).unwrap();
        let logger = temp.logger();
        assert!(logger.record(event(), None));
        logger.shutdown().unwrap().wait().unwrap();
        assert_eq!(fs::metadata(&temp.logs).unwrap().mode() & 0o777, 0o700);
        for entry in fs::read_dir(&temp.logs).unwrap() {
            assert_eq!(entry.unwrap().metadata().unwrap().mode() & 0o777, 0o600);
        }
    }

    #[test]
    fn write_rotates_and_prunes_oldest_files_within_byte_budget() {
        let temp = Temp::new();
        let directory = Arc::new(Directory::ensure(&temp.logs).unwrap());
        let limits = Limits {
            rotate: 700,
            retain: 1500,
            age: RETAIN_AGE,
        };
        let mut writer =
            Writer::new(directory, "11111111111111111111111111111111".into(), limits).unwrap();
        for _ in 0..30 {
            writer
                .write(Record {
                    timestamp: timestamp_ms(),
                    action_id: None,
                    event: event(),
                })
                .unwrap();
        }
        writer.flush().unwrap();
        let paths = log_paths(&temp.logs);
        assert!(paths.len() >= 2);
        let sizes: Vec<_> = paths
            .iter()
            .map(|path| fs::metadata(path).unwrap().len())
            .collect();
        assert!(sizes.iter().all(|size| *size <= limits.rotate));
        assert!(sizes.iter().sum::<u64>() <= limits.retain);
        assert!(!records(&temp.logs).is_empty());
    }

    #[test]
    fn init_enforces_default_twenty_mebibyte_retention_budget() {
        let temp = Temp::new();
        Directory::ensure(&temp.logs).unwrap();
        for sequence in 1..=11 {
            let path = temp.owned_log(sequence, b"");
            File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_len(ROTATE_BYTES)
                .unwrap();
        }
        let logger = temp.logger();
        logger.shutdown().unwrap().wait().unwrap();
        let total: u64 = log_paths(&temp.logs)
            .iter()
            .map(|path| fs::metadata(path).unwrap().len())
            .sum();
        assert_eq!(total, RETAIN_BYTES);
    }

    #[test]
    fn init_prunes_only_expired_inactive_logs() {
        let temp = Temp::new();
        Directory::ensure(&temp.logs).unwrap();
        let old = temp.owned_log(1, b"{}\n");
        let fresh = temp.owned_log(2, b"{}\n");
        File::options()
            .write(true)
            .open(&old)
            .unwrap()
            .set_times(
                FileTimes::new()
                    .set_modified(SystemTime::now() - RETAIN_AGE - Duration::from_secs(60)),
            )
            .unwrap();
        let logger = temp.logger();
        logger.shutdown().unwrap().wait().unwrap();
        assert!(!old.exists());
        assert!(fresh.exists());
    }

    #[test]
    fn clear_closes_old_file_and_accepts_fresh_writes() {
        let temp = Temp::new();
        let logger = temp.logger();
        assert!(logger.record(event(), Some(logger.new_action_id())));
        logger.flush().unwrap().wait().unwrap();
        let before = log_paths(&temp.logs);
        logger.clear().unwrap().wait().unwrap();
        assert!(before.iter().all(|path| !path.exists()));
        assert!(records(&temp.logs).is_empty());
        assert!(logger.record(Event::new(Category::Settings, EventKind::Succeeded), None));
        logger.shutdown().unwrap().wait().unwrap();
        let events = records(&temp.logs);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["category"], "settings");
    }

    #[test]
    fn clear_and_retention_preserve_unrelated_symlink_directory_and_hardlink_entries() {
        let temp = Temp::new();
        Directory::ensure(&temp.logs).unwrap();
        let unrelated = temp.logs.join("notes.jsonl");
        fs::write(&unrelated, "do not remove").unwrap();
        let target = temp.root.join("outside");
        fs::write(&target, "outside contents").unwrap();
        let link = temp
            .logs
            .join("wiesel-v1-00000000000000000000000000000000-0000000000000001.jsonl");
        symlink(&target, &link).unwrap();
        let hardlink = temp
            .logs
            .join("wiesel-v1-00000000000000000000000000000000-0000000000000002.jsonl");
        fs::hard_link(&target, &hardlink).unwrap();
        let directory = temp
            .logs
            .join("wiesel-v1-00000000000000000000000000000000-0000000000000003.jsonl");
        fs::create_dir(&directory).unwrap();
        let wrong_mode = temp.owned_log(4, b"not app-owned permissions");
        fs::set_permissions(&wrong_mode, Permissions::from_mode(0o644)).unwrap();
        let removable = temp.owned_log(5, b"{}\n");
        let logger = temp.logger();
        logger.clear().unwrap().wait().unwrap();
        logger.shutdown().unwrap().wait().unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "outside contents");
        assert!(fs::symlink_metadata(&link).unwrap().is_symlink());
        assert!(
            hardlink.exists() && directory.is_dir() && wrong_mode.exists() && unrelated.exists()
        );
        assert!(!removable.exists());
    }

    #[test]
    fn init_refuses_symlinked_log_directory_and_lock_file() {
        let temp = Temp::new();
        let outside = temp.root.join("outside");
        fs::create_dir(&outside).unwrap();
        symlink(&outside, &temp.logs).unwrap();
        assert!(Diagnostics::init_at(&temp.logs, Limits::default()).is_err());
        fs::remove_file(&temp.logs).unwrap();
        Directory::ensure(&temp.logs).unwrap();
        let target = temp.root.join("lock-target");
        fs::write(&target, "unchanged").unwrap();
        symlink(&target, temp.logs.join(LOCK_NAME)).unwrap();
        assert!(Diagnostics::init_at(&temp.logs, Limits::default()).is_err());
        assert_eq!(fs::read_to_string(target).unwrap(), "unchanged");
    }

    #[test]
    fn clear_refuses_another_active_session_then_succeeds_after_it_closes() {
        let temp = Temp::new();
        let first = temp.logger();
        let second = temp.logger();
        assert!(second.record(event(), None));
        second.flush().unwrap().wait().unwrap();
        let second_paths = log_paths(&temp.logs);
        assert_eq!(
            first.clear().unwrap().wait(),
            Err(DiagnosticError::OtherSessionActive)
        );
        assert!(second_paths.iter().all(|path| path.exists()));
        assert!(first.record(event(), None));
        first.flush().unwrap().wait().unwrap();
        second.shutdown().unwrap().wait().unwrap();
        first.clear().unwrap().wait().unwrap();
        assert!(first.record(event(), None));
        first.shutdown().unwrap().wait().unwrap();
        assert_eq!(records(&temp.logs).len(), 1);
    }

    // The same test binary is an isolated child process, never the real app.
    #[test]
    fn cross_process_active_session_helper() {
        let Some(path) = std::env::var_os("WIESEL_DIAGNOSTIC_TEST_CHILD") else {
            return;
        };
        let root = PathBuf::from(path);
        let logger = Diagnostics::init_at(&root.join("logs"), Limits::default()).unwrap();
        assert!(logger.record(event(), None));
        logger.flush().unwrap().wait().unwrap();
        fs::write(root.join("ready"), b"ready").unwrap();
        let mut byte = [0];
        std::io::Read::read_exact(&mut std::io::stdin(), &mut byte).unwrap();
        logger.shutdown().unwrap().wait().unwrap();
    }

    #[test]
    fn clear_respects_active_file_locks_from_another_process() {
        struct ChildGuard(std::process::Child);
        impl Drop for ChildGuard {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let temp = Temp::new();
        let logger = temp.logger();
        let test_module = module_path!().split_once("::").unwrap().1;
        let helper = format!("{test_module}::cross_process_active_session_helper");
        let mut child = ChildGuard(
            std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", &helper, "--nocapture"])
                .env("WIESEL_DIAGNOSTIC_TEST_CHILD", &temp.root)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .spawn()
                .unwrap(),
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !temp.root.join("ready").exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "child did not become ready"
            );
            assert!(child.0.try_wait().unwrap().is_none(), "child exited early");
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            logger.clear().unwrap().wait(),
            Err(DiagnosticError::OtherSessionActive)
        );
        child.0.stdin.take().unwrap().write_all(b"x").unwrap();
        assert!(child.0.wait().unwrap().success());
        logger.clear().unwrap().wait().unwrap();
        assert!(logger.record(event(), None));
        logger.shutdown().unwrap().wait().unwrap();
        assert_eq!(records(&temp.logs).len(), 1);
    }

    #[test]
    fn retention_drops_event_instead_of_deleting_another_active_file() {
        let temp = Temp::new();
        let directory = Arc::new(Directory::ensure(&temp.logs).unwrap());
        let mut first = Writer::new(
            directory.clone(),
            "11111111111111111111111111111111".into(),
            Limits::default(),
        )
        .unwrap();
        first
            .write(Record {
                timestamp: timestamp_ms(),
                action_id: None,
                event: event(),
            })
            .unwrap();
        let original = log_paths(&temp.logs);
        let limits = Limits {
            rotate: 700,
            retain: 100,
            age: RETAIN_AGE,
        };
        assert!(matches!(
            Writer::new(directory, "22222222222222222222222222222222".into(), limits),
            Err(DiagnosticError::StorageBudget)
        ));
        assert!(original.iter().all(|path| path.exists()));
    }

    #[test]
    fn clear_reports_permission_failure_and_logging_recovers_after_permissions_restore() {
        let temp = Temp::new();
        let logger = temp.logger();
        assert!(logger.record(event(), None));
        logger.flush().unwrap().wait().unwrap();
        fs::set_permissions(&temp.logs, Permissions::from_mode(0o500)).unwrap();
        assert_eq!(
            logger.clear().unwrap().wait(),
            Err(DiagnosticError::Io(io::ErrorKind::PermissionDenied))
        );
        fs::set_permissions(&temp.logs, Permissions::from_mode(0o700)).unwrap();
        assert!(logger.record(
            Event::new(Category::Diagnostics, EventKind::Succeeded),
            None
        ));
        logger.shutdown().unwrap().wait().unwrap();
        assert!(
            records(&temp.logs)
                .iter()
                .any(|value| value["category"] == "diagnostics")
        );
    }

    #[test]
    fn flush_reports_write_failure_without_stopping_the_worker() {
        let temp = Temp::new();
        let directory = Arc::new(Directory::ensure(&temp.logs).unwrap());
        let mut writer = Writer::new(
            directory,
            "11111111111111111111111111111111".into(),
            Limits::default(),
        )
        .unwrap();
        // Force open_active to fail on the next queued record, without touching user logs.
        writer.active = None;
        fs::set_permissions(&temp.logs, Permissions::from_mode(0o500)).unwrap();
        let (sender, receiver) = mpsc::sync_channel(QUEUE_CAPACITY);
        let worker = thread::spawn(move || writer.run(receiver));
        sender
            .try_send(Command::Record(Record {
                timestamp: timestamp_ms(),
                action_id: None,
                event: event(),
            }))
            .unwrap();
        let (reply, complete) = mpsc::sync_channel(1);
        sender
            .try_send(Command::Barrier(Operation::Flush, reply))
            .unwrap();
        assert_eq!(
            Completion(complete).wait(),
            Err(DiagnosticError::Io(io::ErrorKind::PermissionDenied))
        );
        fs::set_permissions(&temp.logs, Permissions::from_mode(0o700)).unwrap();
        sender
            .try_send(Command::Record(Record {
                timestamp: timestamp_ms(),
                action_id: None,
                event: event(),
            }))
            .unwrap();
        let (reply, complete) = mpsc::sync_channel(1);
        sender
            .try_send(Command::Barrier(Operation::Shutdown, reply))
            .unwrap();
        Completion(complete).wait().unwrap();
        worker.join().unwrap();
        assert_eq!(records(&temp.logs).len(), 1);
    }

    #[test]
    fn bounded_queue_drops_records_and_rejects_control_when_full() {
        let temp = Temp::new();
        let directory = Arc::new(Directory::ensure(&temp.logs).unwrap());
        let (sender, receiver) = mpsc::sync_channel(2);
        let logger = Diagnostics {
            sender,
            next_action: Arc::new(AtomicU64::new(1)),
            directory,
        };
        assert!(logger.record(event(), None));
        assert!(logger.record(event(), None));
        assert!(!logger.record(event(), None));
        assert!(matches!(logger.clear(), Err(DiagnosticError::QueueFull)));
        drop(receiver);
        assert!(!logger.record(event(), None));
        assert!(matches!(
            logger.flush(),
            Err(DiagnosticError::WorkerStopped)
        ));
    }

    #[test]
    fn log_directory_refuses_a_replaced_path_without_redirecting_writes() {
        let temp = Temp::new();
        let logger = temp.logger();
        assert_eq!(logger.log_directory().unwrap(), temp.logs);
        let moved = temp.root.join("original-logs");
        fs::rename(&temp.logs, &moved).unwrap();
        fs::create_dir(&temp.logs).unwrap();
        fs::set_permissions(&temp.logs, Permissions::from_mode(0o755)).unwrap();
        assert_eq!(logger.log_directory(), Err(DiagnosticError::UnsafePath));
        assert_eq!(fs::metadata(&temp.logs).unwrap().mode() & 0o777, 0o755);
        assert!(logger.record(event(), None));
        logger.shutdown().unwrap().wait().unwrap();
        assert_eq!(records(&moved).len(), 1);
        assert!(fs::read_dir(&temp.logs).unwrap().next().is_none());
    }

    #[test]
    fn recognized_name_requires_exact_version_and_hex_session_sequence() {
        assert!(recognized_name(
            b"wiesel-v1-0123456789abcdef0123456789abcdef-0000000000000001.jsonl"
        ));
        for name in [
            "notes.jsonl",
            "wiesel-v2-0123456789abcdef0123456789abcdef-0000000000000001.jsonl",
            "wiesel-v1-0123456789abcdef0123456789abcdeg-0000000000000001.jsonl",
            "wiesel-v1-0123456789abcdef0123456789abcdef-0000000000000001.jsonl.bak",
        ] {
            assert!(!recognized_name(name.as_bytes()));
        }
    }
}
