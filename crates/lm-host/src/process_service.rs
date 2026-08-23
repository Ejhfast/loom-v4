//! Bounded operating-system pipes and child programs.
//!
//! One service thread owns every pipe end and child handle.
//! The scheduler submits plain requests and never blocks on them.

use lm_vm::{
    CompletionKey, CoreCtor, HostChildEnv, HostChildInput, HostChildOutput, HostCompletion,
    HostExecSpec, HostValue, HostWaitCancel, SharedBytes,
};
use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender};
use std::sync::Arc;
use std::time::Duration;

const MAX_PENDING_PROCESS: usize = 1_024;
const PIPE_WRITE_CHUNK: usize = 4 << 10;
const SERVICE_TICK: Duration = Duration::from_millis(2);

pub(crate) struct ProcessService {
    commands: Sender<ServiceCommand>,
    completions: Receiver<HostCompletion>,
    pending: Arc<AtomicUsize>,
}

pub(crate) enum ProcessRequest {
    OpenPipe { reader: u64, writer: u64 },
    ReadPipe { reader: u64, count: usize },
    WritePipe { writer: u64, bytes: SharedBytes },
    ClosePipe { token: u64 },
    Spawn { child: u64, spec: HostExecSpec },
    WaitChild { child: u64 },
    TerminateChild { child: u64 },
    KillChild { child: u64 },
    CloseChild { child: u64 },
}

struct Job {
    key: CompletionKey,
    token: u64,
    request: ProcessRequest,
    wait_source: bool,
}

enum ServiceCommand {
    Request(Box<Job>),
    Cancel {
        token: u64,
        wait_source: bool,
        reply: SyncSender<CancelReply>,
    },
    Commit {
        token: u64,
        reply: SyncSender<bool>,
    },
    ClosePipe {
        token: u64,
        reply: SyncSender<bool>,
    },
    CloseChild {
        token: u64,
        reply: SyncSender<bool>,
    },
}

enum CancelReply {
    Direct(bool),
    Wait(HostWaitCancel),
}

#[derive(Clone, Copy)]
enum PipeKind {
    Reader,
    Writer,
}

struct PipeEnd {
    file: std::fs::File,
    kind: PipeKind,
    restored: VecDeque<u8>,
}

struct ChildState {
    child: Child,
    status: Option<ChildStatus>,
}

#[derive(Clone, Copy)]
enum ChildStatus {
    Exited(i64),
    Terminated,
}

enum ChildPipeError {
    Closed,
    WrongDirection,
    Io(std::io::Error),
}

enum Retained {
    PipeRead { reader: u64, bytes: Vec<u8> },
    ChildWait { child: u64 },
}

struct ServiceState {
    pipes: HashMap<u64, PipeEnd>,
    children: HashMap<u64, ChildState>,
    detached: Vec<Child>,
    pending: HashMap<u64, Job>,
    retained: HashMap<u64, Retained>,
}

enum Progress {
    Ready {
        key: CompletionKey,
        token: u64,
        value: HostValue,
        retained: Option<Retained>,
    },
    Park(Job),
}

impl ProcessService {
    pub(crate) fn new() -> ProcessService {
        let (commands, command_rx) = mpsc::channel();
        let (completion_tx, completions) = mpsc::channel();
        let pending = Arc::new(AtomicUsize::new(0));
        let worker_pending = Arc::clone(&pending);
        std::thread::Builder::new()
            .name("loom-process".to_string())
            .spawn(move || process_worker(command_rx, completion_tx, worker_pending))
            .expect("the process service starts");
        ProcessService {
            commands,
            completions,
            pending,
        }
    }

    pub(crate) fn submit(
        &self,
        key: CompletionKey,
        token: u64,
        request: ProcessRequest,
        wait_source: bool,
    ) -> bool {
        if !self.reserve() {
            return false;
        }
        let sent = self
            .commands
            .send(ServiceCommand::Request(Box::new(Job {
                key,
                token,
                request,
                wait_source,
            })))
            .is_ok();
        if !sent {
            self.release();
        }
        sent
    }

    pub(crate) fn poll(&self) -> Option<HostCompletion> {
        self.completions.try_recv().ok()
    }

    pub(crate) fn wait_timeout(
        &self,
        duration: Duration,
    ) -> Result<HostCompletion, RecvTimeoutError> {
        self.completions.recv_timeout(duration)
    }

    pub(crate) fn cancel(&self, token: u64) -> bool {
        matches!(
            self.cancel_inner(token, false),
            Some(CancelReply::Direct(true))
        )
    }

    pub(crate) fn cancel_wait(&self, token: u64) -> HostWaitCancel {
        match self.cancel_inner(token, true) {
            Some(CancelReply::Wait(result)) => result,
            _ => HostWaitCancel::Missing,
        }
    }

    pub(crate) fn commit_wait(&self, token: u64) -> bool {
        let (reply, answer) = mpsc::sync_channel(1);
        if self
            .commands
            .send(ServiceCommand::Commit { token, reply })
            .is_err()
        {
            return false;
        }
        answer.recv_timeout(Duration::from_secs(1)).unwrap_or(false)
    }

    pub(crate) fn force_close_pipe(&self, token: u64) -> bool {
        let (reply, answer) = mpsc::sync_channel(1);
        if self
            .commands
            .send(ServiceCommand::ClosePipe { token, reply })
            .is_err()
        {
            return false;
        }
        answer.recv_timeout(Duration::from_secs(1)).unwrap_or(false)
    }

    pub(crate) fn force_close_child(&self, token: u64) -> bool {
        let (reply, answer) = mpsc::sync_channel(1);
        if self
            .commands
            .send(ServiceCommand::CloseChild { token, reply })
            .is_err()
        {
            return false;
        }
        answer.recv_timeout(Duration::from_secs(1)).unwrap_or(false)
    }

    fn cancel_inner(&self, token: u64, wait_source: bool) -> Option<CancelReply> {
        let (reply, answer) = mpsc::sync_channel(1);
        self.commands
            .send(ServiceCommand::Cancel {
                token,
                wait_source,
                reply,
            })
            .ok()?;
        answer.recv_timeout(Duration::from_secs(1)).ok()
    }

    fn reserve(&self) -> bool {
        self.pending
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |pending| {
                (pending < MAX_PENDING_PROCESS).then_some(pending + 1)
            })
            .is_ok()
    }

    fn release(&self) {
        release_pending(&self.pending);
    }
}

fn process_worker(
    commands: Receiver<ServiceCommand>,
    completions: Sender<HostCompletion>,
    pending_count: Arc<AtomicUsize>,
) {
    let mut state = ServiceState {
        pipes: HashMap::new(),
        children: HashMap::new(),
        detached: Vec::new(),
        pending: HashMap::new(),
        retained: HashMap::new(),
    };
    let mut connected = true;
    while connected {
        match commands.recv_timeout(SERVICE_TICK) {
            Ok(command) => handle_command(command, &mut state, &completions, &pending_count),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => connected = false,
        }
        while let Ok(command) = commands.try_recv() {
            handle_command(command, &mut state, &completions, &pending_count);
        }
        progress_pending(&mut state, &completions, &pending_count);
        reap_detached(&mut state.detached);
    }
}

fn handle_command(
    command: ServiceCommand,
    state: &mut ServiceState,
    completions: &Sender<HostCompletion>,
    pending_count: &AtomicUsize,
) {
    match command {
        ServiceCommand::Request(job) => match progress_job(*job, state) {
            Progress::Ready {
                key,
                token,
                value,
                retained,
            } => {
                if let Some(retained) = retained {
                    state.retained.insert(token, retained);
                }
                send_completion(completions, pending_count, key, token, value);
            }
            Progress::Park(job) => {
                state.pending.insert(job.token, job);
            }
        },
        ServiceCommand::Cancel {
            token,
            wait_source,
            reply,
        } => {
            let result = cancel_request(state, token, wait_source, pending_count);
            let _ = reply.send(result);
        }
        ServiceCommand::Commit { token, reply } => {
            let committed = match state.retained.remove(&token) {
                Some(Retained::ChildWait { child }) => {
                    state.children.remove(&child);
                    true
                }
                Some(Retained::PipeRead { .. }) => true,
                None => false,
            };
            let _ = reply.send(committed);
        }
        ServiceCommand::ClosePipe { token, reply } => {
            let _ = reply.send(state.pipes.remove(&token).is_some());
        }
        ServiceCommand::CloseChild { token, reply } => {
            let closed = detach_child(state, token);
            let _ = reply.send(closed);
        }
    }
}

fn send_completion(
    completions: &Sender<HostCompletion>,
    pending_count: &AtomicUsize,
    key: CompletionKey,
    token: u64,
    value: HostValue,
) {
    let _ = completions.send(HostCompletion {
        key,
        token,
        result: Ok(value),
    });
    release_pending(pending_count);
}

fn progress_pending(
    state: &mut ServiceState,
    completions: &Sender<HostCompletion>,
    pending_count: &AtomicUsize,
) {
    let tokens: Vec<u64> = state.pending.keys().copied().collect();
    for token in tokens {
        let Some(job) = state.pending.remove(&token) else {
            continue;
        };
        match progress_job(job, state) {
            Progress::Ready {
                key,
                token,
                value,
                retained,
            } => {
                if let Some(retained) = retained {
                    state.retained.insert(token, retained);
                }
                send_completion(completions, pending_count, key, token, value);
            }
            Progress::Park(job) => {
                state.pending.insert(token, job);
            }
        }
    }
}

fn progress_job(job: Job, state: &mut ServiceState) -> Progress {
    let key = job.key;
    let token = job.token;
    let wait_source = job.wait_source;
    match run_request(&job.request, state, wait_source) {
        Some((value, retained)) => Progress::Ready {
            key,
            token,
            value,
            retained,
        },
        None => Progress::Park(job),
    }
}

fn run_request(
    request: &ProcessRequest,
    state: &mut ServiceState,
    wait_source: bool,
) -> Option<(HostValue, Option<Retained>)> {
    match request {
        ProcessRequest::OpenPipe { reader, writer } => {
            Some((open_pipe(state, *reader, *writer), None))
        }
        ProcessRequest::ReadPipe { reader, count } => {
            read_pipe(state, *reader, *count).map(|(value, bytes)| {
                let retained = wait_source.then_some(Retained::PipeRead {
                    reader: *reader,
                    bytes,
                });
                (value, retained)
            })
        }
        ProcessRequest::WritePipe { writer, bytes } => {
            write_pipe(state, *writer, bytes).map(|value| (value, None))
        }
        ProcessRequest::ClosePipe { token } => Some((
            if state.pipes.remove(token).is_some() {
                pipe_ok(HostValue::Unit)
            } else {
                pipe_error(CoreCtor::PipeErrorClosed, None)
            },
            None,
        )),
        ProcessRequest::Spawn { child, spec } => Some((spawn_child(state, *child, spec), None)),
        ProcessRequest::WaitChild { child } => {
            wait_child(state, *child, wait_source).map(|value| {
                let retained = wait_source.then_some(Retained::ChildWait { child: *child });
                (value, retained)
            })
        }
        ProcessRequest::TerminateChild { child } => Some((terminate_child(state, *child), None)),
        ProcessRequest::KillChild { child } => Some((kill_child(state, *child), None)),
        ProcessRequest::CloseChild { child } => Some((
            if detach_child(state, *child) {
                exec_ok(HostValue::Unit)
            } else {
                exec_error(CoreCtor::ExecErrorClosed, None)
            },
            None,
        )),
    }
}

fn cancel_request(
    state: &mut ServiceState,
    token: u64,
    wait_source: bool,
    pending_count: &AtomicUsize,
) -> CancelReply {
    if state.pending.remove(&token).is_some() {
        release_pending(pending_count);
        return if wait_source {
            CancelReply::Wait(HostWaitCancel::Cancelled)
        } else {
            CancelReply::Direct(true)
        };
    }
    if wait_source {
        if let Some(retained) = state.retained.remove(&token) {
            restore_retained(state, retained);
            CancelReply::Wait(HostWaitCancel::ReadyRestored)
        } else {
            CancelReply::Wait(HostWaitCancel::Missing)
        }
    } else {
        CancelReply::Direct(false)
    }
}

fn restore_retained(state: &mut ServiceState, retained: Retained) {
    match retained {
        Retained::PipeRead { reader, bytes } => {
            if let Some(pipe) = state.pipes.get_mut(&reader) {
                for byte in bytes.into_iter().rev() {
                    pipe.restored.push_front(byte);
                }
            }
        }
        Retained::ChildWait { .. } => {}
    }
}

fn open_pipe(state: &mut ServiceState, reader: u64, writer: u64) -> HostValue {
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::FromRawFd;
        let mut fds = [0; 2];
        let result = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) };
        if result != 0 {
            return pipe_io_error(std::io::Error::last_os_error(), "pipe creation");
        }
        let read = unsafe { std::fs::File::from_raw_fd(fds[0]) };
        let write = unsafe { std::fs::File::from_raw_fd(fds[1]) };
        state.pipes.insert(
            reader,
            PipeEnd {
                file: read,
                kind: PipeKind::Reader,
                restored: VecDeque::new(),
            },
        );
        state.pipes.insert(
            writer,
            PipeEnd {
                file: write,
                kind: PipeKind::Writer,
                restored: VecDeque::new(),
            },
        );
        pipe_ok(HostValue::Tuple(vec![
            HostValue::PipeReader(reader),
            HostValue::PipeWriter(writer),
        ]))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (state, reader, writer);
        pipe_error(
            CoreCtor::PipeErrorUnsupported,
            Some("anonymous pipes are unsupported on this host"),
        )
    }
}

fn read_pipe(state: &mut ServiceState, reader: u64, count: usize) -> Option<(HostValue, Vec<u8>)> {
    let Some(pipe) = state.pipes.get_mut(&reader) else {
        return Some((pipe_error(CoreCtor::PipeErrorClosed, None), Vec::new()));
    };
    if !matches!(pipe.kind, PipeKind::Reader) {
        return Some((
            pipe_error(
                CoreCtor::PipeErrorInvalidInput,
                Some("the pipe end is not readable"),
            ),
            Vec::new(),
        ));
    }
    if !pipe.restored.is_empty() {
        let count = count.min(pipe.restored.len());
        let bytes: Vec<u8> = pipe.restored.drain(..count).collect();
        return Some((pipe_ok(HostValue::Bytes(bytes.clone().into())), bytes));
    }
    if !fd_ready(&pipe.file, libc::POLLIN | libc::POLLHUP) {
        return None;
    }
    let mut bytes = vec![0; count];
    match pipe.file.read(&mut bytes) {
        Ok(read) => {
            bytes.truncate(read);
            Some((pipe_ok(HostValue::Bytes(bytes.clone().into())), bytes))
        }
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => None,
        Err(error) => Some((pipe_io_error(error, "pipe read"), Vec::new())),
    }
}

fn write_pipe(state: &mut ServiceState, writer: u64, bytes: &SharedBytes) -> Option<HostValue> {
    let Some(pipe) = state.pipes.get_mut(&writer) else {
        return Some(pipe_error(CoreCtor::PipeErrorClosed, None));
    };
    if !matches!(pipe.kind, PipeKind::Writer) {
        return Some(pipe_error(
            CoreCtor::PipeErrorInvalidInput,
            Some("the pipe end is not writable"),
        ));
    }
    if bytes.is_empty() {
        return Some(pipe_ok(HostValue::Int(0)));
    }
    if !fd_ready(&pipe.file, libc::POLLOUT | libc::POLLHUP) {
        return None;
    }
    let count = bytes.len().min(PIPE_WRITE_CHUNK);
    match pipe.file.write(&bytes[..count]) {
        Ok(written) => Some(pipe_ok(HostValue::Int(written as i64))),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => None,
        Err(error) => Some(pipe_io_error(error, "pipe write")),
    }
}

fn fd_ready(file: &std::fs::File, events: i16) -> bool {
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        let mut descriptor = libc::pollfd {
            fd: file.as_raw_fd(),
            events,
            revents: 0,
        };
        unsafe { libc::poll(&mut descriptor, 1, 0) > 0 }
    }
    #[cfg(not(unix))]
    {
        let _ = (file, events);
        false
    }
}

fn spawn_child(state: &mut ServiceState, child: u64, spec: &HostExecSpec) -> HostValue {
    let mut command = Command::new(spec.program.as_str());
    command.args(spec.arguments.iter().map(|value| value.as_str()));
    if let Some(directory) = &spec.directory {
        command.current_dir(directory.as_str());
    }
    apply_child_environment(&mut command, &spec.environment);
    let stdin = match child_input(state, spec.input) {
        Ok(value) => value,
        Err(error) => return child_pipe_error(error),
    };
    let stdout = match child_output(state, spec.output) {
        Ok(value) => value,
        Err(error) => return child_pipe_error(error),
    };
    let stderr = match child_output(state, spec.error) {
        Ok(value) => value,
        Err(error) => return child_pipe_error(error),
    };
    command.stdin(stdin).stdout(stdout).stderr(stderr);
    match command.spawn() {
        Ok(spawned) => {
            consume_spec_pipes(state, spec);
            state.children.insert(
                child,
                ChildState {
                    child: spawned,
                    status: None,
                },
            );
            exec_ok(HostValue::Child(child))
        }
        Err(error) => exec_io_error(error, "child spawn"),
    }
}

fn apply_child_environment(command: &mut Command, environment: &HostChildEnv) {
    match environment {
        HostChildEnv::Inherit => {}
        HostChildEnv::Exact(values) => {
            command.env_clear();
            command.envs(
                values
                    .iter()
                    .map(|(name, value)| (name.as_str(), value.as_str())),
            );
        }
        HostChildEnv::Overlay(values) => {
            command.envs(
                values
                    .iter()
                    .map(|(name, value)| (name.as_str(), value.as_str())),
            );
        }
    }
}

fn child_input(state: &ServiceState, input: HostChildInput) -> Result<Stdio, ChildPipeError> {
    match input {
        HostChildInput::Inherit => Ok(Stdio::inherit()),
        HostChildInput::Null => Ok(Stdio::null()),
        HostChildInput::Pipe(token) => clone_pipe(state, token, PipeKind::Reader),
    }
}

fn child_output(state: &ServiceState, output: HostChildOutput) -> Result<Stdio, ChildPipeError> {
    match output {
        HostChildOutput::Inherit => Ok(Stdio::inherit()),
        HostChildOutput::Null => Ok(Stdio::null()),
        HostChildOutput::Pipe(token) => clone_pipe(state, token, PipeKind::Writer),
    }
}

fn clone_pipe(
    state: &ServiceState,
    token: u64,
    expected: PipeKind,
) -> Result<Stdio, ChildPipeError> {
    let Some(pipe) = state.pipes.get(&token) else {
        return Err(ChildPipeError::Closed);
    };
    if std::mem::discriminant(&pipe.kind) != std::mem::discriminant(&expected) {
        return Err(ChildPipeError::WrongDirection);
    }
    pipe.file
        .try_clone()
        .map(Stdio::from)
        .map_err(ChildPipeError::Io)
}

fn child_pipe_error(error: ChildPipeError) -> HostValue {
    match error {
        ChildPipeError::Closed => exec_error(CoreCtor::ExecErrorClosed, None),
        ChildPipeError::WrongDirection => exec_error(
            CoreCtor::ExecErrorInvalidInput,
            Some("the child pipe has the wrong direction"),
        ),
        ChildPipeError::Io(error) => exec_io_error(error, "child pipe clone"),
    }
}

fn consume_spec_pipes(state: &mut ServiceState, spec: &HostExecSpec) {
    let mut tokens = Vec::new();
    if let HostChildInput::Pipe(token) = spec.input {
        tokens.push(token);
    }
    for output in [spec.output, spec.error] {
        if let HostChildOutput::Pipe(token) = output {
            if !tokens.contains(&token) {
                tokens.push(token);
            }
        }
    }
    for token in tokens {
        state.pipes.remove(&token);
    }
}

fn wait_child(state: &mut ServiceState, child: u64, wait_source: bool) -> Option<HostValue> {
    let Some(current) = state.children.get_mut(&child) else {
        return Some(exec_error(CoreCtor::ExecErrorClosed, None));
    };
    if current.status.is_none() {
        match current.child.try_wait() {
            Ok(Some(status)) => current.status = Some(child_status(status)),
            Ok(None) => return None,
            Err(error) => return Some(exec_io_error(error, "child wait")),
        }
    }
    let value = exec_ok(child_status_value(
        current.status.expect("the child status exists"),
    ));
    if !wait_source {
        state.children.remove(&child);
    }
    Some(value)
}

fn child_status(status: ExitStatus) -> ChildStatus {
    match status.code() {
        Some(code) => ChildStatus::Exited(i64::from(code)),
        None => ChildStatus::Terminated,
    }
}

fn child_status_value(status: ChildStatus) -> HostValue {
    match status {
        ChildStatus::Exited(code) => {
            HostValue::Ctor(CoreCtor::ChildStatusExited, vec![HostValue::Int(code)])
        }
        ChildStatus::Terminated => HostValue::Ctor(CoreCtor::ChildStatusTerminated, Vec::new()),
    }
}

fn terminate_child(state: &mut ServiceState, child: u64) -> HostValue {
    let Some(child) = state.children.get_mut(&child) else {
        return exec_error(CoreCtor::ExecErrorClosed, None);
    };
    #[cfg(unix)]
    {
        let result = unsafe { libc::kill(child.child.id() as libc::pid_t, libc::SIGTERM) };
        if result == 0 {
            exec_ok(HostValue::Unit)
        } else {
            exec_io_error(std::io::Error::last_os_error(), "child termination")
        }
    }
    #[cfg(not(unix))]
    {
        let _ = child;
        exec_error(
            CoreCtor::ExecErrorUnsupported,
            Some("normal child termination is unsupported on this host"),
        )
    }
}

fn kill_child(state: &mut ServiceState, child: u64) -> HostValue {
    let Some(child) = state.children.get_mut(&child) else {
        return exec_error(CoreCtor::ExecErrorClosed, None);
    };
    match child.child.kill() {
        Ok(()) => exec_ok(HostValue::Unit),
        Err(error) => exec_io_error(error, "child kill"),
    }
}

fn detach_child(state: &mut ServiceState, child: u64) -> bool {
    let Some(child) = state.children.remove(&child) else {
        return false;
    };
    state.detached.push(child.child);
    true
}

fn reap_detached(children: &mut Vec<Child>) {
    let mut index = 0;
    while index < children.len() {
        match children[index].try_wait() {
            Ok(Some(_)) => {
                let mut child = children.swap_remove(index);
                let _ = child.wait();
            }
            Ok(None) | Err(_) => index += 1,
        }
    }
}

fn pipe_ok(value: HostValue) -> HostValue {
    HostValue::Ctor(CoreCtor::Ok, vec![value])
}

fn pipe_error(ctor: CoreCtor, message: Option<&str>) -> HostValue {
    let fields = message
        .map(|message| vec![HostValue::Str(message.to_string().into())])
        .unwrap_or_default();
    HostValue::Ctor(CoreCtor::Err, vec![HostValue::Ctor(ctor, fields)])
}

fn pipe_io_error(error: std::io::Error, action: &str) -> HostValue {
    let ctor = match error.kind() {
        std::io::ErrorKind::BrokenPipe => CoreCtor::PipeErrorBrokenPipe,
        std::io::ErrorKind::InvalidInput => CoreCtor::PipeErrorInvalidInput,
        _ => CoreCtor::PipeErrorFailed,
    };
    let message = format!("{action} failed: {error}");
    pipe_error(ctor, Some(&message))
}

fn exec_ok(value: HostValue) -> HostValue {
    HostValue::Ctor(CoreCtor::Ok, vec![value])
}

fn exec_error(ctor: CoreCtor, message: Option<&str>) -> HostValue {
    let fields = message
        .map(|message| vec![HostValue::Str(message.to_string().into())])
        .unwrap_or_default();
    HostValue::Ctor(CoreCtor::Err, vec![HostValue::Ctor(ctor, fields)])
}

fn exec_io_error(error: std::io::Error, action: &str) -> HostValue {
    let ctor = match error.kind() {
        std::io::ErrorKind::NotFound => CoreCtor::ExecErrorNotFound,
        std::io::ErrorKind::PermissionDenied => CoreCtor::ExecErrorPermissionDenied,
        std::io::ErrorKind::InvalidInput => CoreCtor::ExecErrorInvalidInput,
        std::io::ErrorKind::Unsupported => CoreCtor::ExecErrorUnsupported,
        _ => CoreCtor::ExecErrorFailed,
    };
    let message = format!("{action} failed: {error}");
    exec_error(ctor, Some(&message))
}

fn release_pending(pending: &AtomicUsize) {
    let previous = pending.fetch_sub(1, Ordering::Relaxed);
    debug_assert!(previous > 0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlay_replaces_one_value_and_preserves_the_parent_path() {
        let path = std::env::var("PATH").expect("the test process has PATH");
        let mut command = Command::new(std::env::current_exe().expect("the test path exists"));
        command
            .arg("--exact")
            .arg("process_service::tests::child_environment_probe");
        apply_child_environment(
            &mut command,
            &HostChildEnv::Overlay(vec![
                ("LOOM_OVERLAY_TEST".into(), "changed".into()),
                ("LOOM_PARENT_PATH".into(), path.into()),
            ]),
        );
        let output = command.output().expect("the child runs");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn child_environment_probe() {
        let Ok(value) = std::env::var("LOOM_OVERLAY_TEST") else {
            return;
        };
        assert_eq!(value, "changed");
        assert_eq!(std::env::var("PATH"), std::env::var("LOOM_PARENT_PATH"));
    }
}
