//! Bounded asynchronous I/O for the command-line host.
//!
//! The scheduler thread submits plain jobs and never performs a file
//! or stream wait. Fixed workers own all operating-system I/O.

use crate::ReadySender;
use lm_vm::{
    CompletionKey, CoreCtor, HostCompletion, HostOpenOptions, HostRenameMode, HostSeekFrom,
    HostValue, HostWaitCancel, SharedBytes,
};
use std::collections::{HashMap, VecDeque};
use std::io::{Read, Seek, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender, TryRecvError};
use std::sync::Arc;
use std::time::Duration;

const FILE_WORKERS: usize = 4;
const MAX_PENDING_IO: usize = 1_024;
const MAX_INPUT_BUFFER: usize = 1 << 20;
const INPUT_CHUNKS: usize = 16;
const INPUT_CHUNK_BYTES: usize = 8 << 10;

pub(crate) struct IoService {
    files: Vec<Sender<FileJob>>,
    input: Sender<InputCommand>,
    output: Sender<StreamJob>,
    pending: Arc<AtomicUsize>,
}

pub(crate) enum FileRequest {
    Open {
        file: u64,
        path: String,
        options: HostOpenOptions,
    },
    Read {
        file: u64,
        count: usize,
    },
    Write {
        file: u64,
        bytes: SharedBytes,
    },
    Seek {
        file: u64,
        from: HostSeekFrom,
    },
    Flush {
        file: u64,
    },
    Sync {
        file: u64,
    },
    Close {
        file: u64,
    },
    Stat {
        path: String,
    },
    ReadDir {
        path: String,
        max_entries: usize,
    },
    CreateDir {
        path: String,
    },
    RemoveFile {
        path: String,
    },
    RemoveDir {
        path: String,
    },
    Rename {
        from: String,
        to: String,
        mode: HostRenameMode,
    },
    SyncDir {
        path: String,
    },
}

impl FileRequest {
    fn lane(&self) -> u64 {
        match self {
            FileRequest::Open { file, .. }
            | FileRequest::Read { file, .. }
            | FileRequest::Write { file, .. }
            | FileRequest::Seek { file, .. }
            | FileRequest::Flush { file }
            | FileRequest::Sync { file }
            | FileRequest::Close { file } => *file,
            FileRequest::Stat { .. }
            | FileRequest::ReadDir { .. }
            | FileRequest::CreateDir { .. }
            | FileRequest::RemoveFile { .. }
            | FileRequest::RemoveDir { .. }
            | FileRequest::Rename { .. }
            | FileRequest::SyncDir { .. } => 0,
        }
    }
}

pub(crate) enum StreamRequest {
    ReadBytes(usize),
    Write(SharedBytes),
    WriteError(SharedBytes),
}

enum FileJob {
    Request(Job<FileRequest>),
    ForceClose(u64),
}

struct StreamJob(Job<StreamRequest>);

enum InputCommand {
    Request {
        job: Job<StreamRequest>,
        wait_source: bool,
    },
    Commit {
        token: u64,
        reply: SyncSender<bool>,
    },
    Cancel {
        token: u64,
        reply: SyncSender<HostWaitCancel>,
    },
}

enum InputData {
    Bytes(Vec<u8>),
    Eof,
    Failed(String),
}

struct InputPending {
    job: Job<StreamRequest>,
    wait_source: bool,
}

struct Job<T> {
    key: CompletionKey,
    token: u64,
    request: T,
}

impl IoService {
    pub(crate) fn new(completion_tx: ReadySender) -> IoService {
        let pending = Arc::new(AtomicUsize::new(0));

        let mut files = Vec::with_capacity(FILE_WORKERS);
        for worker in 0..FILE_WORKERS {
            let (tx, rx) = mpsc::channel();
            files.push(tx);
            let completions = completion_tx.clone();
            let pending = Arc::clone(&pending);
            std::thread::Builder::new()
                .name(format!("loom-file-{worker}"))
                .spawn(move || file_worker(rx, completions, pending))
                .expect("the command-line file worker starts");
        }

        let (input, input_rx) = mpsc::channel();
        {
            let completions = completion_tx.clone();
            let pending = Arc::clone(&pending);
            std::thread::Builder::new()
                .name("loom-input".to_string())
                .spawn(move || input_worker(input_rx, completions, pending))
                .expect("the command-line input worker starts");
        }

        let (output, output_rx) = mpsc::channel();
        {
            let pending = Arc::clone(&pending);
            std::thread::Builder::new()
                .name("loom-output".to_string())
                .spawn(move || output_worker(output_rx, completion_tx, pending))
                .expect("the command-line output worker starts");
        }

        IoService {
            files,
            input,
            output,
            pending,
        }
    }

    pub(crate) fn submit_file(&self, key: CompletionKey, token: u64, request: FileRequest) -> bool {
        if !self.reserve() {
            return false;
        }
        let at = request.lane() as usize % self.files.len();
        let sent = self.files[at]
            .send(FileJob::Request(Job {
                key,
                token,
                request,
            }))
            .is_ok();
        if !sent {
            self.release();
        }
        sent
    }

    pub(crate) fn submit_stream(
        &self,
        key: CompletionKey,
        token: u64,
        request: StreamRequest,
        wait_source: bool,
    ) -> bool {
        if !self.reserve() {
            return false;
        }
        let job = Job {
            key,
            token,
            request,
        };
        let sent = match &job.request {
            StreamRequest::ReadBytes(_) => self
                .input
                .send(InputCommand::Request { job, wait_source })
                .is_ok(),
            StreamRequest::Write(_) | StreamRequest::WriteError(_) => {
                self.output.send(StreamJob(job)).is_ok()
            }
        };
        if !sent {
            self.release();
        }
        sent
    }

    pub(crate) fn commit_wait(&self, token: u64) -> bool {
        let (reply, answer) = mpsc::sync_channel(1);
        if self
            .input
            .send(InputCommand::Commit { token, reply })
            .is_err()
        {
            return false;
        }
        answer.recv_timeout(Duration::from_secs(1)).unwrap_or(false)
    }

    pub(crate) fn cancel_wait(&self, token: u64) -> HostWaitCancel {
        let (reply, answer) = mpsc::sync_channel(1);
        if self
            .input
            .send(InputCommand::Cancel { token, reply })
            .is_err()
        {
            return HostWaitCancel::Missing;
        }
        answer
            .recv_timeout(Duration::from_secs(1))
            .unwrap_or(HostWaitCancel::Missing)
    }

    pub(crate) fn force_close(&self, file: u64) -> bool {
        let at = file as usize % self.files.len();
        self.files[at].send(FileJob::ForceClose(file)).is_ok()
    }

    fn reserve(&self) -> bool {
        self.pending
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |pending| {
                (pending < MAX_PENDING_IO).then_some(pending + 1)
            })
            .is_ok()
    }

    fn release(&self) {
        let previous = self.pending.fetch_sub(1, Ordering::Relaxed);
        debug_assert!(previous > 0);
    }
}

fn file_worker(jobs: Receiver<FileJob>, completions: ReadySender, pending: Arc<AtomicUsize>) {
    let mut files = HashMap::new();
    while let Ok(job) = jobs.recv() {
        match job {
            FileJob::Request(job) => {
                let value = run_file_request(&mut files, job.request);
                let _ = completions.completion(HostCompletion {
                    key: job.key,
                    token: job.token,
                    result: Ok(value),
                });
                release_pending(&pending);
            }
            FileJob::ForceClose(file) => {
                files.remove(&file);
            }
        }
    }
}

fn run_file_request(files: &mut HashMap<u64, std::fs::File>, request: FileRequest) -> HostValue {
    match request {
        FileRequest::Open {
            file,
            path,
            options,
        } => {
            let mut open = std::fs::OpenOptions::new();
            match options {
                HostOpenOptions::ReadOnly => {
                    open.read(true);
                }
                HostOpenOptions::WriteOnly => {
                    open.write(true);
                }
                HostOpenOptions::ReadWrite => {
                    open.read(true).write(true);
                }
                HostOpenOptions::Create => {
                    open.read(true).write(true).create(true);
                }
                HostOpenOptions::CreateTruncate => {
                    open.read(true).write(true).create(true).truncate(true);
                }
                HostOpenOptions::CreateNew => {
                    open.read(true).write(true).create_new(true);
                }
                HostOpenOptions::Append => {
                    open.write(true).create(true).append(true);
                }
            }
            match open.open(path) {
                Ok(opened) => {
                    files.insert(file, opened);
                    fs_ok(HostValue::File(file))
                }
                Err(error) => fs_io_error(error, "file open"),
            }
        }
        FileRequest::Read { file, count } => {
            let Some(opened) = files.get_mut(&file) else {
                return fs_closed();
            };
            let mut bytes = vec![0; count];
            match opened.read(&mut bytes) {
                Ok(read) => {
                    bytes.truncate(read);
                    fs_ok(HostValue::Bytes(bytes.into()))
                }
                Err(error) => fs_io_error(error, "file read"),
            }
        }
        FileRequest::Write { file, bytes } => {
            let Some(opened) = files.get_mut(&file) else {
                return fs_closed();
            };
            match opened.write(&bytes) {
                Ok(written) => fs_ok(HostValue::Int(written as i64)),
                Err(error) => fs_io_error(error, "file write"),
            }
        }
        FileRequest::Seek { file, from } => {
            let Some(opened) = files.get_mut(&file) else {
                return fs_closed();
            };
            let from = match from {
                HostSeekFrom::Start(offset) => match u64::try_from(offset) {
                    Ok(offset) => std::io::SeekFrom::Start(offset),
                    Err(_) => {
                        return fs_error_with(
                            CoreCtor::FsErrorInvalidInput,
                            "the seek position is invalid",
                        )
                    }
                },
                HostSeekFrom::Current(offset) => std::io::SeekFrom::Current(offset),
                HostSeekFrom::End(offset) => std::io::SeekFrom::End(offset),
            };
            match opened.seek(from) {
                Ok(position) => match i64::try_from(position) {
                    Ok(position) => fs_ok(HostValue::Int(position)),
                    Err(_) => fs_error_with(
                        CoreCtor::FsErrorLimitExceeded,
                        "the seek position is too large",
                    ),
                },
                Err(error) => fs_io_error(error, "file seek"),
            }
        }
        FileRequest::Flush { file } => {
            let Some(opened) = files.get_mut(&file) else {
                return fs_closed();
            };
            match opened.flush() {
                Ok(()) => fs_ok(HostValue::Unit),
                Err(error) => fs_io_error(error, "file flush"),
            }
        }
        FileRequest::Sync { file } => {
            let Some(opened) = files.get_mut(&file) else {
                return fs_closed();
            };
            match opened.sync_all() {
                Ok(()) => fs_ok(HostValue::Unit),
                Err(error) => fs_io_error(error, "file sync"),
            }
        }
        FileRequest::Close { file } => {
            if files.remove(&file).is_some() {
                fs_ok(HostValue::Unit)
            } else {
                fs_closed()
            }
        }
        FileRequest::Stat { path } => match std::fs::metadata(path) {
            Ok(metadata) => file_info_value(&metadata),
            Err(error) => fs_io_error(error, "file metadata query"),
        },
        FileRequest::ReadDir { path, max_entries } => read_dir_value(&path, max_entries),
        FileRequest::CreateDir { path } => match std::fs::create_dir(path) {
            Ok(()) => fs_ok(HostValue::Unit),
            Err(error) => fs_io_error(error, "directory creation"),
        },
        FileRequest::RemoveFile { path } => match std::fs::remove_file(path) {
            Ok(()) => fs_ok(HostValue::Unit),
            Err(error) => fs_io_error(error, "file removal"),
        },
        FileRequest::RemoveDir { path } => match std::fs::remove_dir(path) {
            Ok(()) => fs_ok(HostValue::Unit),
            Err(error) => fs_io_error(error, "directory removal"),
        },
        FileRequest::Rename { from, to, mode } => rename_value(&from, &to, mode),
        FileRequest::SyncDir { path } => match std::fs::File::open(path) {
            Ok(directory) => match directory.sync_all() {
                Ok(()) => fs_ok(HostValue::Unit),
                Err(error) => fs_io_error(error, "directory sync"),
            },
            Err(error) => fs_io_error(error, "directory open"),
        },
    }
}

fn input_worker(
    commands: Receiver<InputCommand>,
    completions: ReadySender,
    pending_count: Arc<AtomicUsize>,
) {
    let (data_tx, data_rx) = mpsc::sync_channel(INPUT_CHUNKS);
    let mut reader_started = false;
    let mut pending = VecDeque::<InputPending>::new();
    let mut retained = HashMap::<u64, Vec<u8>>::new();
    let mut buffer = VecDeque::<u8>::new();
    let mut eof = false;
    let mut failure = None;
    let mut disconnected = false;

    loop {
        let mut moved = false;
        loop {
            match commands.try_recv() {
                Ok(command) => {
                    moved = true;
                    handle_input_command(
                        command,
                        &mut pending,
                        &mut retained,
                        &mut buffer,
                        &pending_count,
                    );
                    if !reader_started && !pending.is_empty() {
                        start_input_reader(data_tx.clone());
                        reader_started = true;
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }
        while buffer.len() <= MAX_INPUT_BUFFER.saturating_sub(INPUT_CHUNK_BYTES) {
            match data_rx.try_recv() {
                Ok(InputData::Bytes(bytes)) => {
                    moved = true;
                    buffer.extend(bytes);
                }
                Ok(InputData::Eof) => {
                    moved = true;
                    eof = true;
                }
                Ok(InputData::Failed(message)) => {
                    moved = true;
                    failure = Some(message);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    eof = true;
                    break;
                }
            }
        }
        while let Some(front) = pending.front() {
            let Some((value, consumed)) =
                prepare_input_reply(&front.job.request, &mut buffer, eof, failure.as_deref())
            else {
                break;
            };
            let front = pending.pop_front().expect("the input request exists");
            if front.wait_source {
                retained.insert(front.job.token, consumed);
            }
            let _ = completions.completion(HostCompletion {
                key: front.job.key,
                token: front.job.token,
                result: Ok(value),
            });
            release_pending(&pending_count);
            moved = true;
        }
        if disconnected && pending.is_empty() {
            return;
        }
        if moved {
            continue;
        }
        match commands.recv_timeout(Duration::from_millis(2)) {
            Ok(command) => {
                handle_input_command(
                    command,
                    &mut pending,
                    &mut retained,
                    &mut buffer,
                    &pending_count,
                );
                if !reader_started && !pending.is_empty() {
                    start_input_reader(data_tx.clone());
                    reader_started = true;
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => disconnected = true,
        }
    }
}

fn handle_input_command(
    command: InputCommand,
    pending: &mut VecDeque<InputPending>,
    retained: &mut HashMap<u64, Vec<u8>>,
    buffer: &mut VecDeque<u8>,
    pending_count: &AtomicUsize,
) {
    match command {
        InputCommand::Request { job, wait_source } => {
            pending.push_back(InputPending { job, wait_source });
        }
        InputCommand::Commit { token, reply } => {
            let found = retained.remove(&token).is_some();
            let _ = reply.send(found);
        }
        InputCommand::Cancel { token, reply } => {
            if let Some(at) = pending.iter().position(|entry| entry.job.token == token) {
                pending.remove(at);
                release_pending(pending_count);
                let _ = reply.send(HostWaitCancel::Cancelled);
            } else if let Some(bytes) = retained.remove(&token) {
                for byte in bytes.into_iter().rev() {
                    buffer.push_front(byte);
                }
                let _ = reply.send(HostWaitCancel::ReadyRestored);
            } else {
                let _ = reply.send(HostWaitCancel::Missing);
            }
        }
    }
}

fn start_input_reader(data: SyncSender<InputData>) {
    std::thread::Builder::new()
        .name("loom-stdin-reader".to_string())
        .spawn(move || {
            let input = std::io::stdin();
            let mut input = input.lock();
            loop {
                let mut bytes = vec![0; INPUT_CHUNK_BYTES];
                match input.read(&mut bytes) {
                    Ok(0) => {
                        let _ = data.send(InputData::Eof);
                        return;
                    }
                    Ok(read) => {
                        bytes.truncate(read);
                        if data.send(InputData::Bytes(bytes)).is_err() {
                            return;
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(error) => {
                        let _ = data.send(InputData::Failed(format!("stdin read failed: {error}")));
                        return;
                    }
                }
            }
        })
        .expect("the standard input reader starts");
}

fn prepare_input_reply(
    request: &StreamRequest,
    buffer: &mut VecDeque<u8>,
    eof: bool,
    failure: Option<&str>,
) -> Option<(HostValue, Vec<u8>)> {
    if let Some(message) = failure {
        return Some((io_error(message.to_string()), Vec::new()));
    }
    match request {
        StreamRequest::ReadBytes(count) => {
            if *count > 0 && buffer.is_empty() && !eof {
                return None;
            }
            let take = (*count).min(buffer.len());
            let consumed: Vec<u8> = buffer.drain(..take).collect();
            Some((io_ok(HostValue::Bytes(consumed.clone().into())), consumed))
        }
        StreamRequest::Write(_) | StreamRequest::WriteError(_) => Some((
            io_error("the input service received an output request".to_string()),
            Vec::new(),
        )),
    }
}

fn output_worker(jobs: Receiver<StreamJob>, completions: ReadySender, pending: Arc<AtomicUsize>) {
    while let Ok(StreamJob(job)) = jobs.recv() {
        let result = match job.request {
            StreamRequest::Write(bytes) => Ok(write_bytes(std::io::stdout(), &bytes)),
            StreamRequest::WriteError(bytes) => Ok(write_bytes(std::io::stderr(), &bytes)),
            StreamRequest::ReadBytes(_) => {
                unreachable!("input uses its own worker")
            }
        };
        let _ = completions.completion(HostCompletion {
            key: job.key,
            token: job.token,
            result,
        });
        release_pending(&pending);
    }
}

fn write_bytes(mut output: impl Write, bytes: &[u8]) -> HostValue {
    let written = loop {
        match output.write(bytes) {
            Ok(written) => break written,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => return broken_pipe(),
            Err(error) => return io_error(format!("stream write failed: {error}")),
        }
    };
    loop {
        match output.flush() {
            Ok(()) => return io_ok(HostValue::Int(written as i64)),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => return broken_pipe(),
            Err(error) => return io_error(format!("stream flush failed: {error}")),
        }
    }
}

fn broken_pipe() -> HostValue {
    HostValue::Ctor(
        CoreCtor::Err,
        vec![HostValue::Ctor(CoreCtor::IoErrorBrokenPipe, vec![])],
    )
}

fn file_kind_value(kind: std::fs::FileType) -> HostValue {
    let ctor = if kind.is_file() {
        CoreCtor::FileKindFile
    } else if kind.is_dir() {
        CoreCtor::FileKindDirectory
    } else if kind.is_symlink() {
        CoreCtor::FileKindSymlink
    } else {
        CoreCtor::FileKindOther
    };
    HostValue::Ctor(ctor, Vec::new())
}

fn file_info_value(metadata: &std::fs::Metadata) -> HostValue {
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|duration| i64::try_from(duration.as_nanos()).ok())
        .map(|value| HostValue::Ctor(CoreCtor::Some, vec![HostValue::Int(value)]))
        .unwrap_or_else(|| HostValue::Ctor(CoreCtor::None, Vec::new()));
    let Ok(length) = i64::try_from(metadata.len()) else {
        return fs_error_with(
            CoreCtor::FsErrorLimitExceeded,
            "the file length exceeds Int",
        );
    };
    fs_ok(HostValue::Ctor(
        CoreCtor::FileInfo,
        vec![
            file_kind_value(metadata.file_type()),
            HostValue::Int(length),
            modified,
            HostValue::Bool(metadata.permissions().readonly()),
        ],
    ))
}

fn read_dir_value(path: &str, max_entries: usize) -> HostValue {
    let entries = match std::fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) => return fs_io_error(error, "directory read"),
    };
    let mut values = Vec::new();
    for entry in entries {
        if values.len() == max_entries {
            return fs_error_with(
                CoreCtor::FsErrorLimitExceeded,
                "the directory exceeds its entry limit",
            );
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => return fs_io_error(error, "directory continuation"),
        };
        let name = match entry.file_name().into_string() {
            Ok(name) => name,
            Err(_) => {
                values.push(HostValue::Ctor(
                    CoreCtor::Err,
                    vec![fs_error_case(
                        CoreCtor::FsErrorInvalidEncoding,
                        "a directory name is not valid UTF-8",
                    )],
                ));
                continue;
            }
        };
        let kind = match entry.file_type() {
            Ok(kind) => kind,
            Err(error) => {
                values.push(HostValue::Ctor(
                    CoreCtor::Err,
                    vec![fs_io_error_case(error, "directory entry query")],
                ));
                continue;
            }
        };
        values.push(HostValue::Ctor(
            CoreCtor::Ok,
            vec![HostValue::Ctor(
                CoreCtor::DirEntry,
                vec![HostValue::Str(name.into()), file_kind_value(kind)],
            )],
        ));
    }
    fs_ok(HostValue::List(values))
}

fn rename_value(from: &str, to: &str, mode: HostRenameMode) -> HostValue {
    let source = match std::fs::symlink_metadata(from) {
        Ok(source) => source,
        Err(error) => return fs_io_error(error, "rename source query"),
    };
    let no_replace = source.is_dir() || matches!(mode, HostRenameMode::NoReplace);
    if !no_replace {
        match std::fs::symlink_metadata(to) {
            Ok(target) if target.is_dir() => {
                return fs_error_with(
                    CoreCtor::FsErrorIsDirectory,
                    "rename cannot replace a directory",
                );
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return fs_io_error(error, "rename target query"),
        }
    }
    let renamed = if no_replace {
        rename_no_replace(from, to)
    } else {
        std::fs::rename(from, to)
    };
    match renamed {
        Ok(()) => fs_ok(HostValue::Unit),
        Err(error) => fs_io_error(error, "rename"),
    }
}

#[cfg(target_os = "linux")]
fn rename_no_replace(from: &str, to: &str) -> std::io::Result<()> {
    use std::ffi::CString;
    let from = CString::new(from)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid path"))?;
    let to = CString::new(to)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid path"))?;
    // The kernel performs one atomic no-replace rename.
    let result = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(target_os = "linux"))]
fn rename_no_replace(_from: &str, _to: &str) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "atomic no-replace rename is unavailable",
    ))
}

fn fs_ok(value: HostValue) -> HostValue {
    HostValue::Ctor(CoreCtor::Ok, vec![value])
}

fn io_ok(value: HostValue) -> HostValue {
    HostValue::Ctor(CoreCtor::Ok, vec![value])
}

fn fs_closed() -> HostValue {
    HostValue::Ctor(
        CoreCtor::Err,
        vec![HostValue::Ctor(CoreCtor::FsErrorClosed, Vec::new())],
    )
}

fn fs_error_with(ctor: CoreCtor, message: impl Into<String>) -> HostValue {
    HostValue::Ctor(CoreCtor::Err, vec![fs_error_case(ctor, message)])
}

fn fs_error_case(ctor: CoreCtor, message: impl Into<String>) -> HostValue {
    HostValue::Ctor(ctor, vec![HostValue::Str(message.into().into())])
}

fn fs_io_error(error: std::io::Error, action: &str) -> HostValue {
    HostValue::Ctor(CoreCtor::Err, vec![fs_io_error_case(error, action)])
}

fn fs_io_error_case(error: std::io::Error, action: &str) -> HostValue {
    let cross_device = error.raw_os_error() == Some(libc::EXDEV);
    let (ctor, detail) = if cross_device {
        (CoreCtor::FsErrorCrossDevice, "crosses file systems")
    } else {
        match error.kind() {
            std::io::ErrorKind::NotFound => (CoreCtor::FsErrorNotFound, "did not find the path"),
            std::io::ErrorKind::PermissionDenied => {
                (CoreCtor::FsErrorPermissionDenied, "lacks permission")
            }
            std::io::ErrorKind::AlreadyExists => {
                (CoreCtor::FsErrorAlreadyExists, "found an existing target")
            }
            std::io::ErrorKind::InvalidInput => {
                (CoreCtor::FsErrorInvalidInput, "received invalid input")
            }
            std::io::ErrorKind::InvalidData => (
                CoreCtor::FsErrorInvalidEncoding,
                "received invalid encoding",
            ),
            std::io::ErrorKind::NotADirectory => {
                (CoreCtor::FsErrorNotDirectory, "needs a directory")
            }
            std::io::ErrorKind::IsADirectory => (CoreCtor::FsErrorIsDirectory, "found a directory"),
            std::io::ErrorKind::DirectoryNotEmpty => (
                CoreCtor::FsErrorDirectoryNotEmpty,
                "found a nonempty directory",
            ),
            std::io::ErrorKind::Unsupported => (CoreCtor::FsErrorUnsupported, "is not supported"),
            _ => (CoreCtor::FsErrorFailed, "failed"),
        }
    };
    fs_error_case(ctor, format!("{action} {detail}"))
}

fn io_error(message: String) -> HostValue {
    HostValue::Ctor(
        CoreCtor::Err,
        vec![HostValue::Ctor(
            CoreCtor::IoErrorFailed,
            vec![HostValue::Str(message.into())],
        )],
    )
}

fn release_pending(pending: &AtomicUsize) {
    let previous = pending.fetch_sub(1, Ordering::Relaxed);
    debug_assert!(previous > 0);
}
