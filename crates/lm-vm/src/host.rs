//! The host-completion interface.
//!
//! `lm-vm` never touches the operating system. A `Host` receives
//! plain-data arguments for one root-granted fixed operation and
//! completes with a plain-data reply, now or later. No Rust reference
//! into guest memory crosses this boundary in either direction.

use crate::CompletionKey;
use lm_heap::{SharedBytes, SharedText};
use std::collections::{BTreeMap, VecDeque};

/// One plain-data operation argument.
#[derive(Debug, Clone, PartialEq)]
pub enum HostArg {
    Int(i64),
    Str(SharedText),
    Bytes(SharedBytes),
    File(u64),
    OpenOptions(HostOpenOptions),
    SeekFrom(HostSeekFrom),
    SocketAddress(HostSocketAddress),
    Tcp(HostTcpResource),
    Shutdown(HostShutdown),
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

/// One portable TCP shutdown direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostShutdown {
    Read,
    Write,
    Both,
}

/// One portable file-open mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostOpenOptions {
    ReadOnly,
    WriteOnly,
    ReadWrite,
    Create,
    CreateTruncate,
    Append,
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
    IoErrorFailed,
    FsErrorClosed,
    FsErrorFailed,
    Pair,
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
}

/// One plain-data operation reply. `Ctor` builds a pinned core enum
/// value inside the performing machine.
#[derive(Debug, Clone, PartialEq)]
pub enum HostValue {
    Unit,
    Int(i64),
    Str(SharedText),
    Bytes(SharedBytes),
    File(u64),
    List(Vec<HostValue>),
    SocketAddress(HostSocketAddress),
    TcpStream(u64),
    TcpListener(u64),
    Ctor(CoreCtor, Vec<HostValue>),
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
    /// Poll the completion source without blocking.
    fn poll(&mut self) -> Option<HostCompletion>;
    /// Wait for the next completion.
    fn wait(&mut self) -> Option<HostCompletion>;
    /// Close one file token during forced resource cleanup.
    fn close_file(&mut self, _token: u64) -> bool {
        false
    }
    /// Cancel one pending completion token.
    fn cancel(&mut self, _token: u64) -> bool {
        false
    }
    /// Close one TCP resource during forced cleanup.
    fn close_tcp(&mut self, _resource: HostTcpResource) -> bool {
        false
    }
}

/// A host without any implementation. Every started operation fails.
pub struct NullHost;

impl Host for NullHost {
    fn start(&mut self, _key: CompletionKey, op: u32, _args: Vec<HostArg>) -> HostStart {
        HostStart::Failed(format!(
            "no host implementation for {}",
            lm_abi::op_name(op)
        ))
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
    pub printed: Vec<String>,
    pub errors: Vec<String>,
    pub input: Vec<String>,
    now: i64,
    monotonic: i64,
    rand_state: u64,
    /// Deferred replies, by completion token. The command-line host
    /// serves files and streams on worker threads, so this host defers
    /// the same operations. A test therefore meets the same boundary:
    /// a reply arrives at a later poll, not inside `start`.
    pending: BTreeMap<u64, Deferred>,
    next_token: u64,
    files: BTreeMap<String, Vec<u8>>,
    file_handles: BTreeMap<u64, MemoryFile>,
    next_file: u64,
    dns: BTreeMap<String, Vec<HostIpAddress>>,
    listeners: BTreeMap<u64, MemoryListener>,
    listener_addresses: BTreeMap<HostSocketAddress, u64>,
    streams: BTreeMap<u64, MemoryStream>,
    next_tcp: u64,
    next_port: u16,
}

/// One reply this host holds until a later poll.
#[derive(Debug)]
struct Deferred {
    key: CompletionKey,
    /// Polls remaining before the reply is ready.
    left: u32,
    action: DeferredAction,
}

#[derive(Debug, Clone)]
enum DeferredAction {
    Ready(HostValue),
    Accept(u64),
    Read { stream: u64, count: usize },
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
            | lm_abi::OP_FS_CLOSE
            | lm_abi::OP_IO_PRINT
            | lm_abi::OP_IO_ERROR
            | lm_abi::OP_IO_READ_LINE
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
const VIRTUAL_WRITE_CHUNK: usize = 4 << 10;

fn fs_ok(value: HostValue) -> HostValue {
    HostValue::Ctor(CoreCtor::Ok, vec![value])
}

fn fs_failed(message: impl Into<String>) -> HostValue {
    HostValue::Ctor(
        CoreCtor::Err,
        vec![HostValue::Ctor(
            CoreCtor::FsErrorFailed,
            vec![HostValue::Str(SharedText::from(message.into()))],
        )],
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
            printed: Vec::new(),
            errors: Vec::new(),
            input: Vec::new(),
            now: 1_000,
            monotonic: 0,
            rand_state: seed.max(1),
            pending: BTreeMap::new(),
            next_token: 1,
            files: BTreeMap::new(),
            file_handles: BTreeMap::new(),
            next_file: 1,
            dns,
            listeners: BTreeMap::new(),
            listener_addresses: BTreeMap::new(),
            streams: BTreeMap::new(),
            next_tcp: 1,
            next_port: 40_000,
        }
    }

    /// Set one in-memory file before execution.
    pub fn set_file(&mut self, path: impl Into<String>, bytes: Vec<u8>) {
        self.files.insert(path.into(), bytes);
    }

    /// Read one in-memory file after execution.
    pub fn file(&self, path: &str) -> Option<&[u8]> {
        self.files.get(path).map(Vec::as_slice)
    }

    /// Set one deterministic DNS answer.
    pub fn set_dns(&mut self, name: impl Into<String>, addresses: Vec<HostIpAddress>) {
        self.dns.insert(name.into(), addresses);
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
        Some(net_ok(HostValue::Ctor(
            CoreCtor::Pair,
            vec![HostValue::TcpStream(stream), HostValue::SocketAddress(peer)],
        )))
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

    fn deferred_value(&mut self, token: u64) -> Option<HostValue> {
        let action = self.pending.get(&token)?.action.clone();
        match action {
            DeferredAction::Ready(value) => Some(value),
            DeferredAction::Accept(listener) => self.accept_value(listener),
            DeferredAction::Read { stream, count } => self.read_value(stream, count),
        }
    }
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
            },
        );
        HostStart::Waiting(token)
    }
}

impl RecordingHost {
    fn serve(&mut self, key: CompletionKey, op: u32, args: Vec<HostArg>) -> HostStart {
        let _ = key;
        match op {
            lm_abi::OP_IO_PRINT => {
                if let Some(HostArg::Str(text)) = args.first() {
                    self.printed.push(text.to_string());
                }
                HostStart::Completed(HostValue::Unit)
            }
            lm_abi::OP_IO_ERROR => {
                if let Some(HostArg::Str(text)) = args.first() {
                    self.errors.push(text.to_string());
                }
                HostStart::Completed(HostValue::Unit)
            }
            lm_abi::OP_IO_READ_LINE => {
                let reply = if self.input.is_empty() {
                    HostValue::Ctor(CoreCtor::Ok, vec![HostValue::Ctor(CoreCtor::None, vec![])])
                } else {
                    let line = self.input.remove(0);
                    HostValue::Ctor(
                        CoreCtor::Ok,
                        vec![HostValue::Ctor(
                            CoreCtor::Some,
                            vec![HostValue::Str(line.into())],
                        )],
                    )
                };
                HostStart::Completed(reply)
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
                let (readable, writable, append, create, truncate) = match options {
                    HostOpenOptions::ReadOnly => (true, false, false, false, false),
                    HostOpenOptions::WriteOnly => (false, true, false, false, false),
                    HostOpenOptions::ReadWrite => (true, true, false, false, false),
                    HostOpenOptions::Create => (true, true, false, true, false),
                    HostOpenOptions::CreateTruncate => (true, true, false, true, true),
                    HostOpenOptions::Append => (false, true, true, true, false),
                };
                if !create && !self.files.contains_key(path.as_str()) {
                    return HostStart::Completed(fs_failed("the file does not exist"));
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
                    return HostStart::Completed(fs_failed("the read count is negative"));
                };
                if count > MAX_FILE_IO_BYTES {
                    return HostStart::Completed(fs_failed("the read count is too large"));
                }
                let (files, handles) = (&self.files, &mut self.file_handles);
                let Some(handle) = handles.get_mut(token) else {
                    return HostStart::Completed(fs_failed("the file token is not open"));
                };
                if !handle.readable {
                    return HostStart::Completed(fs_failed("the file is not readable"));
                }
                let Some(file) = files.get(&handle.path) else {
                    return HostStart::Completed(fs_failed("the file does not exist"));
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
                    return HostStart::Completed(fs_failed("the file token is not open"));
                };
                if !handle.writable {
                    return HostStart::Completed(fs_failed("the file is not writable"));
                }
                let Some(file) = files.get_mut(&handle.path) else {
                    return HostStart::Completed(fs_failed("the file does not exist"));
                };
                if handle.append {
                    handle.cursor = file.len();
                }
                let Some(end) = handle.cursor.checked_add(bytes.len()) else {
                    return HostStart::Completed(fs_failed("the write position is too large"));
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
                    return HostStart::Completed(fs_failed("the file token is not open"));
                };
                let Some(file) = files.get(&handle.path) else {
                    return HostStart::Completed(fs_failed("the file does not exist"));
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
                    return HostStart::Completed(fs_failed("the seek position is invalid"));
                };
                handle.cursor = position;
                HostStart::Completed(fs_ok(HostValue::Int(position as i64)))
            }
            lm_abi::OP_FS_FLUSH => {
                let Some(HostArg::File(token)) = args.first() else {
                    return HostStart::Failed("Fs.Flush needs a file".to_string());
                };
                if !self.file_handles.contains_key(token) {
                    return HostStart::Completed(fs_failed("the file token is not open"));
                }
                HostStart::Completed(fs_ok(HostValue::Unit))
            }
            lm_abi::OP_FS_CLOSE => {
                let Some(HostArg::File(token)) = args.first() else {
                    return HostStart::Failed("Fs.Close needs a file".to_string());
                };
                if self.file_handles.remove(token).is_none() {
                    return HostStart::Completed(fs_failed("the file token is not open"));
                }
                HostStart::Completed(fs_ok(HostValue::Unit))
            }
            lm_abi::OP_DNS_RESOLVE => {
                let (Some(HostArg::Str(name)), Some(HostArg::Int(port))) =
                    (args.first(), args.get(1))
                else {
                    return HostStart::Failed("Dns.Resolve needs a name and port".to_string());
                };
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
                let values = addresses
                    .iter()
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
            _ => HostStart::Failed(format!(
                "the test host does not implement {}",
                lm_abi::op_name(op)
            )),
        }
    }

    fn take_ready(&mut self, token: u64) -> Option<HostCompletion> {
        let value = self.deferred_value(token)?;
        let entry = self.pending.remove(&token)?;
        Some(HostCompletion {
            key: entry.key,
            token,
            result: Ok(value),
        })
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
        self.pending.remove(&token).is_some()
    }

    fn close_tcp(&mut self, resource: HostTcpResource) -> bool {
        self.close_virtual_tcp(resource)
    }
}

/// A shared recording host, so a test keeps access to the buffers
/// after the world takes the host box.
impl Host for std::rc::Rc<std::cell::RefCell<RecordingHost>> {
    fn start(&mut self, key: CompletionKey, op: u32, args: Vec<HostArg>) -> HostStart {
        self.borrow_mut().start(key, op, args)
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

    fn close_tcp(&mut self, resource: HostTcpResource) -> bool {
        self.borrow_mut().close_tcp(resource)
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
