//! Bounded asynchronous I/O for the command-line host.
//!
//! The scheduler thread submits plain jobs and never performs a file
//! or stream wait. Fixed workers own all operating-system I/O.

use lm_vm::{
    CompletionKey, CoreCtor, HostCompletion, HostOpenOptions, HostSeekFrom, HostValue, SharedBytes,
    SharedText,
};
use std::collections::HashMap;
use std::io::{BufRead, Read, Seek, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;

const FILE_WORKERS: usize = 4;
const MAX_PENDING_IO: usize = 1_024;

pub(crate) struct IoService {
    completions: Receiver<HostCompletion>,
    files: Vec<Sender<FileJob>>,
    input: Sender<StreamJob>,
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
    Close {
        file: u64,
    },
}

impl FileRequest {
    fn file(&self) -> u64 {
        match self {
            FileRequest::Open { file, .. }
            | FileRequest::Read { file, .. }
            | FileRequest::Write { file, .. }
            | FileRequest::Seek { file, .. }
            | FileRequest::Flush { file }
            | FileRequest::Close { file } => *file,
        }
    }
}

pub(crate) enum StreamRequest {
    Print(SharedText),
    Error(SharedText),
    ReadLine,
}

enum FileJob {
    Request(Job<FileRequest>),
    ForceClose(u64),
}

struct StreamJob(Job<StreamRequest>);

struct Job<T> {
    key: CompletionKey,
    token: u64,
    request: T,
}

impl IoService {
    pub(crate) fn new() -> IoService {
        let (completion_tx, completions) = mpsc::channel();
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
            completions,
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
        let at = request.file() as usize % self.files.len();
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
    ) -> bool {
        if !self.reserve() {
            return false;
        }
        let target = match &request {
            StreamRequest::ReadLine => &self.input,
            StreamRequest::Print(_) | StreamRequest::Error(_) => &self.output,
        };
        let sent = target
            .send(StreamJob(Job {
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

    pub(crate) fn force_close(&self, file: u64) -> bool {
        let at = file as usize % self.files.len();
        self.files[at].send(FileJob::ForceClose(file)).is_ok()
    }

    pub(crate) fn poll(&self) -> Option<HostCompletion> {
        self.completions.try_recv().ok()
    }

    pub(crate) fn wait(&self) -> Option<HostCompletion> {
        self.completions.recv().ok()
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

fn file_worker(
    jobs: Receiver<FileJob>,
    completions: Sender<HostCompletion>,
    pending: Arc<AtomicUsize>,
) {
    let mut files = HashMap::new();
    while let Ok(job) = jobs.recv() {
        match job {
            FileJob::Request(job) => {
                let value = run_file_request(&mut files, job.request);
                let _ = completions.send(HostCompletion {
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
                HostOpenOptions::Append => {
                    open.write(true).create(true).append(true);
                }
            }
            match open.open(path) {
                Ok(opened) => {
                    files.insert(file, opened);
                    fs_ok(HostValue::File(file))
                }
                Err(error) => fs_error(format!("file open failed: {error}")),
            }
        }
        FileRequest::Read { file, count } => {
            let Some(opened) = files.get_mut(&file) else {
                return fs_error("the file token is not open".to_string());
            };
            let mut bytes = vec![0; count];
            match opened.read(&mut bytes) {
                Ok(read) => {
                    bytes.truncate(read);
                    fs_ok(HostValue::Bytes(bytes.into()))
                }
                Err(error) => fs_error(format!("file read failed: {error}")),
            }
        }
        FileRequest::Write { file, bytes } => {
            let Some(opened) = files.get_mut(&file) else {
                return fs_error("the file token is not open".to_string());
            };
            match opened.write(&bytes) {
                Ok(written) => fs_ok(HostValue::Int(written as i64)),
                Err(error) => fs_error(format!("file write failed: {error}")),
            }
        }
        FileRequest::Seek { file, from } => {
            let Some(opened) = files.get_mut(&file) else {
                return fs_error("the file token is not open".to_string());
            };
            let from = match from {
                HostSeekFrom::Start(offset) => match u64::try_from(offset) {
                    Ok(offset) => std::io::SeekFrom::Start(offset),
                    Err(_) => return fs_error("the seek position is invalid".to_string()),
                },
                HostSeekFrom::Current(offset) => std::io::SeekFrom::Current(offset),
                HostSeekFrom::End(offset) => std::io::SeekFrom::End(offset),
            };
            match opened.seek(from) {
                Ok(position) => match i64::try_from(position) {
                    Ok(position) => fs_ok(HostValue::Int(position)),
                    Err(_) => fs_error("the seek position is too large".to_string()),
                },
                Err(error) => fs_error(format!("file seek failed: {error}")),
            }
        }
        FileRequest::Flush { file } => {
            let Some(opened) = files.get_mut(&file) else {
                return fs_error("the file token is not open".to_string());
            };
            match opened.flush() {
                Ok(()) => fs_ok(HostValue::Unit),
                Err(error) => fs_error(format!("file flush failed: {error}")),
            }
        }
        FileRequest::Close { file } => {
            if files.remove(&file).is_some() {
                fs_ok(HostValue::Unit)
            } else {
                fs_error("the file token is not open".to_string())
            }
        }
    }
}

fn input_worker(
    jobs: Receiver<StreamJob>,
    completions: Sender<HostCompletion>,
    pending: Arc<AtomicUsize>,
) {
    while let Ok(StreamJob(job)) = jobs.recv() {
        let value = match job.request {
            StreamRequest::ReadLine => read_line(),
            StreamRequest::Print(_) | StreamRequest::Error(_) => {
                HostValue::Ctor(CoreCtor::Err, vec![])
            }
        };
        let _ = completions.send(HostCompletion {
            key: job.key,
            token: job.token,
            result: Ok(value),
        });
        release_pending(&pending);
    }
}

fn output_worker(
    jobs: Receiver<StreamJob>,
    completions: Sender<HostCompletion>,
    pending: Arc<AtomicUsize>,
) {
    while let Ok(StreamJob(job)) = jobs.recv() {
        let result = match job.request {
            StreamRequest::Print(text) => {
                let mut out = std::io::stdout();
                out.write_all(text.as_bytes()).and_then(|_| out.flush())
            }
            StreamRequest::Error(text) => {
                let mut out = std::io::stderr();
                out.write_all(text.as_bytes()).and_then(|_| out.flush())
            }
            StreamRequest::ReadLine => unreachable!("input uses its own worker"),
        };
        let result = match result {
            Ok(()) => Ok(HostValue::Unit),
            Err(error) => Err(format!("stream write failed: {error}")),
        };
        let _ = completions.send(HostCompletion {
            key: job.key,
            token: job.token,
            result,
        });
        release_pending(&pending);
    }
}

fn read_line() -> HostValue {
    let mut line = String::new();
    match std::io::stdin().lock().read_line(&mut line) {
        Ok(0) => HostValue::Ctor(CoreCtor::Ok, vec![HostValue::Ctor(CoreCtor::None, vec![])]),
        Ok(_) => {
            if line.ends_with('\n') {
                line.pop();
                if line.ends_with('\r') {
                    line.pop();
                }
            }
            HostValue::Ctor(
                CoreCtor::Ok,
                vec![HostValue::Ctor(
                    CoreCtor::Some,
                    vec![HostValue::Str(line.into())],
                )],
            )
        }
        Err(error) => io_error(format!("stdin read failed: {error}")),
    }
}

fn fs_ok(value: HostValue) -> HostValue {
    HostValue::Ctor(CoreCtor::Ok, vec![value])
}

fn fs_error(message: String) -> HostValue {
    HostValue::Ctor(
        CoreCtor::Err,
        vec![HostValue::Ctor(
            CoreCtor::FsErrorFailed,
            vec![HostValue::Str(message.into())],
        )],
    )
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
