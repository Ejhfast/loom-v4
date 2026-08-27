//! The host-completion interface.
//!
//! `lm-vm` never touches the operating system. A `Host` receives
//! plain-data arguments for one root-granted fixed operation and
//! completes with a plain-data reply, now or later. No Rust reference
//! into guest memory crosses this boundary in either direction.

use crate::CompletionKey;
use lm_heap::{SharedBytes, SharedText};
use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

/// One scheduler wake callback for asynchronous host readiness.
pub type HostWake = Arc<dyn Fn() + Send + Sync>;

/// One plain-data operation argument.
#[derive(Debug, Clone, PartialEq)]
pub enum HostArg {
    Unit,
    Bool(bool),
    Int(i64),
    Float(u64),
    Str(SharedText),
    Bytes(SharedBytes),
    File(u64),
    OpenOptions(HostOpenOptions),
    SeekFrom(HostSeekFrom),
    RenameMode(HostRenameMode),
    SocketAddress(HostSocketAddress),
    Tcp(HostTcpResource),
    Shutdown(HostShutdown),
    List(Vec<HostArg>),
    Tuple(Vec<HostArg>),
    Option(Option<Box<HostArg>>),
    Result(Result<Box<HostArg>, Box<HostArg>>),
    Tls(u64),
    StdStream(HostStdStream),
    RawMode(u64),
    SignalKind(HostSignalKind),
    SignalStream(u64),
    PipeReader(u64),
    PipeWriter(u64),
    Child(u64),
    Udp(u64),
    ExecSpec(HostExecSpec),
    Resource(HostResource),
    CompileEnv(HostCompileEnv),
    CompileOptions(HostCompileOptions),
    Syntax {
        source: SharedText,
        records: SharedBytes,
        index: u32,
    },
}

/// One verified module supplied to the runtime compiler.
#[derive(Debug, Clone, PartialEq)]
pub struct HostCompileModule {
    pub artifact: SharedBytes,
}

/// One explicit runtime compiler environment.
#[derive(Debug, Clone, PartialEq)]
pub struct HostCompileEnv {
    pub modules: Vec<HostCompileModule>,
    pub roots: Vec<(SharedText, SharedText)>,
    pub definitions: Vec<HostCompileDefinition>,
}

/// One stable definition binding for runtime compilation.
#[derive(Debug, Clone, PartialEq)]
pub struct HostCompileDefinition {
    pub local_name: SharedText,
    pub module_name: SharedText,
    pub qualified_key: SharedText,
    pub contract_hash: [u8; 32],
    pub implementation_hash: [u8; 32],
    pub module_hash: [u8; 32],
    pub slots: Vec<HostCompileSlot>,
}

/// One verified slot contract for runtime compilation.
#[derive(Debug, Clone, PartialEq)]
pub struct HostCompileSlot {
    pub artifact: SharedBytes,
    pub index: u32,
}

/// One explicit runtime compiler option set.
#[derive(Debug, Clone, PartialEq)]
pub struct HostCompileOptions {
    pub is_main: bool,
    pub dynamic_result: bool,
    pub late_definitions: bool,
    pub late_functions: Vec<SharedText>,
    pub late_classes: Vec<SharedText>,
}

/// One portable IP address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HostIpAddress {
    V4([u8; 4]),
    V6([u8; 16]),
}

/// One portable socket address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HostSocketAddress {
    pub ip: HostIpAddress,
    pub port: u16,
    pub flow_info: u32,
    pub scope_id: u32,
}

/// One TCP resource kind at the host boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HostTcpKind {
    Stream,
    Listener,
}

/// One opaque TCP resource token at the host boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HostTcpResource {
    pub kind: HostTcpKind,
    pub token: u64,
}

/// One opaque extension resource at the host boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HostResource {
    pub kind: [u8; 32],
    pub token: u64,
    pub generation: u32,
}

/// One portable TCP shutdown direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostShutdown {
    Read,
    Write,
    Both,
}

/// One standard process stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostStdStream {
    Input,
    Output,
    Error,
}

/// One portable process signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostSignalKind {
    Interrupt,
    Terminate,
}

/// One portable child standard-input binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostChildInput {
    Inherit,
    Null,
    Pipe(u64),
}

/// One portable child standard-output binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostChildOutput {
    Inherit,
    Null,
    Pipe(u64),
}

/// One portable child environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostChildEnv {
    Inherit,
    Exact(Vec<(SharedText, SharedText)>),
    Overlay(Vec<(SharedText, SharedText)>),
}

/// One complete operating-system child specification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostExecSpec {
    pub program: SharedText,
    pub arguments: Vec<SharedText>,
    pub directory: Option<SharedText>,
    pub environment: HostChildEnv,
    pub input: HostChildInput,
    pub output: HostChildOutput,
    pub error: HostChildOutput,
}

/// One portable file-open mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostOpenOptions {
    ReadOnly,
    WriteOnly,
    ReadWrite,
    Create,
    CreateTruncate,
    CreateNew,
    Append,
}

/// One portable file rename mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostRenameMode {
    NoReplace,
    Replace,
}

/// One portable file-seek origin and offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostSeekFrom {
    Start(i64),
    Current(i64),
    End(i64),
}

/// One core constructor a host reply may name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoreCtor {
    Some,
    None,
    Ok,
    Err,
    IoErrorBrokenPipe,
    IoErrorInvalidInput,
    IoErrorLimitExceeded,
    IoErrorUnsupported,
    IoErrorFailed,
    EnvInvalidName,
    EnvInvalidEncoding,
    EnvPermissionDenied,
    EnvFailed,
    EntropyInvalidInput,
    EntropyLimitExceeded,
    EntropyUnavailable,
    EntropyFailed,
    FsErrorClosed,
    FsErrorInvalidInput,
    FsErrorInvalidEncoding,
    FsErrorLimitExceeded,
    FsErrorNotFound,
    FsErrorAlreadyExists,
    FsErrorPermissionDenied,
    FsErrorNotDirectory,
    FsErrorIsDirectory,
    FsErrorDirectoryNotEmpty,
    FsErrorCrossDevice,
    FsErrorUnsupported,
    FsErrorFailed,
    FileKindFile,
    FileKindDirectory,
    FileKindSymlink,
    FileKindOther,
    FileInfo,
    DirEntry,
    NetInvalidInput,
    NetNameNotFound,
    NetUnavailable,
    NetPermissionDenied,
    NetAddressInUse,
    NetConnectionRefused,
    NetConnectionReset,
    NetNotConnected,
    NetTimedOut,
    NetClosed,
    NetLimitExceeded,
    NetUnsupported,
    NetFailed,
    TcpReadData,
    TcpReadEnd,
    TlsInvalidConfig,
    TlsHandshake,
    TlsCertificate,
    TlsProtocol,
    TlsNetwork,
    TlsClosed,
    TlsLimitExceeded,
    TtySize,
    TtyClosed,
    TtyNotTerminal,
    TtyBusy,
    TtyPermissionDenied,
    TtyUnsupported,
    TtyFailed,
    SignalInterrupt,
    SignalTerminate,
    SignalClosed,
    SignalInvalidInput,
    SignalBusy,
    SignalUnsupported,
    SignalLimitExceeded,
    SignalFailed,
    PipeErrorClosed,
    PipeErrorBrokenPipe,
    PipeErrorInvalidInput,
    PipeErrorLimitExceeded,
    PipeErrorUnsupported,
    PipeErrorFailed,
    ChildStatusExited,
    ChildStatusTerminated,
    ExecErrorClosed,
    ExecErrorInvalidInput,
    ExecErrorLimitExceeded,
    ExecErrorNotFound,
    ExecErrorPermissionDenied,
    ExecErrorUnsupported,
    ExecErrorFailed,
    UdpDatagram,
    CompileErrors,
}

/// One parser status at the host boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostParseStatus {
    Complete,
    Incomplete,
    Invalid,
}

/// One syntax diagnostic at the host boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostSyntaxDiagnostic {
    pub start: u32,
    pub stop: u32,
    pub message: SharedText,
}

/// One plain-data operation reply. `Ctor` builds a pinned core enum
/// value inside the performing machine.
#[derive(Debug, Clone, PartialEq)]
pub enum HostValue {
    Unit,
    Bool(bool),
    Int(i64),
    Float(u64),
    Str(SharedText),
    Bytes(SharedBytes),
    File(u64),
    List(Vec<HostValue>),
    Tuple(Vec<HostValue>),
    SocketAddress(HostSocketAddress),
    TcpStream(u64),
    TcpListener(u64),
    TlsStream(u64),
    RawMode(u64),
    SignalStream(u64),
    PipeReader(u64),
    PipeWriter(u64),
    Child(u64),
    UdpSocket(u64),
    Resource(HostResource),
    Artifact(SharedBytes),
    SyntaxParse {
        source: SharedText,
        records: SharedBytes,
        status: HostParseStatus,
        diagnostics: Vec<HostSyntaxDiagnostic>,
    },
    Ctor(CoreCtor, Vec<HostValue>),
}

impl HostValue {
    /// Build one `Option.Some` reply.
    pub fn some(value: HostValue) -> HostValue {
        HostValue::Ctor(CoreCtor::Some, vec![value])
    }

    /// Build one `Option.None` reply.
    pub fn none() -> HostValue {
        HostValue::Ctor(CoreCtor::None, Vec::new())
    }

    /// Build one successful `Result` reply.
    pub fn ok(value: HostValue) -> HostValue {
        HostValue::Ctor(CoreCtor::Ok, vec![value])
    }

    /// Build one failed `Result` reply.
    pub fn err(value: HostValue) -> HostValue {
        HostValue::Ctor(CoreCtor::Err, vec![value])
    }
}

/// How one started operation proceeds.
#[derive(Debug, Clone, PartialEq)]
pub enum HostStart {
    /// The operation completed synchronously.
    Completed(HostValue),
    /// The operation waits. The token names the pending completion.
    Waiting(u64),
    /// The host cannot serve the operation. The machine faults with
    /// `HostFault`.
    Failed(String),
}

/// The result of cancelling one selectable host source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostWaitCancel {
    /// Cancellation removed the source before selection.
    Cancelled,
    /// The host had retained a ready result and restored its input.
    ReadyRestored,
    /// No current source used this token.
    Missing,
}

/// One completed asynchronous host operation.
#[derive(Debug, Clone, PartialEq)]
pub struct HostCompletion {
    /// The machine request that started the operation.
    pub key: CompletionKey,
    /// The host scope returned by `HostStart::Waiting`.
    pub token: u64,
    /// The plain-data reply or an asynchronous host failure.
    pub result: Result<HostValue, String>,
}

/// The root host registry. The VM calls it only for operations that
/// the policy chain passed to the root.
pub trait Host {
    /// Start one operation.
    fn start(&mut self, key: CompletionKey, op: u32, args: Vec<HostArg>) -> HostStart;
    /// Start one operation as a selectable source.
    fn start_wait(&mut self, key: CompletionKey, op: u32, args: Vec<HostArg>) -> HostStart {
        self.start(key, op, args)
    }
    /// Poll the completion source without blocking.
    fn poll(&mut self) -> Option<HostCompletion>;
    /// Wait for the next completion.
    fn wait(&mut self) -> Option<HostCompletion>;
    /// Set the wake callback used by a parallel scheduler.
    fn set_scheduler_wake(&mut self, _wake: Option<HostWake>) {}
    /// Return nanoseconds until the next host-managed timer expires.
    fn scheduler_wait_nanos(&self) -> Option<u64> {
        None
    }
    /// Close one file token during forced resource cleanup.
    fn close_file(&mut self, _token: u64) -> bool {
        false
    }
    /// Cancel one pending completion token.
    fn cancel(&mut self, _token: u64) -> bool {
        false
    }
    /// Commit one ready selectable source.
    fn commit_wait(&mut self, _token: u64) -> bool {
        true
    }
    /// Cancel one selectable source and preserve consumable input.
    fn cancel_wait(&mut self, token: u64) -> HostWaitCancel {
        if self.cancel(token) {
            HostWaitCancel::Cancelled
        } else {
            HostWaitCancel::Missing
        }
    }
    /// Close one TCP resource during forced cleanup.
    fn close_tcp(&mut self, _resource: HostTcpResource) -> bool {
        false
    }
    /// Close one TLS stream during forced resource cleanup.
    fn close_tls(&mut self, _token: u64) -> bool {
        false
    }
    /// Restore one raw terminal resource during forced cleanup.
    fn close_raw_mode(&mut self, _token: u64) -> bool {
        false
    }
    /// Close one signal stream during forced cleanup.
    fn close_signal_stream(&mut self, _token: u64) -> bool {
        false
    }
    /// Close one pipe end during forced resource cleanup.
    fn close_pipe(&mut self, _token: u64) -> bool {
        false
    }
    /// Detach one child during forced resource cleanup.
    fn close_child(&mut self, _token: u64) -> bool {
        false
    }
    /// Close one UDP socket during forced resource cleanup.
    fn close_udp(&mut self, _token: u64) -> bool {
        false
    }
    /// Close one opaque extension resource during forced cleanup.
    fn close_resource(&mut self, _resource: HostResource) -> bool {
        false
    }
}

/// A host without any implementation. Every started operation fails.
pub struct NullHost;

impl Host for NullHost {
    fn start(&mut self, _key: CompletionKey, op: u32, _args: Vec<HostArg>) -> HostStart {
        HostStart::Failed(format!("no host implementation for operation slot {op}"))
    }

    fn poll(&mut self) -> Option<HostCompletion> {
        None
    }

    fn wait(&mut self) -> Option<HostCompletion> {
        None
    }
}

/// A deterministic in-memory host for tests.
///
/// It records output and provides input, clocks, random values, and
/// in-memory files. Sleep completes after a fixed number of polls.
pub struct RecordingHost {
    pub input_bytes: Vec<u8>,
    pub written_bytes: Vec<u8>,
    pub written_error_bytes: Vec<u8>,
    /// Exact operations in host submission order.
    pub operations: Vec<u32>,
    /// The maximum bytes accepted by one console write.
    pub console_write_limit: usize,
    pub environment: BTreeMap<String, String>,
    pub arguments: Vec<String>,
    pub current_dir: String,
    now: i64,
    monotonic: i64,
    rand_state: u64,
    /// Deferred replies, by completion token. The command-line host
    /// serves files and streams on worker threads, so this host defers
    /// the same operations. A test therefore meets the same boundary:
    /// a reply arrives at a later poll, not inside `start`.
    pending: BTreeMap<u64, Deferred>,
    ready_waits: std::collections::BTreeSet<u64>,
    retained_waits: BTreeMap<u64, RetainedWait>,
    next_token: u64,
    files: BTreeMap<String, Vec<u8>>,
    directories: std::collections::BTreeSet<String>,
    file_handles: BTreeMap<u64, MemoryFile>,
    next_file: u64,
    dns: BTreeMap<String, Vec<HostIpAddress>>,
    listeners: BTreeMap<u64, MemoryListener>,
    listener_addresses: BTreeMap<HostSocketAddress, u64>,
    streams: BTreeMap<u64, MemoryStream>,
    tls_streams: std::collections::BTreeSet<u64>,
    next_tcp: u64,
    next_port: u16,
    terminal_streams: [bool; 3],
    terminal_size: (i64, i64),
    raw_mode: Option<u64>,
    next_raw_mode: u64,
    signal_stream: Option<MemorySignalStream>,
    next_signal_stream: u64,
    signals_on_open: VecDeque<HostSignalKind>,
    pipes: BTreeMap<u64, MemoryPipe>,
    pipe_ends: BTreeMap<u64, MemoryPipeEnd>,
    next_pipe: u64,
    children: BTreeMap<u64, MemoryChild>,
    child_programs: BTreeMap<String, MemoryChildProgram>,
    next_child: u64,
    udp_sockets: BTreeMap<u64, MemoryUdpSocket>,
    udp_addresses: BTreeMap<HostSocketAddress, u64>,
    next_udp: u64,
}

/// One reply this host holds until a later poll.
#[derive(Debug)]
struct Deferred {
    key: CompletionKey,
    /// Polls remaining before the reply is ready.
    left: u32,
    action: DeferredAction,
    wait_source: bool,
    rollback: Option<RetainedWait>,
}

#[derive(Debug, Clone)]
enum DeferredAction {
    Ready(HostValue),
    InputRead(usize),
    Accept(u64),
    Read { stream: u64, count: usize },
    TlsRead { stream: u64, count: usize },
    SignalNext { stream: u64 },
    PipeRead { reader: u64, count: usize },
    ChildWait { child: u64 },
    UdpRecv { socket: u64 },
}

#[derive(Debug)]
enum RetainedWait {
    Input(Vec<u8>),
    StreamRead {
        stream: u64,
        bytes: Vec<u8>,
    },
    Accept {
        listener: u64,
        connection: (u64, HostSocketAddress),
    },
    Connect {
        client: u64,
    },
    Signal(HostSignalKind),
    PipeRead {
        pipe: u64,
        bytes: Vec<u8>,
    },
    ChildWait {
        child: u64,
    },
    UdpDatagram {
        socket: u64,
        bytes: SharedBytes,
        peer: HostSocketAddress,
    },
}

#[derive(Debug)]
struct MemorySignalStream {
    token: u64,
    interrupt: bool,
    terminate: bool,
    queued: VecDeque<HostSignalKind>,
}

#[derive(Debug)]
struct MemoryPipe {
    bytes: VecDeque<u8>,
    reader_open: bool,
    writer_open: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MemoryPipeKind {
    Reader,
    Writer,
}

#[derive(Debug, Clone, Copy)]
struct MemoryPipeEnd {
    pipe: u64,
    kind: MemoryPipeKind,
}

#[derive(Debug, Clone)]
struct MemoryChild {
    status: MemoryChildStatus,
}

#[derive(Debug, Clone, Copy)]
enum MemoryChildStatus {
    Exited(i64),
    Terminated,
}

#[derive(Debug, Clone)]
struct MemoryChildProgram {
    status: MemoryChildStatus,
    output: Vec<u8>,
    error: Vec<u8>,
}

#[derive(Debug)]
struct MemoryUdpSocket {
    address: HostSocketAddress,
    incoming: VecDeque<(SharedBytes, HostSocketAddress)>,
}

/// True when the command-line host serves this operation off the
/// scheduler thread. This host defers the same set.
fn deferred_op(op: u32) -> bool {
    matches!(
        op,
        lm_abi::OP_FS_OPEN
            | lm_abi::OP_FS_READ
            | lm_abi::OP_FS_WRITE
            | lm_abi::OP_FS_SEEK
            | lm_abi::OP_FS_FLUSH
            | lm_abi::OP_FS_SYNC
            | lm_abi::OP_FS_CLOSE
            | lm_abi::OP_FS_STAT
            | lm_abi::OP_FS_READ_DIR
            | lm_abi::OP_FS_CREATE_DIR
            | lm_abi::OP_FS_REMOVE_FILE
            | lm_abi::OP_FS_REMOVE_DIR
            | lm_abi::OP_FS_RENAME
            | lm_abi::OP_FS_SYNC_DIR
            | lm_abi::OP_IO_READ_BYTES
            | lm_abi::OP_IO_WRITE
            | lm_abi::OP_IO_WRITE_ERROR
            | lm_abi::OP_DNS_RESOLVE
            | lm_abi::OP_TCP_CONNECT
            | lm_abi::OP_TCP_LISTEN
            | lm_abi::OP_TCP_ACCEPT
            | lm_abi::OP_TCP_READ
            | lm_abi::OP_TCP_WRITE
            | lm_abi::OP_TCP_SHUTDOWN
            | lm_abi::OP_TCP_LOCAL_ADDRESS
            | lm_abi::OP_TCP_PEER_ADDRESS
            | lm_abi::OP_TCP_CLOSE
            | lm_abi::OP_TLS_HANDSHAKE
            | lm_abi::OP_TLS_SERVER_HANDSHAKE
            | lm_abi::OP_TLS_READ
            | lm_abi::OP_TLS_WRITE
            | lm_abi::OP_TLS_SHUTDOWN
            | lm_abi::OP_TLS_LOCAL_ADDRESS
            | lm_abi::OP_TLS_PEER_ADDRESS
            | lm_abi::OP_TLS_CLOSE
            | lm_abi::OP_PIPE_OPEN
            | lm_abi::OP_PIPE_READ
            | lm_abi::OP_PIPE_WRITE
            | lm_abi::OP_PIPE_CLOSE
            | lm_abi::OP_EXEC_SPAWN
            | lm_abi::OP_EXEC_WAIT
            | lm_abi::OP_EXEC_TERMINATE
            | lm_abi::OP_EXEC_KILL
            | lm_abi::OP_EXEC_CLOSE
            | lm_abi::OP_UDP_BIND
            | lm_abi::OP_UDP_SEND_TO
            | lm_abi::OP_UDP_RECV_FROM
            | lm_abi::OP_UDP_LOCAL_ADDRESS
            | lm_abi::OP_UDP_CLOSE
    )
}

#[derive(Debug)]
struct MemoryFile {
    path: String,
    cursor: usize,
    readable: bool,
    writable: bool,
    append: bool,
}

#[derive(Debug)]
struct MemoryListener {
    address: HostSocketAddress,
    backlog: usize,
    incoming: VecDeque<(u64, HostSocketAddress)>,
}

#[derive(Debug)]
struct MemoryStream {
    local: HostSocketAddress,
    peer_address: HostSocketAddress,
    peer: Option<u64>,
    incoming: VecDeque<u8>,
    read_closed: bool,
    write_closed: bool,
    peer_write_closed: bool,
}

const MAX_FILE_IO_BYTES: usize = 16 << 20;
const MAX_NETWORK_IO_BYTES: usize = 16 << 20;
const MAX_CONSOLE_IO_BYTES: usize = 16 << 20;
const MAX_ENTROPY_BYTES: usize = 16 << 20;
const MAX_DNS_RESULTS: usize = 64;
const MAX_VIRTUAL_STREAM_BYTES: usize = 64 << 20;
const VIRTUAL_WRITE_CHUNK: usize = 4 << 10;
const MAX_PIPE_IO_BYTES: usize = 16 << 20;
const MAX_VIRTUAL_PIPE_BYTES: usize = 64 << 20;
const MAX_EXEC_ITEMS: usize = 4_096;
const MAX_EXEC_TEXT_BYTES: usize = 1 << 20;
const MAX_EXEC_ITEM_BYTES: usize = 64 << 10;
const MAX_UDP_DATAGRAM_BYTES: usize = 65_535;
const MAX_VIRTUAL_UDP_BYTES: usize = 64 << 20;

fn fs_ok(value: HostValue) -> HostValue {
    HostValue::Ctor(CoreCtor::Ok, vec![value])
}

fn core_ok(value: HostValue) -> HostValue {
    HostValue::Ctor(CoreCtor::Ok, vec![value])
}

fn core_error(ctor: CoreCtor, message: Option<&str>) -> HostValue {
    let fields = message
        .map(|message| vec![HostValue::Str(message.to_string().into())])
        .unwrap_or_default();
    HostValue::Ctor(CoreCtor::Err, vec![HostValue::Ctor(ctor, fields)])
}

fn fs_closed() -> HostValue {
    HostValue::Ctor(
        CoreCtor::Err,
        vec![HostValue::Ctor(CoreCtor::FsErrorClosed, Vec::new())],
    )
}

fn net_ok(value: HostValue) -> HostValue {
    HostValue::Ctor(CoreCtor::Ok, vec![value])
}

fn net_error(ctor: CoreCtor, message: impl Into<String>) -> HostValue {
    HostValue::Ctor(
        CoreCtor::Err,
        vec![HostValue::Ctor(
            ctor,
            vec![HostValue::Str(SharedText::from(message.into()))],
        )],
    )
}

fn net_closed() -> HostValue {
    HostValue::Ctor(
        CoreCtor::Err,
        vec![HostValue::Ctor(CoreCtor::NetClosed, vec![])],
    )
}

fn tls_ok(value: HostValue) -> HostValue {
    HostValue::Ctor(CoreCtor::Ok, vec![value])
}

fn tls_error(ctor: CoreCtor, message: impl Into<String>) -> HostValue {
    HostValue::Ctor(
        CoreCtor::Err,
        vec![HostValue::Ctor(
            ctor,
            vec![HostValue::Str(SharedText::from(message.into()))],
        )],
    )
}

fn tls_closed() -> HostValue {
    HostValue::Ctor(
        CoreCtor::Err,
        vec![HostValue::Ctor(CoreCtor::TlsClosed, vec![])],
    )
}

fn pipe_ok(value: HostValue) -> HostValue {
    HostValue::Ctor(CoreCtor::Ok, vec![value])
}

fn pipe_error(ctor: CoreCtor, message: Option<&str>) -> HostValue {
    core_error(ctor, message)
}

fn exec_ok(value: HostValue) -> HostValue {
    HostValue::Ctor(CoreCtor::Ok, vec![value])
}

fn exec_error(ctor: CoreCtor, message: Option<&str>) -> HostValue {
    core_error(ctor, message)
}

impl Default for RecordingHost {
    fn default() -> RecordingHost {
        RecordingHost::new(1)
    }
}

impl RecordingHost {
    pub fn new(seed: u64) -> RecordingHost {
        let mut dns = BTreeMap::new();
        dns.insert(
            "localhost".to_string(),
            vec![HostIpAddress::V6([
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1,
            ])],
        );
        RecordingHost {
            input_bytes: Vec::new(),
            written_bytes: Vec::new(),
            written_error_bytes: Vec::new(),
            operations: Vec::new(),
            console_write_limit: usize::MAX,
            environment: BTreeMap::new(),
            arguments: Vec::new(),
            current_dir: "/loom".to_string(),
            now: 1_000,
            monotonic: 0,
            rand_state: seed.max(1),
            pending: BTreeMap::new(),
            ready_waits: std::collections::BTreeSet::new(),
            retained_waits: BTreeMap::new(),
            next_token: 1,
            files: BTreeMap::new(),
            directories: [".".to_string(), "/".to_string(), "/loom".to_string()]
                .into_iter()
                .collect(),
            file_handles: BTreeMap::new(),
            next_file: 1,
            dns,
            listeners: BTreeMap::new(),
            listener_addresses: BTreeMap::new(),
            streams: BTreeMap::new(),
            tls_streams: std::collections::BTreeSet::new(),
            next_tcp: 1,
            next_port: 40_000,
            terminal_streams: [true, true, true],
            terminal_size: (80, 24),
            raw_mode: None,
            next_raw_mode: 1,
            signal_stream: None,
            next_signal_stream: 1,
            signals_on_open: VecDeque::new(),
            pipes: BTreeMap::new(),
            pipe_ends: BTreeMap::new(),
            next_pipe: 1,
            children: BTreeMap::new(),
            child_programs: BTreeMap::new(),
            next_child: 1,
            udp_sockets: BTreeMap::new(),
            udp_addresses: BTreeMap::new(),
            next_udp: 1,
        }
    }

    /// Set one in-memory file before execution.
    pub fn set_file(&mut self, path: impl Into<String>, bytes: Vec<u8>) {
        let path = path.into();
        self.directories.insert(memory_parent(&path).to_string());
        self.files.insert(path, bytes);
    }

    /// Read one in-memory file after execution.
    pub fn file(&self, path: &str) -> Option<&[u8]> {
        self.files.get(path).map(Vec::as_slice)
    }

    /// Set one deterministic DNS answer.
    pub fn set_dns(&mut self, name: impl Into<String>, addresses: Vec<HostIpAddress>) {
        self.dns.insert(name.into(), addresses);
    }

    /// Set one environment value before execution.
    pub fn set_env(&mut self, name: impl Into<String>, value: impl Into<String>) {
        self.environment.insert(name.into(), value.into());
    }

    /// Set the terminal state of one standard stream.
    pub fn set_terminal(&mut self, stream: HostStdStream, terminal: bool) {
        self.terminal_streams[std_stream_index(stream)] = terminal;
    }

    /// Set the terminal size returned by the test host.
    pub fn set_terminal_size(&mut self, columns: i64, rows: i64) {
        self.terminal_size = (columns, rows);
    }

    /// Add one signal notification to the active test stream.
    pub fn notify_signal(&mut self, kind: HostSignalKind) -> bool {
        let Some(stream) = &mut self.signal_stream else {
            return false;
        };
        let requested = match kind {
            HostSignalKind::Interrupt => stream.interrupt,
            HostSignalKind::Terminate => stream.terminate,
        };
        if !requested {
            return false;
        }
        if !stream.queued.contains(&kind) {
            stream.queued.push_back(kind);
        }
        true
    }

    /// Queue one signal when the next test stream opens.
    pub fn queue_signal_on_open(&mut self, kind: HostSignalKind) {
        self.signals_on_open.push_back(kind);
    }

    /// Add one deterministic child program to the test host.
    pub fn set_child_program(
        &mut self,
        program: impl Into<String>,
        exit_code: i64,
        output: Vec<u8>,
        error: Vec<u8>,
    ) {
        self.child_programs.insert(
            program.into(),
            MemoryChildProgram {
                status: MemoryChildStatus::Exited(exit_code),
                output,
                error,
            },
        );
    }

    /// True when the test host owns raw terminal mode.
    pub fn raw_mode_active(&self) -> bool {
        self.raw_mode.is_some()
    }

    /// True when the test host owns one signal stream.
    pub fn signal_stream_active(&self) -> bool {
        self.signal_stream.is_some()
    }

    /// Count the live pipe ends in the test host.
    pub fn pipe_end_count(&self) -> usize {
        self.pipe_ends.len()
    }

    /// Count the live child handles in the test host.
    pub fn child_count(&self) -> usize {
        self.children.len()
    }

    fn next_rand(&mut self) -> u64 {
        // xorshift64*: deterministic and dependency-free.
        let mut x = self.rand_state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.rand_state = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn take_tcp_token(&mut self) -> Option<u64> {
        let token = self.next_tcp;
        self.next_tcp = token.checked_add(1)?;
        Some(token)
    }

    fn take_udp_token(&mut self) -> Option<u64> {
        let token = self.next_udp;
        self.next_udp = token.checked_add(1)?;
        Some(token)
    }

    fn take_port(&mut self) -> Option<u16> {
        let port = self.next_port;
        self.next_port = port.checked_add(1)?;
        Some(port)
    }

    fn accept_value(&mut self, listener: u64) -> Option<HostValue> {
        let Some(listener) = self.listeners.get_mut(&listener) else {
            return Some(net_closed());
        };
        let (stream, peer) = listener.incoming.pop_front()?;
        Some(net_ok(HostValue::Tuple(vec![
            HostValue::TcpStream(stream),
            HostValue::SocketAddress(peer),
        ])))
    }

    fn read_value(&mut self, stream: u64, count: usize) -> Option<HostValue> {
        let Some(stream) = self.streams.get_mut(&stream) else {
            return Some(net_closed());
        };
        if stream.read_closed {
            return Some(net_closed());
        }
        if !stream.incoming.is_empty() {
            let count = count.min(stream.incoming.len());
            let bytes: Vec<u8> = stream.incoming.drain(..count).collect();
            return Some(net_ok(HostValue::Ctor(
                CoreCtor::TcpReadData,
                vec![HostValue::Bytes(bytes.into())],
            )));
        }
        if stream.peer_write_closed || stream.peer.is_none() {
            return Some(net_ok(HostValue::Ctor(CoreCtor::TcpReadEnd, vec![])));
        }
        None
    }

    fn tls_read_value(&mut self, stream: u64, count: usize) -> Option<HostValue> {
        if !self.tls_streams.contains(&stream) {
            return Some(tls_closed());
        }
        let Some(stream) = self.streams.get_mut(&stream) else {
            return Some(tls_closed());
        };
        if stream.read_closed {
            return Some(tls_closed());
        }
        if !stream.incoming.is_empty() {
            let count = count.min(stream.incoming.len());
            let bytes: Vec<u8> = stream.incoming.drain(..count).collect();
            return Some(tls_ok(HostValue::Ctor(
                CoreCtor::TcpReadData,
                vec![HostValue::Bytes(bytes.into())],
            )));
        }
        if stream.peer_write_closed || stream.peer.is_none() {
            return Some(tls_ok(HostValue::Ctor(CoreCtor::TcpReadEnd, vec![])));
        }
        None
    }

    fn signal_next_value(&mut self, token: u64) -> Option<HostValue> {
        let Some(stream) = &mut self.signal_stream else {
            return Some(core_error(CoreCtor::SignalClosed, None));
        };
        if stream.token != token {
            return Some(core_error(CoreCtor::SignalClosed, None));
        }
        let kind = stream.queued.pop_front()?;
        let ctor = match kind {
            HostSignalKind::Interrupt => CoreCtor::SignalInterrupt,
            HostSignalKind::Terminate => CoreCtor::SignalTerminate,
        };
        Some(core_ok(HostValue::Ctor(ctor, vec![])))
    }

    fn close_virtual_tcp(&mut self, resource: HostTcpResource) -> bool {
        match resource.kind {
            HostTcpKind::Listener => {
                let Some(listener) = self.listeners.remove(&resource.token) else {
                    return false;
                };
                self.listener_addresses.remove(&listener.address);
                for (stream, _) in listener.incoming {
                    self.close_virtual_tcp(HostTcpResource {
                        kind: HostTcpKind::Stream,
                        token: stream,
                    });
                }
                true
            }
            HostTcpKind::Stream => {
                self.tls_streams.remove(&resource.token);
                let Some(stream) = self.streams.remove(&resource.token) else {
                    return false;
                };
                if let Some(peer) = stream.peer {
                    if let Some(peer) = self.streams.get_mut(&peer) {
                        peer.peer = None;
                        peer.peer_write_closed = true;
                    }
                }
                true
            }
        }
    }

    fn close_virtual_tls(&mut self, token: u64) -> bool {
        if !self.tls_streams.remove(&token) {
            return false;
        }
        self.close_virtual_tcp(HostTcpResource {
            kind: HostTcpKind::Stream,
            token,
        })
    }

    fn close_virtual_udp(&mut self, token: u64) -> bool {
        let Some(socket) = self.udp_sockets.remove(&token) else {
            return false;
        };
        self.udp_addresses.remove(&socket.address);
        true
    }

    fn udp_receive_value(&mut self, socket: u64) -> Option<HostValue> {
        let Some(socket) = self.udp_sockets.get_mut(&socket) else {
            return Some(net_closed());
        };
        let (bytes, peer) = socket.incoming.pop_front()?;
        Some(net_ok(HostValue::Ctor(
            CoreCtor::UdpDatagram,
            vec![HostValue::Bytes(bytes), HostValue::SocketAddress(peer)],
        )))
    }

    fn take_pipe_token(&mut self) -> Option<u64> {
        let token = self.next_pipe;
        self.next_pipe = token.checked_add(1)?;
        Some(token)
    }

    fn close_virtual_pipe(&mut self, token: u64) -> bool {
        let Some(end) = self.pipe_ends.remove(&token) else {
            return false;
        };
        let remove = if let Some(pipe) = self.pipes.get_mut(&end.pipe) {
            match end.kind {
                MemoryPipeKind::Reader => pipe.reader_open = false,
                MemoryPipeKind::Writer => pipe.writer_open = false,
            }
            !pipe.reader_open && !pipe.writer_open
        } else {
            false
        };
        if remove {
            self.pipes.remove(&end.pipe);
        }
        true
    }

    fn pipe_read_value(&mut self, reader: u64, count: usize) -> Option<HostValue> {
        let Some(end) = self.pipe_ends.get(&reader).copied() else {
            return Some(pipe_error(CoreCtor::PipeErrorClosed, None));
        };
        if end.kind != MemoryPipeKind::Reader {
            return Some(pipe_error(
                CoreCtor::PipeErrorInvalidInput,
                Some("the pipe end is not readable"),
            ));
        }
        let Some(pipe) = self.pipes.get_mut(&end.pipe) else {
            return Some(pipe_error(CoreCtor::PipeErrorClosed, None));
        };
        if !pipe.bytes.is_empty() {
            let count = count.min(pipe.bytes.len());
            let bytes: Vec<u8> = pipe.bytes.drain(..count).collect();
            return Some(pipe_ok(HostValue::Bytes(bytes.into())));
        }
        if !pipe.writer_open {
            return Some(pipe_ok(HostValue::Bytes(Vec::new().into())));
        }
        None
    }

    fn pipe_write_value(&mut self, writer: u64, bytes: &[u8]) -> HostValue {
        let Some(end) = self.pipe_ends.get(&writer).copied() else {
            return pipe_error(CoreCtor::PipeErrorClosed, None);
        };
        if end.kind != MemoryPipeKind::Writer {
            return pipe_error(
                CoreCtor::PipeErrorInvalidInput,
                Some("the pipe end is not writable"),
            );
        }
        let Some(pipe) = self.pipes.get_mut(&end.pipe) else {
            return pipe_error(CoreCtor::PipeErrorClosed, None);
        };
        if !pipe.reader_open {
            return pipe_error(CoreCtor::PipeErrorBrokenPipe, None);
        }
        let available = MAX_VIRTUAL_PIPE_BYTES.saturating_sub(pipe.bytes.len());
        let written = bytes.len().min(VIRTUAL_WRITE_CHUNK).min(available);
        if written == 0 && !bytes.is_empty() {
            return pipe_error(
                CoreCtor::PipeErrorLimitExceeded,
                Some("the virtual pipe buffer is full"),
            );
        }
        pipe.bytes.extend(&bytes[..written]);
        pipe_ok(HostValue::Int(written as i64))
    }

    fn pipe_before_read(&self, reader: u64) -> Option<(u64, VecDeque<u8>)> {
        let end = self.pipe_ends.get(&reader)?;
        (end.kind == MemoryPipeKind::Reader)
            .then(|| {
                self.pipes
                    .get(&end.pipe)
                    .map(|pipe| (end.pipe, pipe.bytes.clone()))
            })
            .flatten()
    }

    fn child_status_value(status: MemoryChildStatus) -> HostValue {
        let status = match status {
            MemoryChildStatus::Exited(code) => {
                HostValue::Ctor(CoreCtor::ChildStatusExited, vec![HostValue::Int(code)])
            }
            MemoryChildStatus::Terminated => {
                HostValue::Ctor(CoreCtor::ChildStatusTerminated, Vec::new())
            }
        };
        exec_ok(status)
    }

    fn child_wait_value(&mut self, child: u64, consume: bool) -> HostValue {
        let Some(state) = self.children.get(&child) else {
            return exec_error(CoreCtor::ExecErrorClosed, None);
        };
        let value = Self::child_status_value(state.status);
        if consume {
            self.children.remove(&child);
        }
        value
    }

    fn validate_exec_spec(&self, spec: &HostExecSpec) -> Option<HostValue> {
        if spec.program.is_empty() || spec.program.contains('\0') {
            return Some(exec_error(
                CoreCtor::ExecErrorInvalidInput,
                Some("the program name is invalid"),
            ));
        }
        if spec.program.len() > MAX_EXEC_ITEM_BYTES {
            return Some(exec_error(
                CoreCtor::ExecErrorLimitExceeded,
                Some("the program name is too large"),
            ));
        }
        if spec.arguments.len() > MAX_EXEC_ITEMS {
            return Some(exec_error(
                CoreCtor::ExecErrorLimitExceeded,
                Some("the argument count is too large"),
            ));
        }
        let mut total = spec.program.len();
        for argument in &spec.arguments {
            if argument.contains('\0') {
                return Some(exec_error(
                    CoreCtor::ExecErrorInvalidInput,
                    Some("an argument contains a zero byte"),
                ));
            }
            if argument.len() > MAX_EXEC_ITEM_BYTES {
                return Some(exec_error(
                    CoreCtor::ExecErrorLimitExceeded,
                    Some("an argument is too large"),
                ));
            }
            total = total.saturating_add(argument.len());
        }
        if let Some(directory) = &spec.directory {
            if directory.is_empty() || directory.contains('\0') {
                return Some(exec_error(
                    CoreCtor::ExecErrorInvalidInput,
                    Some("the child directory is invalid"),
                ));
            }
            if directory.len() > MAX_EXEC_ITEM_BYTES {
                return Some(exec_error(
                    CoreCtor::ExecErrorLimitExceeded,
                    Some("the child directory is too large"),
                ));
            }
            total = total.saturating_add(directory.len());
        }
        if let HostChildEnv::Exact(values) | HostChildEnv::Overlay(values) = &spec.environment {
            if values.len() > MAX_EXEC_ITEMS {
                return Some(exec_error(
                    CoreCtor::ExecErrorLimitExceeded,
                    Some("the environment entry count is too large"),
                ));
            }
            for (name, value) in values {
                if name.is_empty() || name.contains('=') || name.contains('\0') {
                    return Some(exec_error(
                        CoreCtor::ExecErrorInvalidInput,
                        Some("an environment name is invalid"),
                    ));
                }
                if value.contains('\0') {
                    return Some(exec_error(
                        CoreCtor::ExecErrorInvalidInput,
                        Some("an environment value contains a zero byte"),
                    ));
                }
                if name.len() > MAX_EXEC_ITEM_BYTES || value.len() > MAX_EXEC_ITEM_BYTES {
                    return Some(exec_error(
                        CoreCtor::ExecErrorLimitExceeded,
                        Some("an environment entry is too large"),
                    ));
                }
                total = total.saturating_add(name.len()).saturating_add(value.len());
            }
        }
        if total > MAX_EXEC_TEXT_BYTES {
            return Some(exec_error(
                CoreCtor::ExecErrorLimitExceeded,
                Some("the child specification is too large"),
            ));
        }
        let input_valid = match spec.input {
            HostChildInput::Inherit | HostChildInput::Null => true,
            HostChildInput::Pipe(token) => self
                .pipe_ends
                .get(&token)
                .is_some_and(|end| end.kind == MemoryPipeKind::Reader),
        };
        let output_valid = |output: HostChildOutput| match output {
            HostChildOutput::Inherit | HostChildOutput::Null => true,
            HostChildOutput::Pipe(token) => self
                .pipe_ends
                .get(&token)
                .is_some_and(|end| end.kind == MemoryPipeKind::Writer),
        };
        if !input_valid || !output_valid(spec.output) || !output_valid(spec.error) {
            return Some(exec_error(CoreCtor::ExecErrorClosed, None));
        }
        None
    }

    fn write_child_output(&mut self, output: HostChildOutput, bytes: &[u8], error: bool) {
        match output {
            HostChildOutput::Inherit => {
                if error {
                    self.written_error_bytes.extend_from_slice(bytes);
                } else {
                    self.written_bytes.extend_from_slice(bytes);
                }
            }
            HostChildOutput::Null => {}
            HostChildOutput::Pipe(writer) => {
                let _ = self.pipe_write_value(writer, bytes);
            }
        }
    }

    fn consume_child_pipes(&mut self, spec: &HostExecSpec) {
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
            self.close_virtual_pipe(token);
        }
    }

    fn discard_value_resources(&mut self, value: HostValue) {
        match value {
            HostValue::File(token) => {
                self.file_handles.remove(&token);
            }
            HostValue::TcpStream(token) => {
                self.close_virtual_tcp(HostTcpResource {
                    kind: HostTcpKind::Stream,
                    token,
                });
            }
            HostValue::TcpListener(token) => {
                self.close_virtual_tcp(HostTcpResource {
                    kind: HostTcpKind::Listener,
                    token,
                });
            }
            HostValue::TlsStream(token) => {
                self.close_virtual_tls(token);
            }
            HostValue::RawMode(token) => {
                if self.raw_mode == Some(token) {
                    self.raw_mode = None;
                }
            }
            HostValue::SignalStream(token) => {
                if self
                    .signal_stream
                    .as_ref()
                    .is_some_and(|stream| stream.token == token)
                {
                    self.signal_stream = None;
                }
            }
            HostValue::PipeReader(token) | HostValue::PipeWriter(token) => {
                self.close_virtual_pipe(token);
            }
            HostValue::Child(token) => {
                self.children.remove(&token);
            }
            HostValue::UdpSocket(token) => {
                self.close_virtual_udp(token);
            }
            HostValue::List(values) | HostValue::Tuple(values) | HostValue::Ctor(_, values) => {
                for value in values {
                    self.discard_value_resources(value);
                }
            }
            HostValue::Unit
            | HostValue::Bool(_)
            | HostValue::Int(_)
            | HostValue::Float(_)
            | HostValue::Str(_)
            | HostValue::Bytes(_)
            | HostValue::SocketAddress(_)
            | HostValue::Resource(_)
            | HostValue::Artifact(_)
            | HostValue::SyntaxParse { .. } => {}
        }
    }

    fn deferred_value(&mut self, token: u64) -> Option<HostValue> {
        let action = self.pending.get(&token)?.action.clone();
        match action {
            DeferredAction::Ready(value) => Some(value),
            DeferredAction::InputRead(count) => {
                let count = count.min(self.input_bytes.len());
                let bytes: Vec<u8> = self.input_bytes.drain(..count).collect();
                Some(core_ok(HostValue::Bytes(bytes.into())))
            }
            DeferredAction::Accept(listener) => self.accept_value(listener),
            DeferredAction::Read { stream, count } => self.read_value(stream, count),
            DeferredAction::TlsRead { stream, count } => self.tls_read_value(stream, count),
            DeferredAction::SignalNext { stream } => self.signal_next_value(stream),
            DeferredAction::PipeRead { reader, count } => self.pipe_read_value(reader, count),
            DeferredAction::ChildWait { child } => self
                .children
                .get(&child)
                .map(|state| Self::child_status_value(state.status))
                .or_else(|| Some(exec_error(CoreCtor::ExecErrorClosed, None))),
            DeferredAction::UdpRecv { socket } => self.udp_receive_value(socket),
        }
    }
}

fn std_stream_index(stream: HostStdStream) -> usize {
    match stream {
        HostStdStream::Input => 0,
        HostStdStream::Output => 1,
        HostStdStream::Error => 2,
    }
}

fn memory_parent(path: &str) -> &str {
    match path.rsplit_once('/') {
        Some(("", _)) => "/",
        Some((parent, _)) => parent,
        None => ".",
    }
}

fn memory_name(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, name)| name)
}

fn fs_case(ctor: CoreCtor, message: impl Into<String>) -> HostValue {
    HostValue::Ctor(ctor, vec![HostValue::Str(message.into().into())])
}

fn fs_named_error(ctor: CoreCtor, message: impl Into<String>) -> HostValue {
    HostValue::Ctor(CoreCtor::Err, vec![fs_case(ctor, message)])
}

fn memory_file_kind(ctor: CoreCtor) -> HostValue {
    HostValue::Ctor(ctor, Vec::new())
}

impl RecordingHost {
    /// Hold one reply for a later poll.
    fn defer(&mut self, key: CompletionKey, value: HostValue) -> HostStart {
        let token = self.next_token;
        self.next_token += 1;
        self.pending.insert(
            token,
            Deferred {
                key,
                left: 1,
                action: DeferredAction::Ready(value),
                wait_source: false,
                rollback: None,
            },
        );
        HostStart::Waiting(token)
    }

    /// Hold one network request until its resource becomes ready.
    fn defer_action(&mut self, key: CompletionKey, action: DeferredAction) -> HostStart {
        let token = self.next_token;
        self.next_token += 1;
        self.pending.insert(
            token,
            Deferred {
                key,
                left: 1,
                action,
                wait_source: false,
                rollback: None,
            },
        );
        HostStart::Waiting(token)
    }

    /// Hold one selectable reply for a later poll.
    fn defer_wait(
        &mut self,
        key: CompletionKey,
        action: DeferredAction,
        rollback: Option<RetainedWait>,
    ) -> HostStart {
        let token = self.next_token;
        self.next_token += 1;
        self.pending.insert(
            token,
            Deferred {
                key,
                left: 1,
                action,
                wait_source: true,
                rollback,
            },
        );
        HostStart::Waiting(token)
    }
}

impl RecordingHost {
    fn serve(&mut self, key: CompletionKey, op: u32, args: Vec<HostArg>) -> HostStart {
        let _ = key;
        self.operations.push(op);
        match op {
            lm_abi::OP_IO_READ_BYTES => {
                let Some(HostArg::Int(count)) = args.first() else {
                    return HostStart::Failed("Io.ReadBytes needs one integer".to_string());
                };
                let Ok(count) = usize::try_from(*count) else {
                    return HostStart::Completed(core_error(
                        CoreCtor::IoErrorInvalidInput,
                        Some("the read count is negative"),
                    ));
                };
                if count > MAX_CONSOLE_IO_BYTES {
                    return HostStart::Completed(core_error(
                        CoreCtor::IoErrorLimitExceeded,
                        Some("the read count is too large"),
                    ));
                }
                let count = count.min(self.input_bytes.len());
                let bytes: Vec<u8> = self.input_bytes.drain(..count).collect();
                HostStart::Completed(core_ok(HostValue::Bytes(bytes.into())))
            }
            lm_abi::OP_IO_WRITE => {
                let Some(HostArg::Bytes(bytes)) = args.first() else {
                    return HostStart::Failed("Io.Write needs one byte value".to_string());
                };
                if bytes.len() > MAX_CONSOLE_IO_BYTES {
                    return HostStart::Completed(core_error(
                        CoreCtor::IoErrorLimitExceeded,
                        Some("the write value is too large"),
                    ));
                }
                let written = bytes.len().min(self.console_write_limit);
                self.written_bytes.extend_from_slice(&bytes[..written]);
                HostStart::Completed(core_ok(HostValue::Int(written as i64)))
            }
            lm_abi::OP_IO_WRITE_ERROR => {
                let Some(HostArg::Bytes(bytes)) = args.first() else {
                    return HostStart::Failed("Io.WriteError needs one byte value".to_string());
                };
                if bytes.len() > MAX_CONSOLE_IO_BYTES {
                    return HostStart::Completed(core_error(
                        CoreCtor::IoErrorLimitExceeded,
                        Some("the write value is too large"),
                    ));
                }
                let written = bytes.len().min(self.console_write_limit);
                self.written_error_bytes
                    .extend_from_slice(&bytes[..written]);
                HostStart::Completed(core_ok(HostValue::Int(written as i64)))
            }
            lm_abi::OP_ENV_GET => {
                let Some(HostArg::Str(name)) = args.first() else {
                    return HostStart::Failed("Env.Get needs one string".to_string());
                };
                if name.is_empty() || name.contains('=') || name.contains('\0') {
                    return HostStart::Completed(core_error(
                        CoreCtor::EnvInvalidName,
                        Some("the environment name is invalid"),
                    ));
                }
                let value = self
                    .environment
                    .get(name.as_str())
                    .map(|value| {
                        HostValue::Ctor(CoreCtor::Some, vec![HostValue::Str(value.clone().into())])
                    })
                    .unwrap_or_else(|| HostValue::Ctor(CoreCtor::None, vec![]));
                HostStart::Completed(core_ok(value))
            }
            lm_abi::OP_FS_CURRENT_DIR => {
                HostStart::Completed(core_ok(HostValue::Str(self.current_dir.clone().into())))
            }
            lm_abi::OP_ARGS_GET => HostStart::Completed(HostValue::List(
                self.arguments
                    .iter()
                    .cloned()
                    .map(|value| HostValue::Str(value.into()))
                    .collect(),
            )),
            lm_abi::OP_ENTROPY_BYTES => {
                let Some(HostArg::Int(count)) = args.first() else {
                    return HostStart::Failed("Entropy.Bytes needs one integer".to_string());
                };
                let Ok(count) = usize::try_from(*count) else {
                    return HostStart::Completed(core_error(
                        CoreCtor::EntropyInvalidInput,
                        Some("the entropy count is negative"),
                    ));
                };
                if count > MAX_ENTROPY_BYTES {
                    return HostStart::Completed(core_error(
                        CoreCtor::EntropyLimitExceeded,
                        Some("the entropy count is too large"),
                    ));
                }
                let bytes: Vec<u8> = (0..count)
                    .map(|index| (index as u8).wrapping_mul(73).wrapping_add(41))
                    .collect();
                HostStart::Completed(core_ok(HostValue::Bytes(bytes.into())))
            }
            lm_abi::OP_CLOCK_NOW => {
                self.now += 1;
                HostStart::Completed(HostValue::Int(self.now))
            }
            lm_abi::OP_CLOCK_MONOTONIC => {
                self.monotonic += 1;
                HostStart::Completed(HostValue::Int(self.monotonic))
            }
            lm_abi::OP_CLOCK_SLEEP => self.defer(key, HostValue::Unit),
            lm_abi::OP_RAND_INT => {
                let (low, high) = match (args.first(), args.get(1)) {
                    (Some(HostArg::Int(low)), Some(HostArg::Int(high))) => (*low, *high),
                    _ => return HostStart::Failed("Rand.Int needs two integers".to_string()),
                };
                if low >= high {
                    return HostStart::Failed("Rand.Int needs low < high".to_string());
                }
                let span = (high - low) as u64;
                let value = low + (self.next_rand() % span) as i64;
                HostStart::Completed(HostValue::Int(value))
            }
            lm_abi::OP_FS_OPEN => {
                let (Some(HostArg::Str(path)), Some(HostArg::OpenOptions(options))) =
                    (args.first(), args.get(1))
                else {
                    return HostStart::Failed("Fs.Open needs a path and options".to_string());
                };
                let (readable, writable, append, create, truncate, exclusive) = match options {
                    HostOpenOptions::ReadOnly => (true, false, false, false, false, false),
                    HostOpenOptions::WriteOnly => (false, true, false, false, false, false),
                    HostOpenOptions::ReadWrite => (true, true, false, false, false, false),
                    HostOpenOptions::Create => (true, true, false, true, false, false),
                    HostOpenOptions::CreateTruncate => (true, true, false, true, true, false),
                    HostOpenOptions::CreateNew => (true, true, false, true, false, true),
                    HostOpenOptions::Append => (false, true, true, true, false, false),
                };
                if self.directories.contains(path.as_str()) {
                    return HostStart::Completed(fs_named_error(
                        CoreCtor::FsErrorIsDirectory,
                        "file open found a directory",
                    ));
                }
                if exclusive && self.files.contains_key(path.as_str()) {
                    return HostStart::Completed(fs_named_error(
                        CoreCtor::FsErrorAlreadyExists,
                        "file open found an existing path",
                    ));
                }
                if !create && !self.files.contains_key(path.as_str()) {
                    return HostStart::Completed(fs_named_error(
                        CoreCtor::FsErrorNotFound,
                        "file open did not find the path",
                    ));
                }
                if create && !self.directories.contains(memory_parent(path)) {
                    return HostStart::Completed(fs_named_error(
                        CoreCtor::FsErrorNotFound,
                        "file open did not find the parent directory",
                    ));
                }
                let path = path.to_string();
                let file = self.files.entry(path.clone()).or_default();
                if truncate {
                    file.clear();
                }
                let cursor = if append { file.len() } else { 0 };
                let token = self.next_file;
                let Some(next) = token.checked_add(1) else {
                    return HostStart::Failed("the file token space is exhausted".to_string());
                };
                self.next_file = next;
                self.file_handles.insert(
                    token,
                    MemoryFile {
                        path: path.clone(),
                        cursor,
                        readable,
                        writable,
                        append,
                    },
                );
                HostStart::Completed(fs_ok(HostValue::File(token)))
            }
            lm_abi::OP_FS_READ => {
                let (Some(HostArg::File(token)), Some(HostArg::Int(count))) =
                    (args.first(), args.get(1))
                else {
                    return HostStart::Failed("Fs.Read needs a file and count".to_string());
                };
                let Ok(count) = usize::try_from(*count) else {
                    return HostStart::Completed(fs_named_error(
                        CoreCtor::FsErrorInvalidInput,
                        "the read count is negative",
                    ));
                };
                if count > MAX_FILE_IO_BYTES {
                    return HostStart::Completed(fs_named_error(
                        CoreCtor::FsErrorLimitExceeded,
                        "the read count is too large",
                    ));
                }
                let (files, handles) = (&self.files, &mut self.file_handles);
                let Some(handle) = handles.get_mut(token) else {
                    return HostStart::Completed(fs_closed());
                };
                if !handle.readable {
                    return HostStart::Completed(fs_named_error(
                        CoreCtor::FsErrorPermissionDenied,
                        "the file is not readable",
                    ));
                }
                let Some(file) = files.get(&handle.path) else {
                    return HostStart::Completed(fs_named_error(
                        CoreCtor::FsErrorNotFound,
                        "the file does not exist",
                    ));
                };
                let end = handle.cursor.saturating_add(count).min(file.len());
                let bytes = file[handle.cursor..end].to_vec();
                handle.cursor = end;
                HostStart::Completed(fs_ok(HostValue::Bytes(bytes.into())))
            }
            lm_abi::OP_FS_WRITE => {
                let (Some(HostArg::File(token)), Some(HostArg::Bytes(bytes))) =
                    (args.first(), args.get(1))
                else {
                    return HostStart::Failed("Fs.Write needs a file and bytes".to_string());
                };
                let (files, handles) = (&mut self.files, &mut self.file_handles);
                let Some(handle) = handles.get_mut(token) else {
                    return HostStart::Completed(fs_closed());
                };
                if !handle.writable {
                    return HostStart::Completed(fs_named_error(
                        CoreCtor::FsErrorPermissionDenied,
                        "the file is not writable",
                    ));
                }
                let Some(file) = files.get_mut(&handle.path) else {
                    return HostStart::Completed(fs_named_error(
                        CoreCtor::FsErrorNotFound,
                        "the file does not exist",
                    ));
                };
                if handle.append {
                    handle.cursor = file.len();
                }
                let Some(end) = handle.cursor.checked_add(bytes.len()) else {
                    return HostStart::Completed(fs_named_error(
                        CoreCtor::FsErrorLimitExceeded,
                        "the write position is too large",
                    ));
                };
                if end > file.len() {
                    file.resize(end, 0);
                }
                file[handle.cursor..end].copy_from_slice(bytes);
                handle.cursor = end;
                HostStart::Completed(fs_ok(HostValue::Int(bytes.len() as i64)))
            }
            lm_abi::OP_FS_SEEK => {
                let (Some(HostArg::File(token)), Some(HostArg::SeekFrom(from))) =
                    (args.first(), args.get(1))
                else {
                    return HostStart::Failed("Fs.Seek needs a file and origin".to_string());
                };
                let (files, handles) = (&self.files, &mut self.file_handles);
                let Some(handle) = handles.get_mut(token) else {
                    return HostStart::Completed(fs_closed());
                };
                let Some(file) = files.get(&handle.path) else {
                    return HostStart::Completed(fs_named_error(
                        CoreCtor::FsErrorNotFound,
                        "the file does not exist",
                    ));
                };
                let base = match from {
                    HostSeekFrom::Start(_) => 0i128,
                    HostSeekFrom::Current(_) => handle.cursor as i128,
                    HostSeekFrom::End(_) => file.len() as i128,
                };
                let offset = match from {
                    HostSeekFrom::Start(offset)
                    | HostSeekFrom::Current(offset)
                    | HostSeekFrom::End(offset) => i128::from(*offset),
                };
                let position = base + offset;
                let Ok(position) = usize::try_from(position) else {
                    return HostStart::Completed(fs_named_error(
                        CoreCtor::FsErrorInvalidInput,
                        "the seek position is invalid",
                    ));
                };
                handle.cursor = position;
                HostStart::Completed(fs_ok(HostValue::Int(position as i64)))
            }
            lm_abi::OP_FS_FLUSH => {
                let Some(HostArg::File(token)) = args.first() else {
                    return HostStart::Failed("Fs.Flush needs a file".to_string());
                };
                if !self.file_handles.contains_key(token) {
                    return HostStart::Completed(fs_closed());
                }
                HostStart::Completed(fs_ok(HostValue::Unit))
            }
            lm_abi::OP_FS_SYNC => {
                let Some(HostArg::File(token)) = args.first() else {
                    return HostStart::Failed("Fs.Sync needs a file".to_string());
                };
                if !self.file_handles.contains_key(token) {
                    return HostStart::Completed(HostValue::Ctor(
                        CoreCtor::Err,
                        vec![HostValue::Ctor(CoreCtor::FsErrorClosed, Vec::new())],
                    ));
                }
                HostStart::Completed(fs_ok(HostValue::Unit))
            }
            lm_abi::OP_FS_CLOSE => {
                let Some(HostArg::File(token)) = args.first() else {
                    return HostStart::Failed("Fs.Close needs a file".to_string());
                };
                if self.file_handles.remove(token).is_none() {
                    return HostStart::Completed(fs_closed());
                }
                HostStart::Completed(fs_ok(HostValue::Unit))
            }
            lm_abi::OP_FS_STAT => {
                let Some(HostArg::Str(path)) = args.first() else {
                    return HostStart::Failed("Fs.Stat needs a path".to_string());
                };
                let kind = if let Some(bytes) = self.files.get(path.as_str()) {
                    Some((CoreCtor::FileKindFile, bytes.len()))
                } else if self.directories.contains(path.as_str()) {
                    Some((CoreCtor::FileKindDirectory, 0))
                } else {
                    None
                };
                let Some((kind, length)) = kind else {
                    return HostStart::Completed(fs_named_error(
                        CoreCtor::FsErrorNotFound,
                        "file metadata query did not find the path",
                    ));
                };
                let Ok(length) = i64::try_from(length) else {
                    return HostStart::Completed(fs_named_error(
                        CoreCtor::FsErrorLimitExceeded,
                        "the file length exceeds Int",
                    ));
                };
                HostStart::Completed(fs_ok(HostValue::Ctor(
                    CoreCtor::FileInfo,
                    vec![
                        memory_file_kind(kind),
                        HostValue::Int(length),
                        HostValue::Ctor(CoreCtor::None, Vec::new()),
                        HostValue::Bool(false),
                    ],
                )))
            }
            lm_abi::OP_FS_READ_DIR => {
                let (Some(HostArg::Str(path)), Some(HostArg::Int(max_entries))) =
                    (args.first(), args.get(1))
                else {
                    return HostStart::Failed("Fs.ReadDir needs a path and limit".to_string());
                };
                let Ok(max_entries) = usize::try_from(*max_entries) else {
                    return HostStart::Completed(fs_named_error(
                        CoreCtor::FsErrorInvalidInput,
                        "the directory entry limit is negative",
                    ));
                };
                if max_entries > 100_000 {
                    return HostStart::Completed(fs_named_error(
                        CoreCtor::FsErrorLimitExceeded,
                        "the directory entry limit is too large",
                    ));
                }
                if !self.directories.contains(path.as_str()) {
                    let ctor = if self.files.contains_key(path.as_str()) {
                        CoreCtor::FsErrorNotDirectory
                    } else {
                        CoreCtor::FsErrorNotFound
                    };
                    return HostStart::Completed(fs_named_error(
                        ctor,
                        "directory read did not find a directory",
                    ));
                }
                let mut entries = BTreeMap::<String, CoreCtor>::new();
                for name in self.files.keys() {
                    if memory_parent(name) == path.as_str() {
                        entries.insert(memory_name(name).to_string(), CoreCtor::FileKindFile);
                    }
                }
                for name in &self.directories {
                    if name != path.as_str() && memory_parent(name) == path.as_str() {
                        entries.insert(memory_name(name).to_string(), CoreCtor::FileKindDirectory);
                    }
                }
                if entries.len() > max_entries {
                    return HostStart::Completed(fs_named_error(
                        CoreCtor::FsErrorLimitExceeded,
                        "the directory exceeds its entry limit",
                    ));
                }
                let entries = entries
                    .into_iter()
                    .map(|(name, kind)| {
                        HostValue::Ctor(
                            CoreCtor::Ok,
                            vec![HostValue::Ctor(
                                CoreCtor::DirEntry,
                                vec![HostValue::Str(name.into()), memory_file_kind(kind)],
                            )],
                        )
                    })
                    .collect();
                HostStart::Completed(fs_ok(HostValue::List(entries)))
            }
            lm_abi::OP_FS_CREATE_DIR => {
                let Some(HostArg::Str(path)) = args.first() else {
                    return HostStart::Failed("Fs.CreateDir needs a path".to_string());
                };
                if self.files.contains_key(path.as_str())
                    || self.directories.contains(path.as_str())
                {
                    return HostStart::Completed(fs_named_error(
                        CoreCtor::FsErrorAlreadyExists,
                        "directory creation found an existing path",
                    ));
                }
                if !self.directories.contains(memory_parent(path)) {
                    return HostStart::Completed(fs_named_error(
                        CoreCtor::FsErrorNotFound,
                        "directory creation did not find its parent",
                    ));
                }
                self.directories.insert(path.to_string());
                HostStart::Completed(fs_ok(HostValue::Unit))
            }
            lm_abi::OP_FS_REMOVE_FILE => {
                let Some(HostArg::Str(path)) = args.first() else {
                    return HostStart::Failed("Fs.RemoveFile needs a path".to_string());
                };
                if self.directories.contains(path.as_str()) {
                    return HostStart::Completed(fs_named_error(
                        CoreCtor::FsErrorIsDirectory,
                        "file removal found a directory",
                    ));
                }
                if self.files.remove(path.as_str()).is_none() {
                    return HostStart::Completed(fs_named_error(
                        CoreCtor::FsErrorNotFound,
                        "file removal did not find the path",
                    ));
                }
                HostStart::Completed(fs_ok(HostValue::Unit))
            }
            lm_abi::OP_FS_REMOVE_DIR => {
                let Some(HostArg::Str(path)) = args.first() else {
                    return HostStart::Failed("Fs.RemoveDir needs a path".to_string());
                };
                if self.files.contains_key(path.as_str()) {
                    return HostStart::Completed(fs_named_error(
                        CoreCtor::FsErrorNotDirectory,
                        "directory removal found a file",
                    ));
                }
                if !self.directories.contains(path.as_str()) {
                    return HostStart::Completed(fs_named_error(
                        CoreCtor::FsErrorNotFound,
                        "directory removal did not find the path",
                    ));
                }
                let nonempty = self
                    .files
                    .keys()
                    .chain(self.directories.iter())
                    .any(|name| name != path.as_str() && memory_parent(name) == path.as_str());
                if nonempty {
                    return HostStart::Completed(fs_named_error(
                        CoreCtor::FsErrorDirectoryNotEmpty,
                        "directory removal found a nonempty directory",
                    ));
                }
                self.directories.remove(path.as_str());
                HostStart::Completed(fs_ok(HostValue::Unit))
            }
            lm_abi::OP_FS_RENAME => {
                let (
                    Some(HostArg::Str(from)),
                    Some(HostArg::Str(to)),
                    Some(HostArg::RenameMode(mode)),
                ) = (args.first(), args.get(1), args.get(2))
                else {
                    return HostStart::Failed("Fs.Rename needs two paths and one mode".to_string());
                };
                let target_exists =
                    self.files.contains_key(to.as_str()) || self.directories.contains(to.as_str());
                if !self.directories.contains(memory_parent(to)) {
                    return HostStart::Completed(fs_named_error(
                        CoreCtor::FsErrorNotFound,
                        "rename did not find the target parent",
                    ));
                }
                if target_exists && matches!(mode, HostRenameMode::NoReplace) {
                    return HostStart::Completed(fs_named_error(
                        CoreCtor::FsErrorAlreadyExists,
                        "rename found an existing target",
                    ));
                }
                if let Some(bytes) = self.files.remove(from.as_str()) {
                    if self.directories.contains(to.as_str()) {
                        self.files.insert(from.to_string(), bytes);
                        return HostStart::Completed(fs_named_error(
                            CoreCtor::FsErrorIsDirectory,
                            "rename found a target directory",
                        ));
                    }
                    self.files.insert(to.to_string(), bytes);
                    for handle in self.file_handles.values_mut() {
                        if handle.path == from.as_str() {
                            handle.path = to.to_string();
                        }
                    }
                    return HostStart::Completed(fs_ok(HostValue::Unit));
                }
                if self.directories.contains(from.as_str()) {
                    if target_exists {
                        return HostStart::Completed(fs_named_error(
                            CoreCtor::FsErrorAlreadyExists,
                            "directory rename found an existing target",
                        ));
                    }
                    let from_prefix = format!("{from}/");
                    let to_prefix = format!("{to}/");
                    let moved_directories: Vec<String> = self
                        .directories
                        .iter()
                        .filter(|path| {
                            path.as_str() == from.as_str() || path.starts_with(&from_prefix)
                        })
                        .cloned()
                        .collect();
                    let moved_files: Vec<String> = self
                        .files
                        .keys()
                        .filter(|path| path.starts_with(&from_prefix))
                        .cloned()
                        .collect();
                    for path in &moved_directories {
                        self.directories.remove(path);
                    }
                    for path in moved_directories {
                        let moved = if path == from.as_str() {
                            to.to_string()
                        } else {
                            format!("{to_prefix}{}", &path[from_prefix.len()..])
                        };
                        self.directories.insert(moved);
                    }
                    for path in moved_files {
                        let bytes = self
                            .files
                            .remove(&path)
                            .expect("the moved file remains present");
                        let moved = format!("{to_prefix}{}", &path[from_prefix.len()..]);
                        self.files.insert(moved, bytes);
                    }
                    for handle in self.file_handles.values_mut() {
                        if handle.path.starts_with(&from_prefix) {
                            handle.path =
                                format!("{to_prefix}{}", &handle.path[from_prefix.len()..]);
                        }
                    }
                    return HostStart::Completed(fs_ok(HostValue::Unit));
                }
                HostStart::Completed(fs_named_error(
                    CoreCtor::FsErrorNotFound,
                    "rename did not find the source",
                ))
            }
            lm_abi::OP_FS_SYNC_DIR => {
                let Some(HostArg::Str(path)) = args.first() else {
                    return HostStart::Failed("Fs.SyncDir needs a path".to_string());
                };
                if !self.directories.contains(path.as_str()) {
                    let ctor = if self.files.contains_key(path.as_str()) {
                        CoreCtor::FsErrorNotDirectory
                    } else {
                        CoreCtor::FsErrorNotFound
                    };
                    return HostStart::Completed(fs_named_error(
                        ctor,
                        "directory sync did not find a directory",
                    ));
                }
                HostStart::Completed(fs_ok(HostValue::Unit))
            }
            lm_abi::OP_DNS_RESOLVE => {
                let (Some(HostArg::Str(name)), Some(HostArg::Int(port))) =
                    (args.first(), args.get(1))
                else {
                    return HostStart::Failed("Dns.Resolve needs a name and port".to_string());
                };
                if name.is_empty()
                    || name.len() > 253
                    || name
                        .as_bytes()
                        .iter()
                        .any(|byte| *byte <= 32 || *byte >= 127)
                {
                    return HostStart::Completed(net_error(
                        CoreCtor::NetInvalidInput,
                        "the host name is invalid",
                    ));
                }
                let Ok(port) = u16::try_from(*port) else {
                    return HostStart::Completed(net_error(
                        CoreCtor::NetInvalidInput,
                        "the port is outside 0 through 65535",
                    ));
                };
                let Some(addresses) = self.dns.get(name.as_str()) else {
                    return HostStart::Completed(net_error(
                        CoreCtor::NetNameNotFound,
                        "the host name has no address",
                    ));
                };
                if addresses.is_empty() {
                    return HostStart::Completed(net_error(
                        CoreCtor::NetNameNotFound,
                        "the host name has no address",
                    ));
                }
                let values = addresses
                    .iter()
                    .take(MAX_DNS_RESULTS)
                    .map(|ip| {
                        HostValue::SocketAddress(HostSocketAddress {
                            ip: *ip,
                            port,
                            flow_info: 0,
                            scope_id: 0,
                        })
                    })
                    .collect();
                HostStart::Completed(net_ok(HostValue::List(values)))
            }
            lm_abi::OP_TCP_LISTEN => {
                let (Some(HostArg::SocketAddress(address)), Some(HostArg::Int(backlog))) =
                    (args.first(), args.get(1))
                else {
                    return HostStart::Failed(
                        "Tcp.Listen needs an address and backlog".to_string(),
                    );
                };
                let Ok(backlog) = usize::try_from(*backlog) else {
                    return HostStart::Completed(net_error(
                        CoreCtor::NetInvalidInput,
                        "the backlog is not positive",
                    ));
                };
                if backlog == 0 || backlog > 65_535 {
                    return HostStart::Completed(net_error(
                        CoreCtor::NetInvalidInput,
                        "the backlog is outside 1 through 65535",
                    ));
                }
                let mut address = *address;
                if address.port == 0 {
                    let Some(port) = self.take_port() else {
                        return HostStart::Completed(net_error(
                            CoreCtor::NetLimitExceeded,
                            "the virtual port space is exhausted",
                        ));
                    };
                    address.port = port;
                }
                if self.listener_addresses.contains_key(&address) {
                    return HostStart::Completed(net_error(
                        CoreCtor::NetAddressInUse,
                        "the address already has a listener",
                    ));
                }
                let Some(token) = self.take_tcp_token() else {
                    return HostStart::Completed(net_error(
                        CoreCtor::NetLimitExceeded,
                        "the virtual TCP token space is exhausted",
                    ));
                };
                self.listeners.insert(
                    token,
                    MemoryListener {
                        address,
                        backlog,
                        incoming: VecDeque::new(),
                    },
                );
                self.listener_addresses.insert(address, token);
                HostStart::Completed(net_ok(HostValue::TcpListener(token)))
            }
            lm_abi::OP_TCP_CONNECT => {
                let Some(HostArg::SocketAddress(address)) = args.first() else {
                    return HostStart::Failed("Tcp.Connect needs an address".to_string());
                };
                let Some(listener_token) = self.listener_addresses.get(address).copied() else {
                    return HostStart::Completed(net_error(
                        CoreCtor::NetConnectionRefused,
                        "the address has no listener",
                    ));
                };
                let full = self
                    .listeners
                    .get(&listener_token)
                    .is_none_or(|listener| listener.incoming.len() >= listener.backlog);
                if full {
                    return HostStart::Completed(net_error(
                        CoreCtor::NetUnavailable,
                        "the listener backlog is full",
                    ));
                }
                let Some(client) = self.take_tcp_token() else {
                    return HostStart::Completed(net_error(
                        CoreCtor::NetLimitExceeded,
                        "the virtual TCP token space is exhausted",
                    ));
                };
                let Some(server) = self.take_tcp_token() else {
                    return HostStart::Completed(net_error(
                        CoreCtor::NetLimitExceeded,
                        "the virtual TCP token space is exhausted",
                    ));
                };
                let Some(port) = self.take_port() else {
                    return HostStart::Completed(net_error(
                        CoreCtor::NetLimitExceeded,
                        "the virtual port space is exhausted",
                    ));
                };
                let local = HostSocketAddress {
                    ip: address.ip,
                    port,
                    flow_info: 0,
                    scope_id: 0,
                };
                self.streams.insert(
                    client,
                    MemoryStream {
                        local,
                        peer_address: *address,
                        peer: Some(server),
                        incoming: VecDeque::new(),
                        read_closed: false,
                        write_closed: false,
                        peer_write_closed: false,
                    },
                );
                self.streams.insert(
                    server,
                    MemoryStream {
                        local: *address,
                        peer_address: local,
                        peer: Some(client),
                        incoming: VecDeque::new(),
                        read_closed: false,
                        write_closed: false,
                        peer_write_closed: false,
                    },
                );
                self.listeners
                    .get_mut(&listener_token)
                    .expect("the listener remains open")
                    .incoming
                    .push_back((server, local));
                HostStart::Completed(net_ok(HostValue::TcpStream(client)))
            }
            lm_abi::OP_TCP_ACCEPT => {
                let Some(HostArg::Tcp(resource)) = args.first() else {
                    return HostStart::Failed("Tcp.Accept needs a listener".to_string());
                };
                if resource.kind != HostTcpKind::Listener {
                    return HostStart::Failed("Tcp.Accept needs a listener".to_string());
                }
                match self.accept_value(resource.token) {
                    Some(value) => HostStart::Completed(value),
                    None => self.defer_action(key, DeferredAction::Accept(resource.token)),
                }
            }
            lm_abi::OP_TCP_READ => {
                let (Some(HostArg::Tcp(resource)), Some(HostArg::Int(count))) =
                    (args.first(), args.get(1))
                else {
                    return HostStart::Failed("Tcp.Read needs a stream and count".to_string());
                };
                if resource.kind != HostTcpKind::Stream {
                    return HostStart::Failed("Tcp.Read needs a stream".to_string());
                }
                let Ok(count) = usize::try_from(*count) else {
                    return HostStart::Completed(net_error(
                        CoreCtor::NetInvalidInput,
                        "the read count is not positive",
                    ));
                };
                if count == 0 {
                    return HostStart::Completed(net_error(
                        CoreCtor::NetInvalidInput,
                        "the read count is not positive",
                    ));
                }
                if count > MAX_NETWORK_IO_BYTES {
                    return HostStart::Completed(net_error(
                        CoreCtor::NetLimitExceeded,
                        "the read count is too large",
                    ));
                }
                match self.read_value(resource.token, count) {
                    Some(value) => HostStart::Completed(value),
                    None => self.defer_action(
                        key,
                        DeferredAction::Read {
                            stream: resource.token,
                            count,
                        },
                    ),
                }
            }
            lm_abi::OP_TCP_WRITE => {
                let (Some(HostArg::Tcp(resource)), Some(HostArg::Bytes(bytes))) =
                    (args.first(), args.get(1))
                else {
                    return HostStart::Failed("Tcp.Write needs a stream and bytes".to_string());
                };
                if resource.kind != HostTcpKind::Stream {
                    return HostStart::Failed("Tcp.Write needs a stream".to_string());
                }
                let Some(stream) = self.streams.get(&resource.token) else {
                    return HostStart::Completed(net_closed());
                };
                if stream.write_closed {
                    return HostStart::Completed(net_error(
                        CoreCtor::NetNotConnected,
                        "the stream write side is closed",
                    ));
                }
                if bytes.len() > MAX_NETWORK_IO_BYTES {
                    return HostStart::Completed(net_error(
                        CoreCtor::NetLimitExceeded,
                        "the write value is too large",
                    ));
                }
                if bytes.is_empty() {
                    return HostStart::Completed(net_ok(HostValue::Int(0)));
                }
                let Some(peer) = stream.peer else {
                    return HostStart::Completed(net_error(
                        CoreCtor::NetConnectionReset,
                        "the peer closed the stream",
                    ));
                };
                let count = bytes.len().min(VIRTUAL_WRITE_CHUNK);
                let Some(peer) = self.streams.get_mut(&peer) else {
                    return HostStart::Completed(net_error(
                        CoreCtor::NetConnectionReset,
                        "the peer closed the stream",
                    ));
                };
                if peer.read_closed {
                    return HostStart::Completed(net_error(
                        CoreCtor::NetConnectionReset,
                        "the peer closed its read side",
                    ));
                }
                if peer.incoming.len().saturating_add(count) > MAX_VIRTUAL_STREAM_BYTES {
                    return HostStart::Completed(net_error(
                        CoreCtor::NetLimitExceeded,
                        "the virtual stream buffer is full",
                    ));
                }
                peer.incoming
                    .extend(bytes.as_slice()[..count].iter().copied());
                HostStart::Completed(net_ok(HostValue::Int(count as i64)))
            }
            lm_abi::OP_TCP_SHUTDOWN => {
                let (Some(HostArg::Tcp(resource)), Some(HostArg::Shutdown(direction))) =
                    (args.first(), args.get(1))
                else {
                    return HostStart::Failed(
                        "Tcp.Shutdown needs a stream and direction".to_string(),
                    );
                };
                if resource.kind != HostTcpKind::Stream {
                    return HostStart::Failed("Tcp.Shutdown needs a stream".to_string());
                }
                let Some(stream) = self.streams.get_mut(&resource.token) else {
                    return HostStart::Completed(net_closed());
                };
                let peer = stream.peer;
                if matches!(direction, HostShutdown::Read | HostShutdown::Both) {
                    stream.read_closed = true;
                    stream.incoming.clear();
                }
                if matches!(direction, HostShutdown::Write | HostShutdown::Both) {
                    stream.write_closed = true;
                    if let Some(peer) = peer.and_then(|peer| self.streams.get_mut(&peer)) {
                        peer.peer_write_closed = true;
                    }
                }
                HostStart::Completed(net_ok(HostValue::Unit))
            }
            lm_abi::OP_TCP_LOCAL_ADDRESS => {
                let Some(HostArg::Tcp(resource)) = args.first() else {
                    return HostStart::Failed("Tcp.LocalAddress needs a resource".to_string());
                };
                let address = match resource.kind {
                    HostTcpKind::Stream => {
                        self.streams.get(&resource.token).map(|stream| stream.local)
                    }
                    HostTcpKind::Listener => self
                        .listeners
                        .get(&resource.token)
                        .map(|listener| listener.address),
                };
                match address {
                    Some(address) => {
                        HostStart::Completed(net_ok(HostValue::SocketAddress(address)))
                    }
                    None => HostStart::Completed(net_closed()),
                }
            }
            lm_abi::OP_TCP_PEER_ADDRESS => {
                let Some(HostArg::Tcp(resource)) = args.first() else {
                    return HostStart::Failed("Tcp.PeerAddress needs a stream".to_string());
                };
                if resource.kind != HostTcpKind::Stream {
                    return HostStart::Failed("Tcp.PeerAddress needs a stream".to_string());
                }
                match self.streams.get(&resource.token) {
                    Some(stream) => {
                        HostStart::Completed(net_ok(HostValue::SocketAddress(stream.peer_address)))
                    }
                    None => HostStart::Completed(net_closed()),
                }
            }
            lm_abi::OP_TCP_CLOSE => {
                let Some(HostArg::Tcp(resource)) = args.first() else {
                    return HostStart::Failed("Tcp.Close needs a resource".to_string());
                };
                if self.close_virtual_tcp(*resource) {
                    HostStart::Completed(net_ok(HostValue::Unit))
                } else {
                    HostStart::Completed(net_closed())
                }
            }
            lm_abi::OP_TLS_HANDSHAKE => {
                let (
                    Some(HostArg::Tcp(resource)),
                    Some(HostArg::Str(server_name)),
                    Some(HostArg::Int(root_mode)),
                    Some(HostArg::List(roots)),
                    Some(HostArg::List(alpn)),
                    Some(HostArg::Int(minimum)),
                    Some(HostArg::Int(buffer_limit)),
                ) = (
                    args.first(),
                    args.get(1),
                    args.get(2),
                    args.get(3),
                    args.get(4),
                    args.get(5),
                    args.get(6),
                )
                else {
                    return HostStart::Failed("Tls.Handshake needs its configuration".to_string());
                };
                if resource.kind != HostTcpKind::Stream {
                    return HostStart::Failed("Tls.Handshake needs a TCP stream".to_string());
                }
                if !self.streams.contains_key(&resource.token)
                    || self.tls_streams.contains(&resource.token)
                {
                    return HostStart::Completed(tls_closed());
                }
                let roots_valid = roots
                    .iter()
                    .all(|value| matches!(value, HostArg::Bytes(bytes) if !bytes.is_empty() && bytes.len() <= 1_048_576));
                let alpn_valid = alpn.iter().all(
                    |value| matches!(value, HostArg::Bytes(bytes) if !bytes.is_empty() && bytes.len() <= 255),
                );
                let root_bytes = roots.iter().fold(0_usize, |total, value| {
                    total.saturating_add(match value {
                        HostArg::Bytes(bytes) => bytes.len(),
                        _ => 0,
                    })
                });
                let alpn_bytes = alpn.iter().fold(0_usize, |total, value| {
                    total.saturating_add(match value {
                        HostArg::Bytes(bytes) => bytes.len(),
                        _ => 0,
                    })
                });
                let valid = !server_name.is_empty()
                    && server_name.len() <= 253
                    && server_name
                        .as_bytes()
                        .iter()
                        .all(|byte| *byte > 32 && *byte < 127)
                    && matches!(root_mode, 0 | 1)
                    && ((*root_mode == 0 && roots.is_empty())
                        || (*root_mode == 1 && !roots.is_empty() && roots.len() <= 128))
                    && roots_valid
                    && root_bytes <= 4_194_304
                    && alpn.len() <= 32
                    && alpn_valid
                    && alpn_bytes <= 4_096
                    && matches!(minimum, 12 | 13)
                    && (1..=1_048_576).contains(buffer_limit);
                if !valid {
                    self.close_virtual_tcp(*resource);
                    return HostStart::Completed(tls_error(
                        CoreCtor::TlsInvalidConfig,
                        "the TLS client configuration is invalid",
                    ));
                }
                self.tls_streams.insert(resource.token);
                HostStart::Completed(tls_ok(HostValue::TlsStream(resource.token)))
            }
            lm_abi::OP_TLS_SERVER_HANDSHAKE => {
                let (
                    Some(HostArg::Tcp(resource)),
                    Some(HostArg::List(certificates)),
                    Some(HostArg::Bytes(private_key)),
                    Some(HostArg::List(alpn)),
                    Some(HostArg::Int(minimum)),
                    Some(HostArg::Int(buffer_limit)),
                ) = (
                    args.first(),
                    args.get(1),
                    args.get(2),
                    args.get(3),
                    args.get(4),
                    args.get(5),
                )
                else {
                    return HostStart::Failed(
                        "Tls.ServerHandshake needs its configuration".to_string(),
                    );
                };
                if resource.kind != HostTcpKind::Stream {
                    return HostStart::Failed("Tls.ServerHandshake needs a TCP stream".to_string());
                }
                if !self.streams.contains_key(&resource.token)
                    || self.tls_streams.contains(&resource.token)
                {
                    return HostStart::Completed(tls_closed());
                }
                let certificates_valid = certificates.iter().all(
                    |value| matches!(value, HostArg::Bytes(bytes) if !bytes.is_empty() && bytes.len() <= 1_048_576),
                );
                let alpn_valid = alpn.iter().all(
                    |value| matches!(value, HostArg::Bytes(bytes) if !bytes.is_empty() && bytes.len() <= 255),
                );
                let certificate_bytes = certificates.iter().fold(0_usize, |total, value| {
                    total.saturating_add(match value {
                        HostArg::Bytes(bytes) => bytes.len(),
                        _ => 0,
                    })
                });
                let alpn_bytes = alpn.iter().fold(0_usize, |total, value| {
                    total.saturating_add(match value {
                        HostArg::Bytes(bytes) => bytes.len(),
                        _ => 0,
                    })
                });
                let valid = !certificates.is_empty()
                    && certificates.len() <= 128
                    && certificates_valid
                    && certificate_bytes <= 4_194_304
                    && !private_key.is_empty()
                    && private_key.len() <= 1_048_576
                    && alpn.len() <= 32
                    && alpn_valid
                    && alpn_bytes <= 4_096
                    && matches!(minimum, 12 | 13)
                    && (1..=1_048_576).contains(buffer_limit);
                if !valid {
                    self.close_virtual_tcp(*resource);
                    return HostStart::Completed(tls_error(
                        CoreCtor::TlsInvalidConfig,
                        "the TLS server configuration is invalid",
                    ));
                }
                self.tls_streams.insert(resource.token);
                HostStart::Completed(tls_ok(HostValue::TlsStream(resource.token)))
            }
            lm_abi::OP_TLS_READ => {
                let (Some(HostArg::Tls(stream)), Some(HostArg::Int(count))) =
                    (args.first(), args.get(1))
                else {
                    return HostStart::Failed("Tls.Read needs a stream and count".to_string());
                };
                let Ok(count) = usize::try_from(*count) else {
                    return HostStart::Completed(tls_error(
                        CoreCtor::TlsInvalidConfig,
                        "the TLS read count is not positive",
                    ));
                };
                if count == 0 {
                    return HostStart::Completed(tls_error(
                        CoreCtor::TlsInvalidConfig,
                        "the TLS read count is not positive",
                    ));
                }
                if count > MAX_NETWORK_IO_BYTES {
                    return HostStart::Completed(tls_error(
                        CoreCtor::TlsLimitExceeded,
                        "the TLS read count is too large",
                    ));
                }
                match self.tls_read_value(*stream, count) {
                    Some(value) => HostStart::Completed(value),
                    None => self.defer_action(
                        key,
                        DeferredAction::TlsRead {
                            stream: *stream,
                            count,
                        },
                    ),
                }
            }
            lm_abi::OP_TLS_WRITE => {
                let (Some(HostArg::Tls(stream)), Some(HostArg::Bytes(bytes))) =
                    (args.first(), args.get(1))
                else {
                    return HostStart::Failed("Tls.Write needs a stream and bytes".to_string());
                };
                if !self.tls_streams.contains(stream) {
                    return HostStart::Completed(tls_closed());
                }
                let Some(state) = self.streams.get(stream) else {
                    return HostStart::Completed(tls_closed());
                };
                if state.write_closed {
                    return HostStart::Completed(tls_error(
                        CoreCtor::TlsProtocol,
                        "the TLS write side is closed",
                    ));
                }
                if bytes.len() > MAX_NETWORK_IO_BYTES {
                    return HostStart::Completed(tls_error(
                        CoreCtor::TlsLimitExceeded,
                        "the TLS write value is too large",
                    ));
                }
                if bytes.is_empty() {
                    return HostStart::Completed(tls_ok(HostValue::Int(0)));
                }
                let Some(peer) = state.peer else {
                    return HostStart::Completed(tls_error(
                        CoreCtor::TlsProtocol,
                        "the TLS peer closed the stream",
                    ));
                };
                let count = bytes.len().min(VIRTUAL_WRITE_CHUNK);
                let Some(peer) = self.streams.get_mut(&peer) else {
                    return HostStart::Completed(tls_error(
                        CoreCtor::TlsProtocol,
                        "the TLS peer closed the stream",
                    ));
                };
                if peer.read_closed {
                    return HostStart::Completed(tls_error(
                        CoreCtor::TlsProtocol,
                        "the TLS peer closed its read side",
                    ));
                }
                if peer.incoming.len().saturating_add(count) > MAX_VIRTUAL_STREAM_BYTES {
                    return HostStart::Completed(tls_error(
                        CoreCtor::TlsLimitExceeded,
                        "the virtual TLS stream buffer is full",
                    ));
                }
                peer.incoming
                    .extend(bytes.as_slice()[..count].iter().copied());
                HostStart::Completed(tls_ok(HostValue::Int(count as i64)))
            }
            lm_abi::OP_TLS_SHUTDOWN => {
                let Some(HostArg::Tls(stream)) = args.first() else {
                    return HostStart::Failed("Tls.Shutdown needs a stream".to_string());
                };
                if !self.tls_streams.contains(stream) {
                    return HostStart::Completed(tls_closed());
                }
                let Some(state) = self.streams.get_mut(stream) else {
                    return HostStart::Completed(tls_closed());
                };
                state.write_closed = true;
                let peer = state.peer;
                if let Some(peer) = peer.and_then(|peer| self.streams.get_mut(&peer)) {
                    peer.peer_write_closed = true;
                }
                HostStart::Completed(tls_ok(HostValue::Unit))
            }
            lm_abi::OP_TLS_LOCAL_ADDRESS => {
                let Some(HostArg::Tls(stream)) = args.first() else {
                    return HostStart::Failed("Tls.LocalAddress needs a stream".to_string());
                };
                let value = if self.tls_streams.contains(stream) {
                    self.streams
                        .get(stream)
                        .map(|state| tls_ok(HostValue::SocketAddress(state.local)))
                        .unwrap_or_else(tls_closed)
                } else {
                    tls_closed()
                };
                HostStart::Completed(value)
            }
            lm_abi::OP_TLS_PEER_ADDRESS => {
                let Some(HostArg::Tls(stream)) = args.first() else {
                    return HostStart::Failed("Tls.PeerAddress needs a stream".to_string());
                };
                let value = if self.tls_streams.contains(stream) {
                    self.streams
                        .get(stream)
                        .map(|state| tls_ok(HostValue::SocketAddress(state.peer_address)))
                        .unwrap_or_else(tls_closed)
                } else {
                    tls_closed()
                };
                HostStart::Completed(value)
            }
            lm_abi::OP_TLS_CLOSE => {
                let Some(HostArg::Tls(stream)) = args.first() else {
                    return HostStart::Failed("Tls.Close needs a stream".to_string());
                };
                if self.close_virtual_tls(*stream) {
                    HostStart::Completed(tls_ok(HostValue::Unit))
                } else {
                    HostStart::Completed(tls_closed())
                }
            }
            lm_abi::OP_TTY_IS_TERMINAL => {
                let Some(HostArg::StdStream(stream)) = args.first() else {
                    return HostStart::Failed("Tty.IsTerminal needs one stream".to_string());
                };
                HostStart::Completed(HostValue::Bool(
                    self.terminal_streams[std_stream_index(*stream)],
                ))
            }
            lm_abi::OP_TTY_SIZE => {
                let Some(HostArg::StdStream(stream)) = args.first() else {
                    return HostStart::Failed("Tty.Size needs one stream".to_string());
                };
                if !self.terminal_streams[std_stream_index(*stream)] {
                    return HostStart::Completed(core_error(CoreCtor::TtyNotTerminal, None));
                }
                let (columns, rows) = self.terminal_size;
                HostStart::Completed(core_ok(HostValue::Ctor(
                    CoreCtor::TtySize,
                    vec![HostValue::Int(columns), HostValue::Int(rows)],
                )))
            }
            lm_abi::OP_TTY_ENTER_RAW => {
                if self.raw_mode.is_some() {
                    return HostStart::Completed(core_error(CoreCtor::TtyBusy, None));
                }
                if !self.terminal_streams[std_stream_index(HostStdStream::Input)] {
                    return HostStart::Completed(core_error(CoreCtor::TtyNotTerminal, None));
                }
                let token = self.next_raw_mode;
                let Some(next) = token.checked_add(1) else {
                    return HostStart::Completed(core_error(
                        CoreCtor::TtyFailed,
                        Some("the raw mode token space is exhausted"),
                    ));
                };
                self.next_raw_mode = next;
                self.raw_mode = Some(token);
                HostStart::Completed(core_ok(HostValue::RawMode(token)))
            }
            lm_abi::OP_TTY_EXIT_RAW => {
                let Some(HostArg::RawMode(token)) = args.first() else {
                    return HostStart::Failed("Tty.ExitRaw needs one raw mode".to_string());
                };
                if self.raw_mode == Some(*token) {
                    self.raw_mode = None;
                    HostStart::Completed(core_ok(HostValue::Unit))
                } else {
                    HostStart::Completed(core_error(CoreCtor::TtyClosed, None))
                }
            }
            lm_abi::OP_SIGNAL_OPEN => {
                let Some(HostArg::List(kinds)) = args.first() else {
                    return HostStart::Failed("Signal.Open needs one signal list".to_string());
                };
                if kinds.is_empty() {
                    return HostStart::Completed(core_error(
                        CoreCtor::SignalInvalidInput,
                        Some("the signal list is empty"),
                    ));
                }
                if self.signal_stream.is_some() {
                    return HostStart::Completed(core_error(CoreCtor::SignalBusy, None));
                }
                let mut interrupt = false;
                let mut terminate = false;
                for kind in kinds {
                    match kind {
                        HostArg::SignalKind(HostSignalKind::Interrupt) => interrupt = true,
                        HostArg::SignalKind(HostSignalKind::Terminate) => terminate = true,
                        _ => {
                            return HostStart::Failed(
                                "Signal.Open needs signal values".to_string(),
                            );
                        }
                    }
                }
                let token = self.next_signal_stream;
                let Some(next) = token.checked_add(1) else {
                    return HostStart::Completed(core_error(
                        CoreCtor::SignalLimitExceeded,
                        Some("the signal stream token space is exhausted"),
                    ));
                };
                self.next_signal_stream = next;
                let mut queued = VecDeque::new();
                for kind in self.signals_on_open.drain(..) {
                    let requested = match kind {
                        HostSignalKind::Interrupt => interrupt,
                        HostSignalKind::Terminate => terminate,
                    };
                    if requested && !queued.contains(&kind) {
                        queued.push_back(kind);
                    }
                }
                self.signal_stream = Some(MemorySignalStream {
                    token,
                    interrupt,
                    terminate,
                    queued,
                });
                HostStart::Completed(core_ok(HostValue::SignalStream(token)))
            }
            lm_abi::OP_SIGNAL_NEXT => {
                let Some(HostArg::SignalStream(stream)) = args.first() else {
                    return HostStart::Failed("Signal.Next needs one signal stream".to_string());
                };
                match self.signal_next_value(*stream) {
                    Some(value) => HostStart::Completed(value),
                    None => self.defer_action(key, DeferredAction::SignalNext { stream: *stream }),
                }
            }
            lm_abi::OP_SIGNAL_CLOSE => {
                let Some(HostArg::SignalStream(stream)) = args.first() else {
                    return HostStart::Failed("Signal.Close needs one signal stream".to_string());
                };
                if self
                    .signal_stream
                    .as_ref()
                    .is_some_and(|active| active.token == *stream)
                {
                    self.signal_stream = None;
                    HostStart::Completed(core_ok(HostValue::Unit))
                } else {
                    HostStart::Completed(core_error(CoreCtor::SignalClosed, None))
                }
            }
            lm_abi::OP_PIPE_OPEN => {
                if !args.is_empty() {
                    return HostStart::Failed("Pipe.Open takes no arguments".to_string());
                }
                let Some(pipe) = self.take_pipe_token() else {
                    return HostStart::Completed(pipe_error(
                        CoreCtor::PipeErrorLimitExceeded,
                        Some("the pipe token space is exhausted"),
                    ));
                };
                let Some(reader) = self.take_pipe_token() else {
                    return HostStart::Completed(pipe_error(
                        CoreCtor::PipeErrorLimitExceeded,
                        Some("the pipe token space is exhausted"),
                    ));
                };
                let Some(writer) = self.take_pipe_token() else {
                    return HostStart::Completed(pipe_error(
                        CoreCtor::PipeErrorLimitExceeded,
                        Some("the pipe token space is exhausted"),
                    ));
                };
                self.pipes.insert(
                    pipe,
                    MemoryPipe {
                        bytes: VecDeque::new(),
                        reader_open: true,
                        writer_open: true,
                    },
                );
                self.pipe_ends.insert(
                    reader,
                    MemoryPipeEnd {
                        pipe,
                        kind: MemoryPipeKind::Reader,
                    },
                );
                self.pipe_ends.insert(
                    writer,
                    MemoryPipeEnd {
                        pipe,
                        kind: MemoryPipeKind::Writer,
                    },
                );
                HostStart::Completed(pipe_ok(HostValue::Tuple(vec![
                    HostValue::PipeReader(reader),
                    HostValue::PipeWriter(writer),
                ])))
            }
            lm_abi::OP_PIPE_READ => {
                let (Some(HostArg::PipeReader(reader)), Some(HostArg::Int(count))) =
                    (args.first(), args.get(1))
                else {
                    return HostStart::Failed("Pipe.Read needs a reader and count".to_string());
                };
                let Ok(count) = usize::try_from(*count) else {
                    return HostStart::Completed(pipe_error(
                        CoreCtor::PipeErrorInvalidInput,
                        Some("the read count is not positive"),
                    ));
                };
                if count == 0 || count > MAX_PIPE_IO_BYTES {
                    let (ctor, message) = if count == 0 {
                        (
                            CoreCtor::PipeErrorInvalidInput,
                            "the read count is not positive",
                        )
                    } else {
                        (
                            CoreCtor::PipeErrorLimitExceeded,
                            "the read count is too large",
                        )
                    };
                    return HostStart::Completed(pipe_error(ctor, Some(message)));
                }
                match self.pipe_read_value(*reader, count) {
                    Some(value) => HostStart::Completed(value),
                    None => self.defer_action(
                        key,
                        DeferredAction::PipeRead {
                            reader: *reader,
                            count,
                        },
                    ),
                }
            }
            lm_abi::OP_PIPE_WRITE => {
                let (Some(HostArg::PipeWriter(writer)), Some(HostArg::Bytes(bytes))) =
                    (args.first(), args.get(1))
                else {
                    return HostStart::Failed("Pipe.Write needs a writer and bytes".to_string());
                };
                if bytes.len() > MAX_PIPE_IO_BYTES {
                    return HostStart::Completed(pipe_error(
                        CoreCtor::PipeErrorLimitExceeded,
                        Some("the write value is too large"),
                    ));
                }
                HostStart::Completed(self.pipe_write_value(*writer, bytes))
            }
            lm_abi::OP_PIPE_CLOSE => {
                let token = match args.first() {
                    Some(HostArg::PipeReader(token)) | Some(HostArg::PipeWriter(token)) => *token,
                    _ => return HostStart::Failed("Pipe.Close needs one pipe end".to_string()),
                };
                if self.close_virtual_pipe(token) {
                    HostStart::Completed(pipe_ok(HostValue::Unit))
                } else {
                    HostStart::Completed(pipe_error(CoreCtor::PipeErrorClosed, None))
                }
            }
            lm_abi::OP_EXEC_SPAWN => {
                let Some(HostArg::ExecSpec(spec)) = args.first() else {
                    return HostStart::Failed(
                        "Exec.Spawn needs one child specification".to_string(),
                    );
                };
                if let Some(error) = self.validate_exec_spec(spec) {
                    return HostStart::Completed(error);
                }
                let Some(program) = self.child_programs.get(spec.program.as_str()).cloned() else {
                    return HostStart::Completed(exec_error(
                        CoreCtor::ExecErrorNotFound,
                        Some("the test host does not know the child program"),
                    ));
                };
                if spec
                    .directory
                    .as_ref()
                    .is_some_and(|path| !self.directories.contains(path.as_str()))
                {
                    return HostStart::Completed(exec_error(
                        CoreCtor::ExecErrorNotFound,
                        Some("the child directory does not exist"),
                    ));
                }
                let child = self.next_child;
                let Some(next) = child.checked_add(1) else {
                    return HostStart::Completed(exec_error(
                        CoreCtor::ExecErrorLimitExceeded,
                        Some("the child token space is exhausted"),
                    ));
                };
                self.next_child = next;
                self.write_child_output(spec.output, &program.output, false);
                self.write_child_output(spec.error, &program.error, true);
                self.consume_child_pipes(spec);
                self.children.insert(
                    child,
                    MemoryChild {
                        status: program.status,
                    },
                );
                HostStart::Completed(exec_ok(HostValue::Child(child)))
            }
            lm_abi::OP_EXEC_WAIT => {
                let Some(HostArg::Child(child)) = args.first() else {
                    return HostStart::Failed("Exec.Wait needs one child".to_string());
                };
                HostStart::Completed(self.child_wait_value(*child, true))
            }
            lm_abi::OP_EXEC_TERMINATE | lm_abi::OP_EXEC_KILL => {
                let Some(HostArg::Child(child)) = args.first() else {
                    return HostStart::Failed("Exec termination needs one child".to_string());
                };
                let Some(child) = self.children.get_mut(child) else {
                    return HostStart::Completed(exec_error(CoreCtor::ExecErrorClosed, None));
                };
                child.status = MemoryChildStatus::Terminated;
                HostStart::Completed(exec_ok(HostValue::Unit))
            }
            lm_abi::OP_EXEC_CLOSE => {
                let Some(HostArg::Child(child)) = args.first() else {
                    return HostStart::Failed("Exec.Close needs one child".to_string());
                };
                if self.children.remove(child).is_some() {
                    HostStart::Completed(exec_ok(HostValue::Unit))
                } else {
                    HostStart::Completed(exec_error(CoreCtor::ExecErrorClosed, None))
                }
            }
            lm_abi::OP_UDP_BIND => {
                let Some(HostArg::SocketAddress(address)) = args.first() else {
                    return HostStart::Failed("Udp.Bind needs one address".to_string());
                };
                let mut address = *address;
                if address.port == 0 {
                    let Some(port) = self.take_port() else {
                        return HostStart::Completed(net_error(
                            CoreCtor::NetLimitExceeded,
                            "the virtual port space is exhausted",
                        ));
                    };
                    address.port = port;
                }
                if self.udp_addresses.contains_key(&address) {
                    return HostStart::Completed(net_error(
                        CoreCtor::NetAddressInUse,
                        "the address already has a UDP socket",
                    ));
                }
                let Some(token) = self.take_udp_token() else {
                    return HostStart::Completed(net_error(
                        CoreCtor::NetLimitExceeded,
                        "the virtual UDP token space is exhausted",
                    ));
                };
                self.udp_sockets.insert(
                    token,
                    MemoryUdpSocket {
                        address,
                        incoming: VecDeque::new(),
                    },
                );
                self.udp_addresses.insert(address, token);
                HostStart::Completed(net_ok(HostValue::UdpSocket(token)))
            }
            lm_abi::OP_UDP_SEND_TO => {
                let (
                    Some(HostArg::Udp(socket)),
                    Some(HostArg::SocketAddress(address)),
                    Some(HostArg::Bytes(bytes)),
                ) = (args.first(), args.get(1), args.get(2))
                else {
                    return HostStart::Failed(
                        "Udp.SendTo needs a socket, address, and bytes".to_string(),
                    );
                };
                let Some(peer) = self.udp_sockets.get(socket).map(|socket| socket.address) else {
                    return HostStart::Completed(net_closed());
                };
                if bytes.len() > MAX_UDP_DATAGRAM_BYTES {
                    return HostStart::Completed(net_error(
                        CoreCtor::NetLimitExceeded,
                        "the UDP datagram is too large",
                    ));
                }
                if let Some(destination) = self.udp_addresses.get(address).copied() {
                    let target = self
                        .udp_sockets
                        .get_mut(&destination)
                        .expect("the UDP address names one socket");
                    let retained = target
                        .incoming
                        .iter()
                        .fold(0_usize, |total, (data, _)| total.saturating_add(data.len()));
                    if retained.saturating_add(bytes.len()) > MAX_VIRTUAL_UDP_BYTES {
                        return HostStart::Completed(net_error(
                            CoreCtor::NetLimitExceeded,
                            "the virtual UDP queue is full",
                        ));
                    }
                    target.incoming.push_back((bytes.clone(), peer));
                }
                HostStart::Completed(net_ok(HostValue::Unit))
            }
            lm_abi::OP_UDP_RECV_FROM => {
                let Some(HostArg::Udp(socket)) = args.first() else {
                    return HostStart::Failed("Udp.RecvFrom needs one socket".to_string());
                };
                match self.udp_receive_value(*socket) {
                    Some(value) => HostStart::Completed(value),
                    None => self.defer_action(key, DeferredAction::UdpRecv { socket: *socket }),
                }
            }
            lm_abi::OP_UDP_LOCAL_ADDRESS => {
                let Some(HostArg::Udp(socket)) = args.first() else {
                    return HostStart::Failed("Udp.LocalAddress needs one socket".to_string());
                };
                match self.udp_sockets.get(socket) {
                    Some(socket) => {
                        HostStart::Completed(net_ok(HostValue::SocketAddress(socket.address)))
                    }
                    None => HostStart::Completed(net_closed()),
                }
            }
            lm_abi::OP_UDP_CLOSE => {
                let Some(HostArg::Udp(socket)) = args.first() else {
                    return HostStart::Failed("Udp.Close needs one socket".to_string());
                };
                if self.close_virtual_udp(*socket) {
                    HostStart::Completed(net_ok(HostValue::Unit))
                } else {
                    HostStart::Completed(net_closed())
                }
            }
            _ => HostStart::Failed(format!(
                "the test host does not implement {}",
                lm_abi::op_name(op)
            )),
        }
    }

    fn take_ready(&mut self, token: u64) -> Option<HostCompletion> {
        let action = self.pending.get(&token)?.action.clone();
        let input = match &action {
            DeferredAction::InputRead(count) => Some(
                self.input_bytes
                    .iter()
                    .copied()
                    .take(*count)
                    .collect::<Vec<_>>(),
            ),
            _ => None,
        };
        let stream = match &action {
            DeferredAction::Read { stream, .. } | DeferredAction::TlsRead { stream, .. } => self
                .streams
                .get(stream)
                .map(|state| (*stream, state.incoming.clone())),
            _ => None,
        };
        let accepted = match &action {
            DeferredAction::Accept(listener) => self
                .listeners
                .get(listener)
                .and_then(|state| state.incoming.front().copied())
                .map(|connection| (*listener, connection)),
            _ => None,
        };
        let signal = match &action {
            DeferredAction::SignalNext { stream } => self
                .signal_stream
                .as_ref()
                .filter(|state| state.token == *stream)
                .and_then(|state| state.queued.front().copied()),
            _ => None,
        };
        let pipe = match &action {
            DeferredAction::PipeRead { reader, .. } => self.pipe_before_read(*reader),
            _ => None,
        };
        let udp = match &action {
            DeferredAction::UdpRecv { socket } => self
                .udp_sockets
                .get(socket)
                .and_then(|state| state.incoming.front().cloned())
                .map(|(bytes, peer)| (*socket, bytes, peer)),
            _ => None,
        };
        let value = self.deferred_value(token)?;
        let mut entry = self.pending.remove(&token)?;
        if entry.wait_source {
            let dynamic = if let Some(bytes) = input {
                (!bytes.is_empty()).then_some(RetainedWait::Input(bytes))
            } else if let Some((stream, before)) = stream {
                let after = self
                    .streams
                    .get(&stream)
                    .map(|state| state.incoming.len())
                    .unwrap_or(0);
                let removed = before.len().saturating_sub(after);
                (removed > 0).then(|| RetainedWait::StreamRead {
                    stream,
                    bytes: before.into_iter().take(removed).collect(),
                })
            } else if let Some((listener, connection)) = accepted {
                let still_first = self
                    .listeners
                    .get(&listener)
                    .and_then(|state| state.incoming.front().copied())
                    == Some(connection);
                (!still_first).then_some(RetainedWait::Accept {
                    listener,
                    connection,
                })
            } else if let Some((pipe, before)) = pipe {
                let after = self
                    .pipes
                    .get(&pipe)
                    .map(|state| state.bytes.len())
                    .unwrap_or(0);
                let removed = before.len().saturating_sub(after);
                (removed > 0).then(|| RetainedWait::PipeRead {
                    pipe,
                    bytes: before.into_iter().take(removed).collect(),
                })
            } else if let Some((socket, bytes, peer)) = udp {
                Some(RetainedWait::UdpDatagram {
                    socket,
                    bytes,
                    peer,
                })
            } else {
                signal.map(RetainedWait::Signal)
            };
            if let Some(retained) = entry.rollback.take().or(dynamic) {
                self.retained_waits.insert(token, retained);
            }
            self.ready_waits.insert(token);
        }
        Some(HostCompletion {
            key: entry.key,
            token,
            result: Ok(value),
        })
    }

    fn restore_wait(&mut self, retained: RetainedWait) {
        match retained {
            RetainedWait::Input(bytes) => {
                let mut restored = bytes;
                restored.append(&mut self.input_bytes);
                self.input_bytes = restored;
            }
            RetainedWait::StreamRead { stream, bytes } => {
                if let Some(state) = self.streams.get_mut(&stream) {
                    for byte in bytes.into_iter().rev() {
                        state.incoming.push_front(byte);
                    }
                }
            }
            RetainedWait::Accept {
                listener,
                connection,
            } => {
                if let Some(state) = self.listeners.get_mut(&listener) {
                    state.incoming.push_front(connection);
                }
            }
            RetainedWait::Connect { client } => {
                let server = self.streams.get(&client).and_then(|state| state.peer);
                if let Some(server) = server {
                    for listener in self.listeners.values_mut() {
                        if let Some(at) = listener
                            .incoming
                            .iter()
                            .position(|(stream, _)| *stream == server)
                        {
                            listener.incoming.remove(at);
                            break;
                        }
                    }
                    self.close_virtual_tcp(HostTcpResource {
                        kind: HostTcpKind::Stream,
                        token: server,
                    });
                }
                self.close_virtual_tcp(HostTcpResource {
                    kind: HostTcpKind::Stream,
                    token: client,
                });
            }
            RetainedWait::Signal(kind) => {
                if let Some(stream) = &mut self.signal_stream {
                    if !stream.queued.contains(&kind) {
                        stream.queued.push_front(kind);
                    }
                }
            }
            RetainedWait::PipeRead { pipe, bytes } => {
                if let Some(state) = self.pipes.get_mut(&pipe) {
                    for byte in bytes.into_iter().rev() {
                        state.bytes.push_front(byte);
                    }
                }
            }
            RetainedWait::ChildWait { .. } => {}
            RetainedWait::UdpDatagram {
                socket,
                bytes,
                peer,
            } => {
                if let Some(state) = self.udp_sockets.get_mut(&socket) {
                    state.incoming.push_front((bytes, peer));
                }
            }
        }
    }
}

fn result_tcp_stream(value: &HostValue) -> Option<u64> {
    match value {
        HostValue::Ctor(CoreCtor::Ok, values) => match values.as_slice() {
            [HostValue::TcpStream(token)] => Some(*token),
            _ => None,
        },
        _ => None,
    }
}

impl Host for RecordingHost {
    fn start(&mut self, key: CompletionKey, op: u32, args: Vec<HostArg>) -> HostStart {
        // The reply is computed now, so every effect keeps submit
        // order and the run stays deterministic. Only its delivery
        // waits.
        match self.serve(key, op, args) {
            HostStart::Completed(value) if deferred_op(op) => self.defer(key, value),
            other => other,
        }
    }

    fn start_wait(&mut self, key: CompletionKey, op: u32, args: Vec<HostArg>) -> HostStart {
        match op {
            lm_abi::OP_CLOCK_SLEEP => {
                self.defer_wait(key, DeferredAction::Ready(HostValue::Unit), None)
            }
            lm_abi::OP_IO_READ_BYTES => {
                let Some(HostArg::Int(count)) = args.first() else {
                    return HostStart::Failed("Io.ReadBytes needs one integer".to_string());
                };
                let Ok(count) = usize::try_from(*count) else {
                    return HostStart::Completed(core_error(
                        CoreCtor::IoErrorInvalidInput,
                        Some("the read count is negative"),
                    ));
                };
                if count > MAX_CONSOLE_IO_BYTES {
                    return HostStart::Completed(core_error(
                        CoreCtor::IoErrorLimitExceeded,
                        Some("the read count is too large"),
                    ));
                }
                self.defer_wait(key, DeferredAction::InputRead(count), None)
            }
            lm_abi::OP_DNS_RESOLVE => match self.serve(key, op, args) {
                HostStart::Completed(value) => {
                    self.defer_wait(key, DeferredAction::Ready(value), None)
                }
                other => other,
            },
            lm_abi::OP_TCP_CONNECT => match self.serve(key, op, args) {
                HostStart::Completed(value) => {
                    let rollback =
                        result_tcp_stream(&value).map(|client| RetainedWait::Connect { client });
                    self.defer_wait(key, DeferredAction::Ready(value), rollback)
                }
                other => other,
            },
            lm_abi::OP_TCP_ACCEPT => {
                let Some(HostArg::Tcp(resource)) = args.first() else {
                    return HostStart::Failed("Tcp.Accept needs a listener".to_string());
                };
                if resource.kind != HostTcpKind::Listener {
                    return HostStart::Failed("Tcp.Accept needs a listener".to_string());
                }
                self.defer_wait(key, DeferredAction::Accept(resource.token), None)
            }
            lm_abi::OP_TCP_READ => {
                let (Some(HostArg::Tcp(resource)), Some(HostArg::Int(count))) =
                    (args.first(), args.get(1))
                else {
                    return HostStart::Failed("Tcp.Read needs a stream and count".to_string());
                };
                if resource.kind != HostTcpKind::Stream {
                    return HostStart::Failed("Tcp.Read needs a stream".to_string());
                }
                let Ok(count) = usize::try_from(*count) else {
                    return HostStart::Completed(net_error(
                        CoreCtor::NetInvalidInput,
                        "the read count is not positive",
                    ));
                };
                if count == 0 || count > MAX_NETWORK_IO_BYTES {
                    return HostStart::Completed(net_error(
                        if count == 0 {
                            CoreCtor::NetInvalidInput
                        } else {
                            CoreCtor::NetLimitExceeded
                        },
                        if count == 0 {
                            "the read count is not positive"
                        } else {
                            "the read count is too large"
                        },
                    ));
                }
                self.defer_wait(
                    key,
                    DeferredAction::Read {
                        stream: resource.token,
                        count,
                    },
                    None,
                )
            }
            lm_abi::OP_TLS_READ => {
                let (Some(HostArg::Tls(stream)), Some(HostArg::Int(count))) =
                    (args.first(), args.get(1))
                else {
                    return HostStart::Failed("Tls.Read needs a stream and count".to_string());
                };
                let Ok(count) = usize::try_from(*count) else {
                    return HostStart::Completed(tls_error(
                        CoreCtor::TlsInvalidConfig,
                        "the TLS read count is not positive",
                    ));
                };
                if count == 0 || count > MAX_NETWORK_IO_BYTES {
                    return HostStart::Completed(tls_error(
                        if count == 0 {
                            CoreCtor::TlsInvalidConfig
                        } else {
                            CoreCtor::TlsLimitExceeded
                        },
                        if count == 0 {
                            "the TLS read count is not positive"
                        } else {
                            "the TLS read count is too large"
                        },
                    ));
                }
                self.defer_wait(
                    key,
                    DeferredAction::TlsRead {
                        stream: *stream,
                        count,
                    },
                    None,
                )
            }
            lm_abi::OP_SIGNAL_NEXT => {
                let Some(HostArg::SignalStream(stream)) = args.first() else {
                    return HostStart::Failed("Signal.Next needs one signal stream".to_string());
                };
                self.defer_wait(key, DeferredAction::SignalNext { stream: *stream }, None)
            }
            lm_abi::OP_PIPE_READ => {
                let (Some(HostArg::PipeReader(reader)), Some(HostArg::Int(count))) =
                    (args.first(), args.get(1))
                else {
                    return HostStart::Failed("Pipe.Read needs a reader and count".to_string());
                };
                let Ok(count) = usize::try_from(*count) else {
                    return HostStart::Completed(pipe_error(
                        CoreCtor::PipeErrorInvalidInput,
                        Some("the read count is not positive"),
                    ));
                };
                if count == 0 || count > MAX_PIPE_IO_BYTES {
                    let (ctor, message) = if count == 0 {
                        (
                            CoreCtor::PipeErrorInvalidInput,
                            "the read count is not positive",
                        )
                    } else {
                        (
                            CoreCtor::PipeErrorLimitExceeded,
                            "the read count is too large",
                        )
                    };
                    return HostStart::Completed(pipe_error(ctor, Some(message)));
                }
                self.defer_wait(
                    key,
                    DeferredAction::PipeRead {
                        reader: *reader,
                        count,
                    },
                    None,
                )
            }
            lm_abi::OP_EXEC_WAIT => {
                let Some(HostArg::Child(child)) = args.first() else {
                    return HostStart::Failed("Exec.Wait needs one child".to_string());
                };
                if !self.children.contains_key(child) {
                    return HostStart::Completed(exec_error(CoreCtor::ExecErrorClosed, None));
                }
                self.defer_wait(
                    key,
                    DeferredAction::ChildWait { child: *child },
                    Some(RetainedWait::ChildWait { child: *child }),
                )
            }
            lm_abi::OP_UDP_RECV_FROM => {
                let Some(HostArg::Udp(socket)) = args.first() else {
                    return HostStart::Failed("Udp.RecvFrom needs one socket".to_string());
                };
                self.defer_wait(key, DeferredAction::UdpRecv { socket: *socket }, None)
            }
            _ => self.start(key, op, args),
        }
    }

    /// Answer the oldest ready reply. A poll with nothing ready moves
    /// every pending reply one step closer, so the order is stable.
    fn poll(&mut self) -> Option<HostCompletion> {
        let tokens: Vec<u64> = self
            .pending
            .iter()
            .filter_map(|(token, entry)| (entry.left == 0).then_some(*token))
            .collect();
        for token in tokens {
            if let Some(completion) = self.take_ready(token) {
                return Some(completion);
            }
        }
        for entry in self.pending.values_mut() {
            entry.left = entry.left.saturating_sub(1);
        }
        None
    }

    fn wait(&mut self) -> Option<HostCompletion> {
        let tokens: Vec<u64> = self.pending.keys().copied().collect();
        tokens.into_iter().find_map(|token| self.take_ready(token))
    }

    fn close_file(&mut self, token: u64) -> bool {
        self.file_handles.remove(&token).is_some()
    }

    fn cancel(&mut self, token: u64) -> bool {
        let Some(pending) = self.pending.remove(&token) else {
            return false;
        };
        if let DeferredAction::Ready(value) = pending.action {
            self.discard_value_resources(value);
        }
        true
    }

    fn commit_wait(&mut self, token: u64) -> bool {
        let ready = self.ready_waits.remove(&token);
        if let Some(RetainedWait::ChildWait { child }) = self.retained_waits.remove(&token) {
            self.children.remove(&child);
        }
        ready
    }

    fn cancel_wait(&mut self, token: u64) -> HostWaitCancel {
        if let Some(mut pending) = self.pending.remove(&token) {
            if let Some(retained) = pending.rollback.take() {
                self.restore_wait(retained);
            }
            return HostWaitCancel::Cancelled;
        }
        if self.ready_waits.remove(&token) {
            if let Some(retained) = self.retained_waits.remove(&token) {
                self.restore_wait(retained);
            }
            return HostWaitCancel::ReadyRestored;
        }
        HostWaitCancel::Missing
    }

    fn close_tcp(&mut self, resource: HostTcpResource) -> bool {
        self.close_virtual_tcp(resource)
    }

    fn close_tls(&mut self, token: u64) -> bool {
        self.close_virtual_tls(token)
    }

    fn close_raw_mode(&mut self, token: u64) -> bool {
        if self.raw_mode == Some(token) {
            self.raw_mode = None;
            true
        } else {
            false
        }
    }

    fn close_signal_stream(&mut self, token: u64) -> bool {
        if self
            .signal_stream
            .as_ref()
            .is_some_and(|stream| stream.token == token)
        {
            self.signal_stream = None;
            true
        } else {
            false
        }
    }

    fn close_pipe(&mut self, token: u64) -> bool {
        self.close_virtual_pipe(token)
    }

    fn close_child(&mut self, token: u64) -> bool {
        self.children.remove(&token).is_some()
    }

    fn close_udp(&mut self, token: u64) -> bool {
        self.close_virtual_udp(token)
    }
}

/// A shared recording host, so a test keeps access to the buffers
/// after the world takes the host box.
impl Host for std::rc::Rc<std::cell::RefCell<RecordingHost>> {
    fn start(&mut self, key: CompletionKey, op: u32, args: Vec<HostArg>) -> HostStart {
        self.borrow_mut().start(key, op, args)
    }

    fn start_wait(&mut self, key: CompletionKey, op: u32, args: Vec<HostArg>) -> HostStart {
        self.borrow_mut().start_wait(key, op, args)
    }

    fn poll(&mut self) -> Option<HostCompletion> {
        self.borrow_mut().poll()
    }

    fn wait(&mut self) -> Option<HostCompletion> {
        self.borrow_mut().wait()
    }

    fn close_file(&mut self, token: u64) -> bool {
        self.borrow_mut().close_file(token)
    }

    fn cancel(&mut self, token: u64) -> bool {
        self.borrow_mut().cancel(token)
    }

    fn commit_wait(&mut self, token: u64) -> bool {
        self.borrow_mut().commit_wait(token)
    }

    fn cancel_wait(&mut self, token: u64) -> HostWaitCancel {
        self.borrow_mut().cancel_wait(token)
    }

    fn close_tcp(&mut self, resource: HostTcpResource) -> bool {
        self.borrow_mut().close_tcp(resource)
    }

    fn close_tls(&mut self, token: u64) -> bool {
        self.borrow_mut().close_tls(token)
    }

    fn close_udp(&mut self, token: u64) -> bool {
        self.borrow_mut().close_udp(token)
    }

    fn close_raw_mode(&mut self, token: u64) -> bool {
        self.borrow_mut().close_raw_mode(token)
    }

    fn close_signal_stream(&mut self, token: u64) -> bool {
        self.borrow_mut().close_signal_stream(token)
    }

    fn close_pipe(&mut self, token: u64) -> bool {
        self.borrow_mut().close_pipe(token)
    }

    fn close_child(&mut self, token: u64) -> bool {
        self.borrow_mut().close_child(token)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_arguments_and_replies_clone_shared_storage() {
        let bytes = SharedBytes::from(&[0, 255, 1]);
        let arg = HostArg::Bytes(bytes.clone());
        let reply = HostValue::Bytes(bytes.clone());
        let HostArg::Bytes(arg_bytes) = arg else {
            panic!("the argument is Bytes");
        };
        let HostValue::Bytes(reply_bytes) = reply else {
            panic!("the reply is Bytes");
        };
        assert!(bytes.shares_storage(&arg_bytes));
        assert!(bytes.shares_storage(&reply_bytes));
    }
}
