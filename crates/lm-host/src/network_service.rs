//! Bounded DNS workers and one evented network reactor.

use crate::ReadySender;
use lm_vm::{
    CompletionKey, CoreCtor, HostCompletion, HostIpAddress, HostShutdown, HostSocketAddress,
    HostTcpKind, HostTcpResource, HostValue, HostWaitCancel, SharedBytes,
};
use mio::net::{TcpListener, TcpStream, UdpSocket};
use mio::{Events, Interest, Poll, Token, Waker};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName};
use socket2::{Domain, Protocol, Socket, Type};
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{Read, Write};
use std::net::{IpAddr, Shutdown, SocketAddr, ToSocketAddrs};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const WAKE: Token = Token(0);
const DNS_WORKERS: usize = 2;
const MAX_PENDING_NETWORK: usize = 4_096;
const MAX_NETWORK_RESOURCES: usize = 4_096;
const MAX_RETAINED_NETWORK_BYTES: usize = 64 << 20;
const MAX_DNS_RESULTS: usize = 64;
const TLS_CONFIG_OVERHEAD: usize = 256 << 10;
const MAX_UDP_DATAGRAM_BYTES: usize = 65_535;

pub(crate) struct NetworkService {
    requests: SyncSender<Command>,
    controls: Sender<Control>,
    wake: Arc<Waker>,
    dns: SyncSender<DnsJob>,
    pending: Arc<AtomicUsize>,
    retained: Arc<AtomicUsize>,
    active_dns: Arc<Mutex<HashSet<u64>>>,
    canceled_dns: Arc<Mutex<HashSet<u64>>>,
    waits: Arc<Mutex<NetworkWaitState>>,
    reactor: Option<std::thread::JoinHandle<()>>,
}

pub(crate) enum TcpRequest {
    Connect {
        stream: u64,
        address: HostSocketAddress,
    },
    Listen {
        listener: u64,
        address: HostSocketAddress,
        backlog: usize,
    },
    Accept {
        listener: u64,
        stream: u64,
    },
    Read {
        stream: u64,
        count: usize,
    },
    Write {
        stream: u64,
        bytes: SharedBytes,
    },
    Shutdown {
        stream: u64,
        direction: HostShutdown,
    },
    LocalAddress(HostTcpResource),
    PeerAddress {
        stream: u64,
    },
    Close(HostTcpResource),
}

pub(crate) struct TlsClientSettings {
    pub(crate) server_name: String,
    pub(crate) root_mode: i64,
    pub(crate) roots: Vec<SharedBytes>,
    pub(crate) alpn: Vec<SharedBytes>,
    pub(crate) minimum_version: i64,
    pub(crate) buffer_limit: usize,
}

pub(crate) struct TlsServerSettings {
    pub(crate) certificates: Vec<SharedBytes>,
    pub(crate) private_key: SharedBytes,
    pub(crate) alpn: Vec<SharedBytes>,
    pub(crate) minimum_version: i64,
    pub(crate) buffer_limit: usize,
}

pub(crate) enum TlsRequest {
    Handshake {
        stream: u64,
        settings: TlsClientSettings,
    },
    ServerHandshake {
        stream: u64,
        settings: TlsServerSettings,
    },
    Read {
        stream: u64,
        count: usize,
    },
    Write {
        stream: u64,
        bytes: SharedBytes,
    },
    Shutdown {
        stream: u64,
    },
    LocalAddress {
        stream: u64,
    },
    PeerAddress {
        stream: u64,
    },
    Close {
        stream: u64,
    },
}

pub(crate) enum UdpRequest {
    Bind {
        socket: u64,
        address: HostSocketAddress,
    },
    SendTo {
        socket: u64,
        address: HostSocketAddress,
        bytes: SharedBytes,
    },
    RecvFrom {
        socket: u64,
    },
    LocalAddress {
        socket: u64,
    },
    Close {
        socket: u64,
    },
}

struct DnsJob {
    pending: Pending,
    name: String,
    port: u16,
}

#[derive(Clone)]
struct Pending {
    key: CompletionKey,
    token: u64,
    retained: usize,
    wait_state: Option<Arc<Mutex<NetworkWaitState>>>,
    retained_budget: Arc<AtomicUsize>,
}

pub(crate) struct NetworkCompletion {
    value: HostCompletion,
    _retained: Option<RetainedLease>,
}

impl NetworkCompletion {
    pub(crate) fn into_value(self) -> HostCompletion {
        self.value
    }

    #[cfg(test)]
    pub(crate) fn without_retained(value: HostCompletion) -> NetworkCompletion {
        NetworkCompletion {
            value,
            _retained: None,
        }
    }
}

struct RetainedLease {
    budget: Arc<AtomicUsize>,
    bytes: usize,
}

impl Drop for RetainedLease {
    fn drop(&mut self) {
        release_retained(&self.budget, self.bytes);
    }
}

struct PendingWrite {
    pending: Pending,
    bytes: SharedBytes,
}

struct PendingDatagramWrite {
    pending: Pending,
    address: SocketAddr,
    bytes: SharedBytes,
}

struct QueuedDatagram {
    bytes: SharedBytes,
    peer: SocketAddr,
    _retained: RetainedLease,
}

enum Command {
    Request(Pending, TcpRequest),
    Tls(Pending, TlsRequest),
    Udp(Pending, UdpRequest),
}

enum Control {
    Cancel(u64),
    CancelWait {
        token: u64,
        reply: SyncSender<bool>,
    },
    RestoreWait {
        rollback: NetworkWaitRollback,
        reply: SyncSender<bool>,
    },
    ForceClose(HostTcpResource),
    ForceCloseTls(u64),
    ForceCloseUdp(u64),
    Stop,
}

enum Entry {
    Stream(StreamState),
    Listener(ListenerState),
    Tls(Box<TlsState>),
    Udp(UdpState),
}

struct StreamState {
    socket: TcpStream,
    registered: bool,
    connect: Option<Pending>,
    reads: VecDeque<(Pending, usize)>,
    writes: VecDeque<PendingWrite>,
    read_buffer: VecDeque<u8>,
    read_shutdown: bool,
    write_shutdown: bool,
}

struct ListenerState {
    socket: TcpListener,
    accepts: VecDeque<(Pending, u64)>,
    accepted: VecDeque<(TcpStream, SocketAddr)>,
}

struct TlsState {
    socket: TcpStream,
    connection: rustls::Connection,
    registered: bool,
    handshake: Option<Pending>,
    reads: VecDeque<(Pending, usize)>,
    read_buffer: VecDeque<u8>,
    writes: VecDeque<PendingWrite>,
    shutdowns: VecDeque<Pending>,
    peer_closed: bool,
    socket_eof: bool,
    write_shutdown: bool,
    close_notify_sent: bool,
    _retained: RetainedLease,
}

struct UdpState {
    socket: UdpSocket,
    registered: bool,
    receives: VecDeque<Pending>,
    writes: VecDeque<PendingDatagramWrite>,
    queued: VecDeque<QueuedDatagram>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum NetworkWaitKind {
    Dns,
    Reactor,
}

struct ReadyNetworkWait {
    rollback: NetworkWaitRollback,
    retained: usize,
}

enum NetworkWaitRollback {
    None,
    Connect {
        stream: u64,
    },
    Accept {
        listener: u64,
        stream: u64,
        address: SocketAddr,
    },
    TcpRead {
        stream: u64,
        bytes: Vec<u8>,
    },
    TlsRead {
        stream: u64,
        bytes: Vec<u8>,
    },
    UdpRecv {
        socket: u64,
        datagram: QueuedDatagram,
    },
}

#[derive(Default)]
struct NetworkWaitState {
    pending: HashMap<u64, NetworkWaitKind>,
    ready: HashMap<u64, ReadyNetworkWait>,
    cancelled: HashSet<u64>,
}

impl TcpRequest {
    fn retained_bytes(&self) -> usize {
        match self {
            TcpRequest::Read { count, .. } => *count,
            TcpRequest::Write { bytes, .. } => bytes.retained_capacity(),
            _ => 0,
        }
    }
}

impl TlsClientSettings {
    fn retained_bytes(&self) -> usize {
        self.roots.iter().chain(self.alpn.iter()).fold(
            self.server_name
                .len()
                .saturating_add(self.buffer_limit.saturating_mul(2))
                .saturating_add(TLS_CONFIG_OVERHEAD),
            |total, bytes| total.saturating_add(bytes.retained_capacity()),
        )
    }
}

impl TlsServerSettings {
    fn retained_bytes(&self) -> usize {
        self.certificates.iter().chain(self.alpn.iter()).fold(
            self.private_key
                .retained_capacity()
                .saturating_add(self.buffer_limit.saturating_mul(2))
                .saturating_add(TLS_CONFIG_OVERHEAD),
            |total, bytes| total.saturating_add(bytes.retained_capacity()),
        )
    }
}

impl TlsRequest {
    fn retained_bytes(&self) -> usize {
        match self {
            TlsRequest::Handshake { settings, .. } => settings.retained_bytes(),
            TlsRequest::ServerHandshake { settings, .. } => settings.retained_bytes(),
            TlsRequest::Read { count, .. } => *count,
            TlsRequest::Write { bytes, .. } => bytes.retained_capacity(),
            _ => 0,
        }
    }
}

impl UdpRequest {
    fn retained_bytes(&self) -> usize {
        match self {
            UdpRequest::SendTo { bytes, .. } => bytes.retained_capacity(),
            UdpRequest::RecvFrom { .. } => MAX_UDP_DATAGRAM_BYTES,
            _ => 0,
        }
    }
}

impl NetworkService {
    pub(crate) fn new(completion_tx: ReadySender) -> NetworkService {
        let poll = Poll::new().expect("the network poll starts");
        let wake = Arc::new(Waker::new(poll.registry(), WAKE).expect("the network wake starts"));
        let (request_tx, request_rx) = mpsc::sync_channel(MAX_PENDING_NETWORK);
        let (control_tx, control_rx) = mpsc::channel();
        let pending = Arc::new(AtomicUsize::new(0));
        let retained = Arc::new(AtomicUsize::new(0));
        let waits = Arc::new(Mutex::new(NetworkWaitState::default()));
        let reactor_pending = Arc::clone(&pending);
        let reactor_retained = Arc::clone(&retained);
        let reactor_waits = Arc::clone(&waits);
        let reactor_completions = completion_tx.clone();
        let reactor = std::thread::Builder::new()
            .name("loom-network".to_string())
            .spawn(move || {
                reactor(
                    poll,
                    request_rx,
                    control_rx,
                    reactor_completions,
                    reactor_pending,
                    reactor_retained,
                    reactor_waits,
                )
            })
            .expect("the network reactor starts");

        let (dns, dns_rx) = mpsc::sync_channel(MAX_PENDING_NETWORK);
        let dns_rx = Arc::new(Mutex::new(dns_rx));
        let active_dns = Arc::new(Mutex::new(HashSet::new()));
        let canceled_dns = Arc::new(Mutex::new(HashSet::new()));
        for worker in 0..DNS_WORKERS {
            let jobs = Arc::clone(&dns_rx);
            let completions = completion_tx.clone();
            let pending = Arc::clone(&pending);
            let retained = Arc::clone(&retained);
            let active = Arc::clone(&active_dns);
            let canceled = Arc::clone(&canceled_dns);
            std::thread::Builder::new()
                .name(format!("loom-dns-{worker}"))
                .spawn(move || dns_worker(jobs, completions, pending, retained, active, canceled))
                .expect("the DNS worker starts");
        }

        NetworkService {
            requests: request_tx,
            controls: control_tx,
            wake,
            dns,
            pending,
            retained,
            active_dns,
            canceled_dns,
            waits,
            reactor: Some(reactor),
        }
    }

    pub(crate) fn submit_dns(
        &self,
        key: CompletionKey,
        token: u64,
        name: String,
        port: u16,
        wait_source: bool,
    ) -> bool {
        let retained = name.len();
        if !reserve(&self.pending, &self.retained, retained) {
            return false;
        }
        self.active_dns
            .lock()
            .expect("the DNS set locks")
            .insert(token);
        let job = DnsJob {
            pending: Pending {
                key,
                token,
                retained,
                wait_state: wait_source.then(|| Arc::clone(&self.waits)),
                retained_budget: Arc::clone(&self.retained),
            },
            name,
            port,
        };
        if wait_source {
            self.waits
                .lock()
                .expect("the network wait state locks")
                .pending
                .insert(token, NetworkWaitKind::Dns);
        }
        match self.dns.try_send(job) {
            Ok(()) => true,
            Err(TrySendError::Full(job)) | Err(TrySendError::Disconnected(job)) => {
                self.active_dns
                    .lock()
                    .expect("the DNS set locks")
                    .remove(&job.pending.token);
                if wait_source {
                    self.waits
                        .lock()
                        .expect("the network wait state locks")
                        .pending
                        .remove(&token);
                }
                release_pending(&self.pending);
                release_retained(&self.retained, job.pending.retained);
                false
            }
        }
    }

    pub(crate) fn submit_tcp(
        &self,
        key: CompletionKey,
        token: u64,
        request: TcpRequest,
        wait_source: bool,
    ) -> bool {
        let retained = request.retained_bytes();
        if !reserve(&self.pending, &self.retained, retained) {
            return false;
        }
        let pending = Pending {
            key,
            token,
            retained,
            wait_state: wait_source.then(|| Arc::clone(&self.waits)),
            retained_budget: Arc::clone(&self.retained),
        };
        if wait_source {
            self.waits
                .lock()
                .expect("the network wait state locks")
                .pending
                .insert(token, NetworkWaitKind::Reactor);
        }
        match self.requests.try_send(Command::Request(pending, request)) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                if wait_source {
                    self.waits
                        .lock()
                        .expect("the network wait state locks")
                        .pending
                        .remove(&token);
                }
                release_pending(&self.pending);
                release_retained(&self.retained, retained);
                return false;
            }
        }
        let _ = self.wake.wake();
        true
    }

    pub(crate) fn submit_tls(
        &self,
        key: CompletionKey,
        token: u64,
        request: TlsRequest,
        wait_source: bool,
    ) -> bool {
        let retained = request.retained_bytes();
        if !reserve(&self.pending, &self.retained, retained) {
            return false;
        }
        let pending = Pending {
            key,
            token,
            retained,
            wait_state: wait_source.then(|| Arc::clone(&self.waits)),
            retained_budget: Arc::clone(&self.retained),
        };
        if wait_source {
            self.waits
                .lock()
                .expect("the network wait state locks")
                .pending
                .insert(token, NetworkWaitKind::Reactor);
        }
        match self.requests.try_send(Command::Tls(pending, request)) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                if wait_source {
                    self.waits
                        .lock()
                        .expect("the network wait state locks")
                        .pending
                        .remove(&token);
                }
                release_pending(&self.pending);
                release_retained(&self.retained, retained);
                return false;
            }
        }
        let _ = self.wake.wake();
        true
    }

    pub(crate) fn submit_udp(
        &self,
        key: CompletionKey,
        token: u64,
        request: UdpRequest,
        wait_source: bool,
    ) -> bool {
        let retained = request.retained_bytes();
        if !reserve(&self.pending, &self.retained, retained) {
            return false;
        }
        let pending = Pending {
            key,
            token,
            retained,
            wait_state: wait_source.then(|| Arc::clone(&self.waits)),
            retained_budget: Arc::clone(&self.retained),
        };
        if wait_source {
            self.waits
                .lock()
                .expect("the network wait state locks")
                .pending
                .insert(token, NetworkWaitKind::Reactor);
        }
        match self.requests.try_send(Command::Udp(pending, request)) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                if wait_source {
                    self.waits
                        .lock()
                        .expect("the network wait state locks")
                        .pending
                        .remove(&token);
                }
                release_pending(&self.pending);
                release_retained(&self.retained, retained);
                return false;
            }
        }
        let _ = self.wake.wake();
        true
    }

    pub(crate) fn cancel(&self, token: u64) -> bool {
        let active = self.active_dns.lock().expect("the DNS set locks");
        if active.contains(&token) {
            self.canceled_dns
                .lock()
                .expect("the DNS cancel set locks")
                .insert(token);
        }
        drop(active);
        let sent = self.controls.send(Control::Cancel(token)).is_ok();
        let _ = self.wake.wake();
        sent
    }

    pub(crate) fn commit_wait(&self, token: u64) -> bool {
        let ready = self
            .waits
            .lock()
            .expect("the network wait state locks")
            .ready
            .remove(&token);
        let Some(ready) = ready else {
            return false;
        };
        release_retained(&self.retained, ready.retained);
        true
    }

    pub(crate) fn cancel_wait(&self, token: u64) -> HostWaitCancel {
        enum State {
            Ready(ReadyNetworkWait),
            Pending(NetworkWaitKind),
            Missing,
        }

        let state = {
            let mut waits = self.waits.lock().expect("the network wait state locks");
            if let Some(ready) = waits.ready.remove(&token) {
                State::Ready(ready)
            } else if let Some(kind) = waits.pending.get(&token).copied() {
                waits.cancelled.insert(token);
                State::Pending(kind)
            } else {
                State::Missing
            }
        };
        match state {
            State::Ready(ready) => {
                release_retained(&self.retained, ready.retained);
                if matches!(ready.rollback, NetworkWaitRollback::None) {
                    return HostWaitCancel::ReadyRestored;
                }
                let (reply, answer) = mpsc::sync_channel(1);
                if self
                    .controls
                    .send(Control::RestoreWait {
                        rollback: ready.rollback,
                        reply,
                    })
                    .is_err()
                {
                    return HostWaitCancel::Missing;
                }
                let _ = self.wake.wake();
                if answer.recv_timeout(Duration::from_secs(1)) == Ok(true) {
                    HostWaitCancel::ReadyRestored
                } else {
                    HostWaitCancel::Missing
                }
            }
            State::Pending(NetworkWaitKind::Dns) => HostWaitCancel::Cancelled,
            State::Pending(NetworkWaitKind::Reactor) => {
                let (reply, answer) = mpsc::sync_channel(1);
                if self
                    .controls
                    .send(Control::CancelWait { token, reply })
                    .is_err()
                {
                    return HostWaitCancel::Missing;
                }
                let _ = self.wake.wake();
                if answer.recv_timeout(Duration::from_secs(1)) == Ok(true) {
                    HostWaitCancel::Cancelled
                } else {
                    HostWaitCancel::Missing
                }
            }
            State::Missing => HostWaitCancel::Missing,
        }
    }

    pub(crate) fn force_close(&self, resource: HostTcpResource) -> bool {
        let sent = self.controls.send(Control::ForceClose(resource)).is_ok();
        let _ = self.wake.wake();
        sent
    }

    pub(crate) fn force_close_tls(&self, token: u64) -> bool {
        let sent = self.controls.send(Control::ForceCloseTls(token)).is_ok();
        let _ = self.wake.wake();
        sent
    }

    pub(crate) fn force_close_udp(&self, token: u64) -> bool {
        let sent = self.controls.send(Control::ForceCloseUdp(token)).is_ok();
        let _ = self.wake.wake();
        sent
    }
}

impl Drop for NetworkService {
    fn drop(&mut self) {
        let _ = self.controls.send(Control::Stop);
        let _ = self.wake.wake();
        if let Some(reactor) = self.reactor.take() {
            let _ = reactor.join();
        }
    }
}

fn reserve(pending: &AtomicUsize, retained: &AtomicUsize, bytes: usize) -> bool {
    if pending
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
            (count < MAX_PENDING_NETWORK).then_some(count + 1)
        })
        .is_err()
    {
        return false;
    }
    if bytes > MAX_RETAINED_NETWORK_BYTES
        || retained
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |total| {
                total
                    .checked_add(bytes)
                    .filter(|next| *next <= MAX_RETAINED_NETWORK_BYTES)
            })
            .is_err()
    {
        release_pending(pending);
        return false;
    }
    true
}

fn release_pending(pending: &AtomicUsize) {
    let previous = pending.fetch_sub(1, Ordering::Relaxed);
    debug_assert!(previous > 0);
}

fn release_retained(retained: &AtomicUsize, bytes: usize) {
    let previous = retained.fetch_sub(bytes, Ordering::Relaxed);
    debug_assert!(previous >= bytes);
}

fn complete(completions: &ReadySender, count: &AtomicUsize, pending: Pending, value: HostValue) {
    let _ = complete_with_rollback(
        completions,
        count,
        pending,
        value,
        NetworkWaitRollback::None,
    );
}

fn complete_with_rollback(
    completions: &ReadySender,
    count: &AtomicUsize,
    pending: Pending,
    value: HostValue,
    rollback: NetworkWaitRollback,
) -> Option<NetworkWaitRollback> {
    let completion_retained = if let Some(waits) = &pending.wait_state {
        let mut state = waits.lock().expect("the network wait state locks");
        state.pending.remove(&pending.token);
        if state.cancelled.remove(&pending.token) {
            release_retained(&pending.retained_budget, pending.retained);
            release_pending(count);
            return Some(rollback);
        }
        state.ready.insert(
            pending.token,
            ReadyNetworkWait {
                rollback,
                retained: pending.retained,
            },
        );
        0
    } else {
        pending.retained
    };
    let retained = (completion_retained > 0).then(|| RetainedLease {
        budget: Arc::clone(&pending.retained_budget),
        bytes: completion_retained,
    });
    let _ = completions.network(NetworkCompletion {
        value: HostCompletion {
            key: pending.key,
            token: pending.token,
            result: Ok(value),
        },
        _retained: retained,
    });
    release_pending(count);
    None
}

fn dns_worker(
    jobs: Arc<Mutex<Receiver<DnsJob>>>,
    completions: ReadySender,
    pending_count: Arc<AtomicUsize>,
    retained: Arc<AtomicUsize>,
    active: Arc<Mutex<HashSet<u64>>>,
    canceled: Arc<Mutex<HashSet<u64>>>,
) {
    loop {
        let job = {
            let receiver = jobs.lock().expect("the DNS queue locks");
            receiver.recv()
        };
        let Ok(job) = job else { return };
        let value = resolve_dns(&job.name, job.port);
        active
            .lock()
            .expect("the DNS set locks")
            .remove(&job.pending.token);
        let was_canceled = canceled
            .lock()
            .expect("the DNS cancel set locks")
            .remove(&job.pending.token);
        if was_canceled {
            release_pending(&pending_count);
            release_retained(&retained, job.pending.retained);
        } else {
            complete(&completions, &pending_count, job.pending, value);
        }
    }
}

fn resolve_dns(name: &str, port: u16) -> HostValue {
    let resolved = (name, port).to_socket_addrs();
    let Ok(resolved) = resolved else {
        return net_error(CoreCtor::NetNameNotFound, "the host name has no address");
    };
    let mut addresses = Vec::new();
    for address in resolved.take(MAX_DNS_RESULTS) {
        let address = host_address(address);
        if !addresses.contains(&address) {
            addresses.push(address);
        }
    }
    if addresses.is_empty() {
        return net_error(CoreCtor::NetNameNotFound, "the host name has no address");
    }
    net_ok(HostValue::List(
        addresses
            .into_iter()
            .map(HostValue::SocketAddress)
            .collect(),
    ))
}

fn reactor(
    mut poll: Poll,
    requests: Receiver<Command>,
    controls: Receiver<Control>,
    completions: ReadySender,
    pending: Arc<AtomicUsize>,
    retained: Arc<AtomicUsize>,
    waits: Arc<Mutex<NetworkWaitState>>,
) {
    let mut events = Events::with_capacity(1_024);
    let mut entries: HashMap<u64, Entry> = HashMap::new();
    loop {
        while let Ok(control) = controls.try_recv() {
            match control {
                Control::Cancel(token) => {
                    if let CancelToken::Close(resource) =
                        cancel_token(&mut entries, &pending, &retained, token)
                    {
                        close_entry(&poll, &mut entries, &completions, &pending, resource);
                    }
                }
                Control::CancelWait { token, reply } => {
                    let cancelled = cancel_token(&mut entries, &pending, &retained, token);
                    if let CancelToken::Close(resource) = cancelled {
                        close_entry(&poll, &mut entries, &completions, &pending, resource);
                    }
                    if !matches!(cancelled, CancelToken::Missing) {
                        let mut state = waits.lock().expect("the network wait state locks");
                        state.pending.remove(&token);
                        state.cancelled.remove(&token);
                    }
                    let _ = reply.send(true);
                }
                Control::RestoreWait { rollback, reply } => {
                    let restored =
                        restore_wait(&poll, &mut entries, &completions, &pending, rollback);
                    let _ = reply.send(restored);
                }
                Control::ForceClose(resource) => {
                    close_entry(&poll, &mut entries, &completions, &pending, resource.token);
                }
                Control::ForceCloseTls(stream) => {
                    close_entry(&poll, &mut entries, &completions, &pending, stream);
                }
                Control::ForceCloseUdp(socket) => {
                    close_entry(&poll, &mut entries, &completions, &pending, socket);
                }
                Control::Stop => return,
            }
        }
        while let Ok(command) = requests.try_recv() {
            handle_command(
                &poll,
                &mut entries,
                &completions,
                &pending,
                &retained,
                &waits,
                command,
            );
        }
        if poll.poll(&mut events, None).is_err() {
            return;
        }
        let ready: Vec<(u64, bool, bool)> = events
            .iter()
            .filter(|event| event.token() != WAKE)
            .map(|event| {
                (
                    event.token().0 as u64,
                    event.is_readable() || event.is_read_closed(),
                    event.is_writable() || event.is_write_closed(),
                )
            })
            .collect();
        for (resource, readable, writable) in ready {
            drive_entry(
                &poll,
                &mut entries,
                &completions,
                &pending,
                resource,
                readable,
                writable,
            );
        }
    }
}

fn handle_command(
    poll: &Poll,
    entries: &mut HashMap<u64, Entry>,
    completions: &ReadySender,
    count: &AtomicUsize,
    retained: &Arc<AtomicUsize>,
    waits: &Mutex<NetworkWaitState>,
    command: Command,
) {
    let token = match &command {
        Command::Request(pending, _) | Command::Tls(pending, _) | Command::Udp(pending, _) => {
            pending.token
        }
    };
    if waits
        .lock()
        .expect("the network wait state locks")
        .cancelled
        .remove(&token)
    {
        waits
            .lock()
            .expect("the network wait state locks")
            .pending
            .remove(&token);
        release_pending(count);
        let pending_retained = match command {
            Command::Request(pending, _) | Command::Tls(pending, _) | Command::Udp(pending, _) => {
                pending.retained
            }
        };
        release_retained(retained, pending_retained);
        return;
    }
    match command {
        Command::Request(pending, request) => match request {
            TcpRequest::Connect { stream, address } => {
                if entries.len() >= MAX_NETWORK_RESOURCES {
                    complete(
                        completions,
                        count,
                        pending,
                        net_error(CoreCtor::NetLimitExceeded, "the socket limit is full"),
                    );
                    return;
                }
                if entries.contains_key(&stream) {
                    complete(
                        completions,
                        count,
                        pending,
                        net_error(CoreCtor::NetFailed, "the TCP token is in use"),
                    );
                    return;
                }
                match TcpStream::connect(socket_address(address)) {
                    Ok(mut socket) => {
                        if let Err(error) = socket.set_nodelay(true) {
                            complete(completions, count, pending, io_error(error));
                            return;
                        }
                        if poll
                            .registry()
                            .register(
                                &mut socket,
                                Token(stream as usize),
                                Interest::READABLE | Interest::WRITABLE,
                            )
                            .is_err()
                        {
                            complete(
                                completions,
                                count,
                                pending,
                                net_error(
                                    CoreCtor::NetFailed,
                                    "the TCP stream registration failed",
                                ),
                            );
                            return;
                        }
                        entries.insert(
                            stream,
                            Entry::Stream(StreamState {
                                socket,
                                registered: true,
                                connect: Some(pending),
                                reads: VecDeque::new(),
                                writes: VecDeque::new(),
                                read_buffer: VecDeque::new(),
                                read_shutdown: false,
                                write_shutdown: false,
                            }),
                        );
                    }
                    Err(error) => complete(completions, count, pending, io_error(error)),
                }
            }
            TcpRequest::Listen {
                listener,
                address,
                backlog,
            } => {
                if entries.len() >= MAX_NETWORK_RESOURCES {
                    complete(
                        completions,
                        count,
                        pending,
                        net_error(CoreCtor::NetLimitExceeded, "the socket limit is full"),
                    );
                    return;
                }
                match bind_listener(socket_address(address), backlog) {
                    Ok(mut socket) => {
                        if poll
                            .registry()
                            .register(&mut socket, Token(listener as usize), Interest::READABLE)
                            .is_err()
                        {
                            complete(
                                completions,
                                count,
                                pending,
                                net_error(
                                    CoreCtor::NetFailed,
                                    "the TCP listener registration failed",
                                ),
                            );
                            return;
                        }
                        entries.insert(
                            listener,
                            Entry::Listener(ListenerState {
                                socket,
                                accepts: VecDeque::new(),
                                accepted: VecDeque::new(),
                            }),
                        );
                        complete(
                            completions,
                            count,
                            pending,
                            net_ok(HostValue::TcpListener(listener)),
                        );
                    }
                    Err(error) => complete(completions, count, pending, io_error(error)),
                }
            }
            TcpRequest::Accept { listener, stream } => {
                if entries.len() >= MAX_NETWORK_RESOURCES {
                    complete(
                        completions,
                        count,
                        pending,
                        net_error(CoreCtor::NetLimitExceeded, "the socket limit is full"),
                    );
                    return;
                }
                match entries.get_mut(&listener) {
                    Some(Entry::Listener(state)) => state.accepts.push_back((pending, stream)),
                    _ => {
                        complete(completions, count, pending, net_closed());
                        return;
                    }
                }
                drive_listener(poll, entries, completions, count, listener);
            }
            TcpRequest::Read {
                stream,
                count: size,
            } => {
                match entries.get_mut(&stream) {
                    Some(Entry::Stream(state))
                        if state.connect.is_none() && !state.read_shutdown =>
                    {
                        state.reads.push_back((pending, size));
                    }
                    _ => {
                        complete(completions, count, pending, net_closed());
                        return;
                    }
                }
                drive_stream(poll, entries, completions, count, stream, true, false);
            }
            TcpRequest::Write { stream, bytes } => {
                match entries.get_mut(&stream) {
                    Some(Entry::Stream(state))
                        if state.connect.is_none() && !state.write_shutdown =>
                    {
                        state.writes.push_back(PendingWrite { pending, bytes });
                    }
                    _ => {
                        complete(completions, count, pending, net_closed());
                        return;
                    }
                }
                drive_stream(poll, entries, completions, count, stream, false, true);
            }
            TcpRequest::Shutdown { stream, direction } => {
                let result = match entries.get_mut(&stream) {
                    Some(Entry::Stream(state)) if state.connect.is_none() => {
                        let close_read =
                            matches!(direction, HostShutdown::Read | HostShutdown::Both)
                                && !state.read_shutdown;
                        let close_write =
                            matches!(direction, HostShutdown::Write | HostShutdown::Both)
                                && !state.write_shutdown;
                        let system = match (close_read, close_write) {
                            (true, true) => Some(Shutdown::Both),
                            (true, false) => Some(Shutdown::Read),
                            (false, true) => Some(Shutdown::Write),
                            (false, false) => None,
                        };
                        match system.map_or(Ok(()), |system| state.socket.shutdown(system)) {
                            Ok(()) => {
                                if close_read {
                                    state.read_shutdown = true;
                                    for (pending, _) in state.reads.drain(..) {
                                        complete(completions, count, pending, net_closed());
                                    }
                                }
                                if close_write {
                                    state.write_shutdown = true;
                                    for write in state.writes.drain(..) {
                                        complete(completions, count, write.pending, net_closed());
                                    }
                                }
                                net_ok(HostValue::Unit)
                            }
                            Err(error) => io_error(error),
                        }
                    }
                    _ => net_closed(),
                };
                complete(completions, count, pending, result);
                drive_stream(poll, entries, completions, count, stream, false, false);
            }
            TcpRequest::LocalAddress(resource) => {
                let result = match entries.get(&resource.token) {
                    Some(Entry::Stream(state)) if resource.kind == HostTcpKind::Stream => {
                        state.socket.local_addr()
                    }
                    Some(Entry::Listener(state)) if resource.kind == HostTcpKind::Listener => {
                        state.socket.local_addr()
                    }
                    _ => {
                        complete(completions, count, pending, net_closed());
                        return;
                    }
                };
                let value = match result {
                    Ok(address) => net_ok(HostValue::SocketAddress(host_address(address))),
                    Err(error) => io_error(error),
                };
                complete(completions, count, pending, value);
            }
            TcpRequest::PeerAddress { stream } => {
                let result = match entries.get(&stream) {
                    Some(Entry::Stream(state)) if state.connect.is_none() => {
                        state.socket.peer_addr()
                    }
                    _ => {
                        complete(completions, count, pending, net_closed());
                        return;
                    }
                };
                let value = match result {
                    Ok(address) => net_ok(HostValue::SocketAddress(host_address(address))),
                    Err(error) => io_error(error),
                };
                complete(completions, count, pending, value);
            }
            TcpRequest::Close(resource) => {
                let existed = close_entry(poll, entries, completions, count, resource.token);
                let value = if existed {
                    net_ok(HostValue::Unit)
                } else {
                    net_closed()
                };
                complete(completions, count, pending, value);
            }
        },
        Command::Tls(pending, request) => {
            handle_tls_request(
                poll,
                entries,
                completions,
                count,
                retained,
                pending,
                request,
            );
        }
        Command::Udp(pending, request) => {
            handle_udp_request(poll, entries, completions, count, pending, request);
        }
    }
}

fn handle_udp_request(
    poll: &Poll,
    entries: &mut HashMap<u64, Entry>,
    completions: &ReadySender,
    count: &AtomicUsize,
    pending: Pending,
    request: UdpRequest,
) {
    match request {
        UdpRequest::Bind { socket, address } => {
            if entries.len() >= MAX_NETWORK_RESOURCES {
                complete(
                    completions,
                    count,
                    pending,
                    net_error(CoreCtor::NetLimitExceeded, "the socket limit is full"),
                );
                return;
            }
            if entries.contains_key(&socket) {
                complete(
                    completions,
                    count,
                    pending,
                    net_error(CoreCtor::NetFailed, "the UDP token is in use"),
                );
                return;
            }
            match UdpSocket::bind(socket_address(address)) {
                Ok(socket_handle) => {
                    entries.insert(
                        socket,
                        Entry::Udp(UdpState {
                            socket: socket_handle,
                            registered: false,
                            receives: VecDeque::new(),
                            writes: VecDeque::new(),
                            queued: VecDeque::new(),
                        }),
                    );
                    complete(
                        completions,
                        count,
                        pending,
                        net_ok(HostValue::UdpSocket(socket)),
                    );
                }
                Err(error) => complete(completions, count, pending, io_error(error)),
            }
        }
        UdpRequest::SendTo {
            socket,
            address,
            bytes,
        } => {
            match entries.get_mut(&socket) {
                Some(Entry::Udp(state)) => {
                    state.writes.push_back(PendingDatagramWrite {
                        pending,
                        address: socket_address(address),
                        bytes,
                    });
                }
                _ => {
                    complete(completions, count, pending, net_closed());
                    return;
                }
            }
            drive_udp(poll, entries, completions, count, socket, false, true);
        }
        UdpRequest::RecvFrom { socket } => {
            match entries.get_mut(&socket) {
                Some(Entry::Udp(state)) => state.receives.push_back(pending),
                _ => {
                    complete(completions, count, pending, net_closed());
                    return;
                }
            }
            drive_udp(poll, entries, completions, count, socket, true, false);
        }
        UdpRequest::LocalAddress { socket } => {
            let result = match entries.get(&socket) {
                Some(Entry::Udp(state)) => state.socket.local_addr(),
                _ => {
                    complete(completions, count, pending, net_closed());
                    return;
                }
            };
            let value = match result {
                Ok(address) => net_ok(HostValue::SocketAddress(host_address(address))),
                Err(error) => io_error(error),
            };
            complete(completions, count, pending, value);
        }
        UdpRequest::Close { socket } => {
            let existed = matches!(entries.get(&socket), Some(Entry::Udp(_)))
                && close_entry(poll, entries, completions, count, socket);
            complete(
                completions,
                count,
                pending,
                if existed {
                    net_ok(HostValue::Unit)
                } else {
                    net_closed()
                },
            );
        }
    }
}

fn handle_tls_request(
    poll: &Poll,
    entries: &mut HashMap<u64, Entry>,
    completions: &ReadySender,
    count: &AtomicUsize,
    retained: &Arc<AtomicUsize>,
    pending: Pending,
    request: TlsRequest,
) {
    match request {
        TlsRequest::Handshake { stream, settings } => {
            let connection = match make_tls_client(settings) {
                Ok(connection) => rustls::Connection::Client(connection),
                Err(message) => {
                    close_entry(poll, entries, completions, count, stream);
                    complete(
                        completions,
                        count,
                        pending,
                        tls_error(CoreCtor::TlsInvalidConfig, message),
                    );
                    return;
                }
            };
            start_tls_handshake(
                poll,
                entries,
                completions,
                count,
                retained,
                pending,
                stream,
                connection,
            );
        }
        TlsRequest::ServerHandshake { stream, settings } => {
            let connection = match make_tls_server(settings) {
                Ok(connection) => rustls::Connection::Server(connection),
                Err(message) => {
                    close_entry(poll, entries, completions, count, stream);
                    complete(
                        completions,
                        count,
                        pending,
                        tls_error(CoreCtor::TlsInvalidConfig, message),
                    );
                    return;
                }
            };
            start_tls_handshake(
                poll,
                entries,
                completions,
                count,
                retained,
                pending,
                stream,
                connection,
            );
        }
        TlsRequest::Read {
            stream,
            count: size,
        } => {
            match entries.get_mut(&stream) {
                Some(Entry::Tls(state)) if state.handshake.is_none() => {
                    state.reads.push_back((pending, size));
                }
                _ => {
                    complete(completions, count, pending, tls_closed());
                    return;
                }
            }
            drive_tls(poll, entries, completions, count, stream, true, false);
        }
        TlsRequest::Write { stream, bytes } => {
            match entries.get_mut(&stream) {
                Some(Entry::Tls(state)) if state.handshake.is_none() && !state.write_shutdown => {
                    if bytes.is_empty() {
                        complete(completions, count, pending, tls_ok(HostValue::Int(0)));
                        return;
                    }
                    state.writes.push_back(PendingWrite { pending, bytes });
                }
                _ => {
                    complete(completions, count, pending, tls_closed());
                    return;
                }
            }
            drive_tls(poll, entries, completions, count, stream, false, true);
        }
        TlsRequest::Shutdown { stream } => {
            match entries.get_mut(&stream) {
                Some(Entry::Tls(state)) if state.handshake.is_none() && !state.write_shutdown => {
                    state.write_shutdown = true;
                    state.shutdowns.push_back(pending);
                }
                _ => {
                    complete(completions, count, pending, tls_closed());
                    return;
                }
            }
            drive_tls(poll, entries, completions, count, stream, false, true);
        }
        TlsRequest::LocalAddress { stream } => {
            let result = match entries.get(&stream) {
                Some(Entry::Tls(state)) if state.handshake.is_none() => state.socket.local_addr(),
                _ => {
                    complete(completions, count, pending, tls_closed());
                    return;
                }
            };
            let value = match result {
                Ok(address) => tls_ok(HostValue::SocketAddress(host_address(address))),
                Err(error) => tls_io_error(error),
            };
            complete(completions, count, pending, value);
        }
        TlsRequest::PeerAddress { stream } => {
            let result = match entries.get(&stream) {
                Some(Entry::Tls(state)) if state.handshake.is_none() => state.socket.peer_addr(),
                _ => {
                    complete(completions, count, pending, tls_closed());
                    return;
                }
            };
            let value = match result {
                Ok(address) => tls_ok(HostValue::SocketAddress(host_address(address))),
                Err(error) => tls_io_error(error),
            };
            complete(completions, count, pending, value);
        }
        TlsRequest::Close { stream } => {
            let existed = matches!(entries.get(&stream), Some(Entry::Tls(_)))
                && close_entry(poll, entries, completions, count, stream);
            let value = if existed {
                tls_ok(HostValue::Unit)
            } else {
                tls_closed()
            };
            complete(completions, count, pending, value);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn start_tls_handshake(
    poll: &Poll,
    entries: &mut HashMap<u64, Entry>,
    completions: &ReadySender,
    count: &AtomicUsize,
    retained: &Arc<AtomicUsize>,
    mut pending: Pending,
    stream: u64,
    connection: rustls::Connection,
) {
    let mut state = match entries.remove(&stream) {
        Some(Entry::Stream(state)) => state,
        Some(entry) => {
            entries.insert(stream, entry);
            complete(completions, count, pending, tls_closed());
            return;
        }
        None => {
            complete(completions, count, pending, tls_closed());
            return;
        }
    };
    if state.connect.is_some()
        || !state.reads.is_empty()
        || !state.writes.is_empty()
        || state.read_shutdown
        || state.write_shutdown
    {
        entries.insert(stream, Entry::Stream(state));
        close_entry(poll, entries, completions, count, stream);
        complete(
            completions,
            count,
            pending,
            tls_error(
                CoreCtor::TlsHandshake,
                "the TCP stream cannot start a TLS handshake",
            ),
        );
        return;
    }
    if state.registered {
        let _ = poll.registry().deregister(&mut state.socket);
    }
    let live_retained = RetainedLease {
        budget: Arc::clone(retained),
        bytes: pending.retained,
    };
    pending.retained = 0;
    entries.insert(
        stream,
        Entry::Tls(Box::new(TlsState {
            socket: state.socket,
            connection,
            registered: false,
            handshake: Some(pending),
            reads: VecDeque::new(),
            read_buffer: VecDeque::new(),
            writes: VecDeque::new(),
            shutdowns: VecDeque::new(),
            peer_closed: false,
            socket_eof: false,
            write_shutdown: false,
            close_notify_sent: false,
            _retained: live_retained,
        })),
    );
    drive_tls(poll, entries, completions, count, stream, true, true);
}

fn make_tls_client(settings: TlsClientSettings) -> Result<rustls::ClientConnection, String> {
    validate_tls_client_settings(&settings)?;
    let mut roots = if settings.root_mode == 0 {
        rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned())
    } else if settings.root_mode == 1 {
        rustls::RootCertStore::empty()
    } else {
        return Err("the TLS root mode is invalid".to_string());
    };
    if settings.root_mode == 1 {
        if settings.roots.is_empty() {
            return Err("the custom TLS root list is empty".to_string());
        }
        for bytes in settings.roots {
            let certificate = CertificateDer::from(bytes.as_slice().to_vec());
            if roots.add(certificate).is_err() {
                return Err("a custom TLS root certificate is invalid".to_string());
            }
        }
    }
    let versions: &[&'static rustls::SupportedProtocolVersion] = match settings.minimum_version {
        12 => &[&rustls::version::TLS13, &rustls::version::TLS12],
        13 => &[&rustls::version::TLS13],
        _ => return Err("the minimum TLS version is invalid".to_string()),
    };
    let provider = std::sync::Arc::new(rustls::crypto::ring::default_provider());
    let builder = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(versions)
        .map_err(bounded)?;
    let mut config = builder.with_root_certificates(roots).with_no_client_auth();
    config.alpn_protocols = settings
        .alpn
        .into_iter()
        .map(|bytes| bytes.as_slice().to_vec())
        .collect();
    let name = ServerName::try_from(settings.server_name)
        .map_err(|_| "the TLS server name is invalid".to_string())?;
    let mut connection =
        rustls::ClientConnection::new(std::sync::Arc::new(config), name).map_err(bounded)?;
    connection.set_buffer_limit(Some(settings.buffer_limit));
    Ok(connection)
}

fn make_tls_server(settings: TlsServerSettings) -> Result<rustls::ServerConnection, String> {
    validate_tls_server_settings(&settings)?;
    let versions: &[&'static rustls::SupportedProtocolVersion] = match settings.minimum_version {
        12 => &[&rustls::version::TLS13, &rustls::version::TLS12],
        13 => &[&rustls::version::TLS13],
        _ => return Err("the minimum TLS version is invalid".to_string()),
    };
    let provider = std::sync::Arc::new(rustls::crypto::ring::default_provider());
    let builder = rustls::ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(versions)
        .map_err(bounded)?;
    let certificates = settings
        .certificates
        .into_iter()
        .map(|bytes| CertificateDer::from(bytes.as_slice().to_vec()))
        .collect();
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
        settings.private_key.as_slice().to_vec(),
    ));
    let mut config = builder
        .with_no_client_auth()
        .with_single_cert(certificates, key)
        .map_err(|_| "the TLS certificate or private key is invalid".to_string())?;
    config.alpn_protocols = settings
        .alpn
        .into_iter()
        .map(|bytes| bytes.as_slice().to_vec())
        .collect();
    let mut connection =
        rustls::ServerConnection::new(std::sync::Arc::new(config)).map_err(bounded)?;
    connection.set_buffer_limit(Some(settings.buffer_limit));
    Ok(connection)
}

fn validate_tls_client_settings(settings: &TlsClientSettings) -> Result<(), String> {
    let name = settings.server_name.as_bytes();
    if name.is_empty() || name.len() > 253 || name.iter().any(|byte| *byte <= 32 || *byte >= 127) {
        return Err("the TLS server name is invalid".to_string());
    }
    if settings.root_mode != 0 && settings.root_mode != 1 {
        return Err("the TLS root mode is invalid".to_string());
    }
    if settings.root_mode == 0 && !settings.roots.is_empty() {
        return Err("the WebPKI root mode has custom roots".to_string());
    }
    if settings.root_mode == 1 && (settings.roots.is_empty() || settings.roots.len() > 128) {
        return Err("the custom TLS root list size is invalid".to_string());
    }
    let mut root_bytes = 0_usize;
    for root in &settings.roots {
        if root.is_empty() || root.len() > 1_048_576 {
            return Err("a custom TLS root certificate size is invalid".to_string());
        }
        root_bytes = root_bytes
            .checked_add(root.len())
            .ok_or_else(|| "the custom TLS root data is too large".to_string())?;
        if root_bytes > 4_194_304 {
            return Err("the custom TLS root data is too large".to_string());
        }
    }
    if settings.alpn.len() > 32 {
        return Err("the TLS ALPN list is too large".to_string());
    }
    let mut alpn_bytes = 0_usize;
    for protocol in &settings.alpn {
        if protocol.is_empty() || protocol.len() > 255 {
            return Err("a TLS ALPN value has an invalid length".to_string());
        }
        alpn_bytes = alpn_bytes
            .checked_add(protocol.len())
            .ok_or_else(|| "the TLS ALPN data is too large".to_string())?;
        if alpn_bytes > 4_096 {
            return Err("the TLS ALPN data is too large".to_string());
        }
    }
    if !matches!(settings.minimum_version, 12 | 13) {
        return Err("the minimum TLS version is invalid".to_string());
    }
    if settings.buffer_limit == 0 || settings.buffer_limit > 1_048_576 {
        return Err("the TLS buffer limit is invalid".to_string());
    }
    Ok(())
}

fn validate_tls_server_settings(settings: &TlsServerSettings) -> Result<(), String> {
    if settings.certificates.is_empty() || settings.certificates.len() > 128 {
        return Err("the TLS certificate list size is invalid".to_string());
    }
    let mut certificate_bytes = 0_usize;
    for certificate in &settings.certificates {
        if certificate.is_empty() || certificate.len() > 1_048_576 {
            return Err("a TLS certificate size is invalid".to_string());
        }
        certificate_bytes = certificate_bytes
            .checked_add(certificate.len())
            .ok_or_else(|| "the TLS certificate data is too large".to_string())?;
        if certificate_bytes > 4_194_304 {
            return Err("the TLS certificate data is too large".to_string());
        }
    }
    if settings.private_key.is_empty() || settings.private_key.len() > 1_048_576 {
        return Err("the TLS private key size is invalid".to_string());
    }
    if settings.alpn.len() > 32 {
        return Err("the TLS ALPN list is too large".to_string());
    }
    let mut alpn_bytes = 0_usize;
    for protocol in &settings.alpn {
        if protocol.is_empty() || protocol.len() > 255 {
            return Err("a TLS ALPN value has an invalid length".to_string());
        }
        alpn_bytes = alpn_bytes
            .checked_add(protocol.len())
            .ok_or_else(|| "the TLS ALPN data is too large".to_string())?;
        if alpn_bytes > 4_096 {
            return Err("the TLS ALPN data is too large".to_string());
        }
    }
    if !matches!(settings.minimum_version, 12 | 13) {
        return Err("the minimum TLS version is invalid".to_string());
    }
    if settings.buffer_limit == 0 || settings.buffer_limit > 1_048_576 {
        return Err("the TLS buffer limit is invalid".to_string());
    }
    Ok(())
}

fn drive_entry(
    poll: &Poll,
    entries: &mut HashMap<u64, Entry>,
    completions: &ReadySender,
    count: &AtomicUsize,
    resource: u64,
    readable: bool,
    writable: bool,
) {
    match entries.get(&resource) {
        Some(Entry::Stream(_)) => drive_stream(
            poll,
            entries,
            completions,
            count,
            resource,
            readable,
            writable,
        ),
        Some(Entry::Listener(_)) => drive_listener(poll, entries, completions, count, resource),
        Some(Entry::Tls(_)) => drive_tls(
            poll,
            entries,
            completions,
            count,
            resource,
            readable,
            writable,
        ),
        Some(Entry::Udp(_)) => drive_udp(
            poll,
            entries,
            completions,
            count,
            resource,
            readable,
            writable,
        ),
        None => {}
    }
}

fn complete_udp_receive(
    completions: &ReadySender,
    count: &AtomicUsize,
    mut pending: Pending,
    socket: u64,
    bytes: SharedBytes,
    peer: SocketAddr,
    queued: Option<QueuedDatagram>,
) -> Option<QueuedDatagram> {
    let rollback = if pending.wait_state.is_some() {
        let datagram = match queued {
            Some(datagram) => datagram,
            None => {
                let held = bytes.retained_capacity();
                debug_assert!(held <= pending.retained);
                pending.retained = pending.retained.saturating_sub(held);
                QueuedDatagram {
                    bytes: bytes.clone(),
                    peer,
                    _retained: RetainedLease {
                        budget: Arc::clone(&pending.retained_budget),
                        bytes: held,
                    },
                }
            }
        };
        NetworkWaitRollback::UdpRecv { socket, datagram }
    } else {
        NetworkWaitRollback::None
    };
    let cancelled = complete_with_rollback(
        completions,
        count,
        pending,
        net_ok(HostValue::Ctor(
            CoreCtor::UdpDatagram,
            vec![
                HostValue::Bytes(bytes),
                HostValue::SocketAddress(host_address(peer)),
            ],
        )),
        rollback,
    );
    match cancelled {
        Some(NetworkWaitRollback::UdpRecv { datagram, .. }) => Some(datagram),
        _ => None,
    }
}

fn drive_udp(
    poll: &Poll,
    entries: &mut HashMap<u64, Entry>,
    completions: &ReadySender,
    count: &AtomicUsize,
    socket: u64,
    readable: bool,
    writable: bool,
) {
    let Some(Entry::Udp(state)) = entries.get_mut(&socket) else {
        return;
    };
    if writable {
        while let Some(write) = state.writes.pop_front() {
            match state.socket.send_to(write.bytes.as_slice(), write.address) {
                Ok(written) if written == write.bytes.len() => {
                    complete(completions, count, write.pending, net_ok(HostValue::Unit));
                }
                Ok(_) => complete(
                    completions,
                    count,
                    write.pending,
                    net_error(CoreCtor::NetFailed, "the UDP send was incomplete"),
                ),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    state.writes.push_front(write);
                    break;
                }
                Err(error) => complete(completions, count, write.pending, io_error(error)),
            }
        }
    }
    while let Some(pending) = state.receives.pop_front() {
        if let Some(datagram) = state.queued.pop_front() {
            let bytes = datagram.bytes.clone();
            let peer = datagram.peer;
            if let Some(restored) = complete_udp_receive(
                completions,
                count,
                pending,
                socket,
                bytes,
                peer,
                Some(datagram),
            ) {
                state.queued.push_front(restored);
            }
            continue;
        }
        if !readable {
            state.receives.push_front(pending);
            break;
        }
        let mut buffer = vec![0_u8; MAX_UDP_DATAGRAM_BYTES];
        match state.socket.recv_from(&mut buffer) {
            Ok((read, peer)) => {
                buffer.truncate(read);
                buffer.shrink_to_fit();
                let bytes: SharedBytes = buffer.into();
                if let Some(restored) =
                    complete_udp_receive(completions, count, pending, socket, bytes, peer, None)
                {
                    state.queued.push_front(restored);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                state.receives.push_front(pending);
                break;
            }
            Err(error) => complete(completions, count, pending, io_error(error)),
        }
    }
    let interest = match (!state.receives.is_empty(), !state.writes.is_empty()) {
        (true, true) => Some(Interest::READABLE | Interest::WRITABLE),
        (true, false) => Some(Interest::READABLE),
        (false, true) => Some(Interest::WRITABLE),
        (false, false) => None,
    };
    let registration_failed = match (state.registered, interest) {
        (true, Some(interest)) => poll
            .registry()
            .reregister(&mut state.socket, Token(socket as usize), interest)
            .is_err(),
        (false, Some(interest)) => {
            let registered = poll
                .registry()
                .register(&mut state.socket, Token(socket as usize), interest)
                .is_ok();
            state.registered = registered;
            !registered
        }
        (true, None) => {
            if poll.registry().deregister(&mut state.socket).is_ok() {
                state.registered = false;
            }
            false
        }
        (false, None) => false,
    };
    if registration_failed {
        fail_udp_entry(poll, entries, completions, count, socket);
    }
}

fn fail_udp_entry(
    poll: &Poll,
    entries: &mut HashMap<u64, Entry>,
    completions: &ReadySender,
    count: &AtomicUsize,
    socket: u64,
) {
    let Some(Entry::Udp(mut state)) = entries.remove(&socket) else {
        return;
    };
    if state.registered {
        let _ = poll.registry().deregister(&mut state.socket);
    }
    for pending in state.receives.drain(..) {
        complete(
            completions,
            count,
            pending,
            net_error(CoreCtor::NetFailed, "the UDP socket registration failed"),
        );
    }
    for write in state.writes.drain(..) {
        complete(
            completions,
            count,
            write.pending,
            net_error(CoreCtor::NetFailed, "the UDP socket registration failed"),
        );
    }
}

fn drive_listener(
    poll: &Poll,
    entries: &mut HashMap<u64, Entry>,
    completions: &ReadySender,
    count: &AtomicUsize,
    listener: u64,
) {
    loop {
        if entries.len() >= MAX_NETWORK_RESOURCES {
            let pending = match entries.get_mut(&listener) {
                Some(Entry::Listener(state)) => match state.accepts.pop_front() {
                    Some((pending, _)) => pending,
                    None => return,
                },
                _ => return,
            };
            complete(
                completions,
                count,
                pending,
                net_error(CoreCtor::NetLimitExceeded, "the socket limit is full"),
            );
            continue;
        }
        let accepted = {
            let Some(Entry::Listener(state)) = entries.get_mut(&listener) else {
                return;
            };
            let Some((pending, stream)) = state.accepts.pop_front() else {
                return;
            };
            let accepted = match state.accepted.pop_front() {
                Some(accepted) => Ok(accepted),
                None => state.socket.accept(),
            };
            match accepted {
                Ok((socket, address)) => match socket.set_nodelay(true) {
                    Ok(()) => Ok((pending, stream, socket, address)),
                    Err(error) => Err((pending, error)),
                },
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    state.accepts.push_front((pending, stream));
                    return;
                }
                Err(error) => Err((pending, error)),
            }
        };
        match accepted {
            Ok((pending, stream, socket, address)) => {
                entries.insert(
                    stream,
                    Entry::Stream(StreamState {
                        socket,
                        registered: false,
                        connect: None,
                        reads: VecDeque::new(),
                        writes: VecDeque::new(),
                        read_buffer: VecDeque::new(),
                        read_shutdown: false,
                        write_shutdown: false,
                    }),
                );
                let rollback_address = address;
                let rollback = complete_with_rollback(
                    completions,
                    count,
                    pending,
                    net_ok(HostValue::Tuple(vec![
                        HostValue::TcpStream(stream),
                        HostValue::SocketAddress(host_address(address)),
                    ])),
                    NetworkWaitRollback::Accept {
                        listener,
                        stream,
                        address: rollback_address,
                    },
                );
                if let Some(rollback) = rollback {
                    let _ = restore_wait(poll, entries, completions, count, rollback);
                }
            }
            Err((pending, error)) => complete(completions, count, pending, io_error(error)),
        }
    }
}

fn drive_stream(
    poll: &Poll,
    entries: &mut HashMap<u64, Entry>,
    completions: &ReadySender,
    count: &AtomicUsize,
    stream: u64,
    readable: bool,
    writable: bool,
) {
    let mut failed = None;
    {
        let Some(Entry::Stream(state)) = entries.get_mut(&stream) else {
            return;
        };
        if state.connect.is_some() && (readable || writable) {
            match state.socket.take_error() {
                Ok(Some(error)) => {
                    let pending = state.connect.take().expect("the connect call exists");
                    complete(completions, count, pending, io_error(error));
                    failed = Some(());
                }
                Ok(None) if state.socket.peer_addr().is_ok() => {
                    let pending = state.connect.take().expect("the connect call exists");
                    let cancelled = complete_with_rollback(
                        completions,
                        count,
                        pending,
                        net_ok(HostValue::TcpStream(stream)),
                        NetworkWaitRollback::Connect { stream },
                    )
                    .is_some();
                    if cancelled {
                        failed = Some(());
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    let pending = state.connect.take().expect("the connect call exists");
                    complete(completions, count, pending, io_error(error));
                    failed = Some(());
                }
            }
        }
        if state.connect.is_none() && failed.is_none() && readable {
            while let Some((pending, size)) = state.reads.pop_front() {
                if !state.read_buffer.is_empty() {
                    let take = size.min(state.read_buffer.len());
                    let bytes: Vec<u8> = state.read_buffer.drain(..take).collect();
                    let rollback = if pending.wait_state.is_some() {
                        NetworkWaitRollback::TcpRead {
                            stream,
                            bytes: bytes.clone(),
                        }
                    } else {
                        NetworkWaitRollback::None
                    };
                    let cancelled = complete_with_rollback(
                        completions,
                        count,
                        pending,
                        net_ok(HostValue::Ctor(
                            CoreCtor::TcpReadData,
                            vec![HostValue::Bytes(bytes.into())],
                        )),
                        rollback,
                    );
                    if let Some(NetworkWaitRollback::TcpRead { bytes, .. }) = cancelled {
                        for byte in bytes.into_iter().rev() {
                            state.read_buffer.push_front(byte);
                        }
                    }
                    continue;
                }
                let mut bytes = vec![0; size];
                match state.socket.read(&mut bytes) {
                    Ok(0) => complete(
                        completions,
                        count,
                        pending,
                        net_ok(HostValue::Ctor(CoreCtor::TcpReadEnd, vec![])),
                    ),
                    Ok(read) => {
                        bytes.truncate(read);
                        let rollback = if pending.wait_state.is_some() {
                            NetworkWaitRollback::TcpRead {
                                stream,
                                bytes: bytes.clone(),
                            }
                        } else {
                            NetworkWaitRollback::None
                        };
                        let cancelled = complete_with_rollback(
                            completions,
                            count,
                            pending,
                            net_ok(HostValue::Ctor(
                                CoreCtor::TcpReadData,
                                vec![HostValue::Bytes(bytes.into())],
                            )),
                            rollback,
                        );
                        if let Some(NetworkWaitRollback::TcpRead { bytes, .. }) = cancelled {
                            for byte in bytes.into_iter().rev() {
                                state.read_buffer.push_front(byte);
                            }
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        state.reads.push_front((pending, size));
                        break;
                    }
                    Err(error) => complete(completions, count, pending, io_error(error)),
                }
            }
        }
        if state.connect.is_none() && failed.is_none() && writable {
            while let Some(write) = state.writes.pop_front() {
                match state.socket.write(&write.bytes) {
                    Ok(0) if !write.bytes.is_empty() => complete(
                        completions,
                        count,
                        write.pending,
                        net_error(CoreCtor::NetFailed, "the TCP write made no progress"),
                    ),
                    Ok(written) => complete(
                        completions,
                        count,
                        write.pending,
                        net_ok(HostValue::Int(written as i64)),
                    ),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        state.writes.push_front(write);
                        break;
                    }
                    Err(error) => complete(completions, count, write.pending, io_error(error)),
                }
            }
        }
    }
    if failed.is_some() {
        close_entry(poll, entries, completions, count, stream);
        return;
    }
    let update_failed = if let Some(Entry::Stream(state)) = entries.get_mut(&stream) {
        let wants_read = state.connect.is_some() || !state.reads.is_empty();
        let wants_write = state.connect.is_some() || !state.writes.is_empty();
        let interest = match (wants_read, wants_write) {
            (true, true) => Some(Interest::READABLE | Interest::WRITABLE),
            (true, false) => Some(Interest::READABLE),
            (false, true) => Some(Interest::WRITABLE),
            (false, false) => None,
        };
        match (state.registered, interest) {
            (true, Some(interest)) => poll
                .registry()
                .reregister(&mut state.socket, Token(stream as usize), interest)
                .is_err(),
            (false, Some(interest)) => {
                let failed = poll
                    .registry()
                    .register(&mut state.socket, Token(stream as usize), interest)
                    .is_err();
                state.registered = !failed;
                failed
            }
            (true, None) => {
                let failed = poll.registry().deregister(&mut state.socket).is_err();
                state.registered = failed;
                failed
            }
            (false, None) => false,
        }
    } else {
        false
    };
    if update_failed {
        close_entry(poll, entries, completions, count, stream);
    }
}

fn drive_tls(
    poll: &Poll,
    entries: &mut HashMap<u64, Entry>,
    completions: &ReadySender,
    count: &AtomicUsize,
    stream: u64,
    readable: bool,
    writable: bool,
) {
    let mut fatal = None;
    {
        let Some(Entry::Tls(state)) = entries.get_mut(&stream) else {
            return;
        };
        if readable {
            loop {
                match state.connection.read_tls(&mut state.socket) {
                    Ok(0) => {
                        state.socket_eof = true;
                        match state.connection.process_new_packets() {
                            Ok(io) => state.peer_closed |= io.peer_has_closed(),
                            Err(error) => {
                                fatal = Some(tls_rustls_error(error, state.handshake.is_some()))
                            }
                        }
                        if fatal.is_none() && state.handshake.is_some() {
                            fatal = Some(tls_error(
                                CoreCtor::TlsHandshake,
                                "the peer closed during the TLS handshake",
                            ));
                        }
                        break;
                    }
                    Ok(_) => match state.connection.process_new_packets() {
                        Ok(io) => state.peer_closed |= io.peer_has_closed(),
                        Err(error) => {
                            fatal = Some(tls_rustls_error(error, state.handshake.is_some()));
                            break;
                        }
                    },
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(error) => {
                        fatal = Some(tls_io_error(error));
                        break;
                    }
                }
            }
        }
        if fatal.is_none() && !state.connection.is_handshaking() && state.handshake.is_none() {
            while let Some(write) = state.writes.pop_front() {
                match state.connection.writer().write(write.bytes.as_slice()) {
                    Ok(0) => {
                        state.writes.push_front(write);
                        break;
                    }
                    Ok(written) => complete(
                        completions,
                        count,
                        write.pending,
                        tls_ok(HostValue::Int(written as i64)),
                    ),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        state.writes.push_front(write);
                        break;
                    }
                    Err(error) => {
                        fatal = Some(tls_io_error(error));
                        break;
                    }
                }
            }
            while fatal.is_none() {
                let Some((pending, size)) = state.reads.pop_front() else {
                    break;
                };
                if !state.read_buffer.is_empty() {
                    let take = size.min(state.read_buffer.len());
                    let bytes: Vec<u8> = state.read_buffer.drain(..take).collect();
                    let rollback = if pending.wait_state.is_some() {
                        NetworkWaitRollback::TlsRead {
                            stream,
                            bytes: bytes.clone(),
                        }
                    } else {
                        NetworkWaitRollback::None
                    };
                    let cancelled = complete_with_rollback(
                        completions,
                        count,
                        pending,
                        tls_ok(HostValue::Ctor(
                            CoreCtor::TcpReadData,
                            vec![HostValue::Bytes(bytes.into())],
                        )),
                        rollback,
                    );
                    if let Some(NetworkWaitRollback::TlsRead { bytes, .. }) = cancelled {
                        for byte in bytes.into_iter().rev() {
                            state.read_buffer.push_front(byte);
                        }
                    }
                    continue;
                }
                let mut bytes = vec![0; size];
                match state.connection.reader().read(&mut bytes) {
                    Ok(0) if state.peer_closed => complete(
                        completions,
                        count,
                        pending,
                        tls_ok(HostValue::Ctor(CoreCtor::TcpReadEnd, vec![])),
                    ),
                    Ok(0) if state.socket_eof => {
                        state.reads.push_front((pending, size));
                        fatal = Some(tls_error(
                            CoreCtor::TlsProtocol,
                            "the TLS peer closed without close-notify",
                        ));
                    }
                    Ok(0) => {
                        state.reads.push_front((pending, size));
                        break;
                    }
                    Ok(read) => {
                        bytes.truncate(read);
                        let rollback = if pending.wait_state.is_some() {
                            NetworkWaitRollback::TlsRead {
                                stream,
                                bytes: bytes.clone(),
                            }
                        } else {
                            NetworkWaitRollback::None
                        };
                        let cancelled = complete_with_rollback(
                            completions,
                            count,
                            pending,
                            tls_ok(HostValue::Ctor(
                                CoreCtor::TcpReadData,
                                vec![HostValue::Bytes(bytes.into())],
                            )),
                            rollback,
                        );
                        if let Some(NetworkWaitRollback::TlsRead { bytes, .. }) = cancelled {
                            for byte in bytes.into_iter().rev() {
                                state.read_buffer.push_front(byte);
                            }
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        state.reads.push_front((pending, size));
                        break;
                    }
                    Err(error) => {
                        state.reads.push_front((pending, size));
                        fatal = Some(tls_error(CoreCtor::TlsProtocol, bounded(error)));
                    }
                }
            }
        }
        if fatal.is_none()
            && state.write_shutdown
            && !state.close_notify_sent
            && state.writes.is_empty()
        {
            state.connection.send_close_notify();
            state.close_notify_sent = true;
        }
        if fatal.is_none() && writable {
            while state.connection.wants_write() {
                match state.connection.write_tls(&mut state.socket) {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(error) => {
                        fatal = Some(tls_io_error(error));
                        break;
                    }
                }
            }
        }
        if fatal.is_none() && !state.connection.is_handshaking() && !state.connection.wants_write()
        {
            if let Some(handshake) = state.handshake.take() {
                complete(
                    completions,
                    count,
                    handshake,
                    tls_ok(HostValue::TlsStream(stream)),
                );
            }
        }
        if fatal.is_none()
            && state.write_shutdown
            && state.close_notify_sent
            && !state.connection.wants_write()
            && !state.shutdowns.is_empty()
        {
            let value = match state.socket.shutdown(Shutdown::Write) {
                Ok(()) => tls_ok(HostValue::Unit),
                Err(error) if error.kind() == std::io::ErrorKind::NotConnected => {
                    tls_ok(HostValue::Unit)
                }
                Err(error) => tls_io_error(error),
            };
            for pending in state.shutdowns.drain(..) {
                complete(completions, count, pending, value.clone());
            }
        }
    }
    if let Some(value) = fatal {
        fail_tls_entry(poll, entries, completions, count, stream, value);
        return;
    }
    let update_failed = if let Some(Entry::Tls(state)) = entries.get_mut(&stream) {
        let wants_read = state.handshake.is_some() || !state.reads.is_empty();
        let wants_write = state.connection.wants_write()
            || !state.writes.is_empty()
            || !state.shutdowns.is_empty();
        let interest = match (wants_read, wants_write) {
            (true, true) => Some(Interest::READABLE | Interest::WRITABLE),
            (true, false) => Some(Interest::READABLE),
            (false, true) => Some(Interest::WRITABLE),
            (false, false) => None,
        };
        match (state.registered, interest) {
            (true, Some(interest)) => poll
                .registry()
                .reregister(&mut state.socket, Token(stream as usize), interest)
                .is_err(),
            (false, Some(interest)) => {
                let failed = poll
                    .registry()
                    .register(&mut state.socket, Token(stream as usize), interest)
                    .is_err();
                state.registered = !failed;
                failed
            }
            (true, None) => {
                let failed = poll.registry().deregister(&mut state.socket).is_err();
                state.registered = failed;
                failed
            }
            (false, None) => false,
        }
    } else {
        false
    };
    if update_failed {
        fail_tls_entry(
            poll,
            entries,
            completions,
            count,
            stream,
            tls_network_error(CoreCtor::NetFailed, "the TLS socket registration failed"),
        );
    }
}

fn fail_tls_entry(
    poll: &Poll,
    entries: &mut HashMap<u64, Entry>,
    completions: &ReadySender,
    count: &AtomicUsize,
    stream: u64,
    value: HostValue,
) {
    let Some(Entry::Tls(mut state)) = entries.remove(&stream) else {
        return;
    };
    if state.registered {
        let _ = poll.registry().deregister(&mut state.socket);
    }
    if let Some(pending) = state.handshake.take() {
        complete(completions, count, pending, value.clone());
    }
    for (pending, _) in state.reads.drain(..) {
        complete(completions, count, pending, value.clone());
    }
    for write in state.writes.drain(..) {
        complete(completions, count, write.pending, value.clone());
    }
    for pending in state.shutdowns.drain(..) {
        complete(completions, count, pending, value.clone());
    }
}

fn close_entry(
    poll: &Poll,
    entries: &mut HashMap<u64, Entry>,
    completions: &ReadySender,
    count: &AtomicUsize,
    resource: u64,
) -> bool {
    let Some(mut entry) = entries.remove(&resource) else {
        return false;
    };
    match &mut entry {
        Entry::Stream(state) => {
            if state.registered {
                let _ = poll.registry().deregister(&mut state.socket);
            }
            if let Some(pending) = state.connect.take() {
                complete(completions, count, pending, net_closed());
            }
            for (pending, _) in state.reads.drain(..) {
                complete(completions, count, pending, net_closed());
            }
            for write in state.writes.drain(..) {
                complete(completions, count, write.pending, net_closed());
            }
        }
        Entry::Listener(state) => {
            let _ = poll.registry().deregister(&mut state.socket);
            for (pending, _) in state.accepts.drain(..) {
                complete(completions, count, pending, net_closed());
            }
        }
        Entry::Tls(state) => {
            if state.registered {
                let _ = poll.registry().deregister(&mut state.socket);
            }
            if let Some(pending) = state.handshake.take() {
                complete(completions, count, pending, tls_closed());
            }
            for (pending, _) in state.reads.drain(..) {
                complete(completions, count, pending, tls_closed());
            }
            for write in state.writes.drain(..) {
                complete(completions, count, write.pending, tls_closed());
            }
            for pending in state.shutdowns.drain(..) {
                complete(completions, count, pending, tls_closed());
            }
        }
        Entry::Udp(state) => {
            if state.registered {
                let _ = poll.registry().deregister(&mut state.socket);
            }
            for pending in state.receives.drain(..) {
                complete(completions, count, pending, net_closed());
            }
            for write in state.writes.drain(..) {
                complete(completions, count, write.pending, net_closed());
            }
        }
    }
    true
}

fn restore_wait(
    poll: &Poll,
    entries: &mut HashMap<u64, Entry>,
    completions: &ReadySender,
    count: &AtomicUsize,
    rollback: NetworkWaitRollback,
) -> bool {
    match rollback {
        NetworkWaitRollback::None => true,
        NetworkWaitRollback::Connect { stream } => {
            close_entry(poll, entries, completions, count, stream)
        }
        NetworkWaitRollback::Accept {
            listener,
            stream,
            address,
        } => {
            let Some(Entry::Stream(mut state)) = entries.remove(&stream) else {
                return false;
            };
            if state.registered {
                let _ = poll.registry().deregister(&mut state.socket);
            }
            if state.connect.is_some() || !state.reads.is_empty() || !state.writes.is_empty() {
                entries.insert(stream, Entry::Stream(state));
                return false;
            }
            let Some(Entry::Listener(listener)) = entries.get_mut(&listener) else {
                return true;
            };
            listener.accepted.push_front((state.socket, address));
            true
        }
        NetworkWaitRollback::TcpRead { stream, bytes } => {
            let Some(Entry::Stream(state)) = entries.get_mut(&stream) else {
                return true;
            };
            for byte in bytes.into_iter().rev() {
                state.read_buffer.push_front(byte);
            }
            true
        }
        NetworkWaitRollback::TlsRead { stream, bytes } => {
            let Some(Entry::Tls(state)) = entries.get_mut(&stream) else {
                return true;
            };
            for byte in bytes.into_iter().rev() {
                state.read_buffer.push_front(byte);
            }
            true
        }
        NetworkWaitRollback::UdpRecv { socket, datagram } => {
            let Some(Entry::Udp(state)) = entries.get_mut(&socket) else {
                return true;
            };
            state.queued.push_front(datagram);
            true
        }
    }
}

#[derive(Clone, Copy)]
enum CancelToken {
    Missing,
    Removed,
    Close(u64),
}

fn cancel_token(
    entries: &mut HashMap<u64, Entry>,
    count: &AtomicUsize,
    retained: &AtomicUsize,
    token: u64,
) -> CancelToken {
    for (resource, entry) in entries.iter_mut() {
        match entry {
            Entry::Stream(state) => {
                if state
                    .connect
                    .as_ref()
                    .is_some_and(|pending| pending.token == token)
                {
                    let pending = state.connect.take().expect("the connect call exists");
                    release_pending(count);
                    release_retained(retained, pending.retained);
                    return CancelToken::Close(*resource);
                }
                if let Some(at) = state
                    .reads
                    .iter()
                    .position(|(pending, _)| pending.token == token)
                {
                    let (pending, _) = state.reads.remove(at).expect("the read call exists");
                    release_pending(count);
                    release_retained(retained, pending.retained);
                    return CancelToken::Removed;
                }
                if let Some(at) = state
                    .writes
                    .iter()
                    .position(|write| write.pending.token == token)
                {
                    let write = state.writes.remove(at).expect("the write call exists");
                    release_pending(count);
                    release_retained(retained, write.pending.retained);
                    return CancelToken::Removed;
                }
            }
            Entry::Listener(state) => {
                if let Some(at) = state
                    .accepts
                    .iter()
                    .position(|(pending, _)| pending.token == token)
                {
                    let (pending, _) = state.accepts.remove(at).expect("the accept call exists");
                    release_pending(count);
                    release_retained(retained, pending.retained);
                    return CancelToken::Removed;
                }
            }
            Entry::Tls(state) => {
                if state
                    .handshake
                    .as_ref()
                    .is_some_and(|pending| pending.token == token)
                {
                    let pending = state.handshake.take().expect("the handshake call exists");
                    release_pending(count);
                    release_retained(retained, pending.retained);
                    return CancelToken::Close(*resource);
                }
                if let Some(at) = state
                    .reads
                    .iter()
                    .position(|(pending, _)| pending.token == token)
                {
                    let (pending, _) = state.reads.remove(at).expect("the read call exists");
                    release_pending(count);
                    release_retained(retained, pending.retained);
                    return CancelToken::Removed;
                }
                if let Some(at) = state
                    .writes
                    .iter()
                    .position(|write| write.pending.token == token)
                {
                    let write = state.writes.remove(at).expect("the write call exists");
                    release_pending(count);
                    release_retained(retained, write.pending.retained);
                    return CancelToken::Removed;
                }
                if let Some(at) = state
                    .shutdowns
                    .iter()
                    .position(|pending| pending.token == token)
                {
                    let pending = state
                        .shutdowns
                        .remove(at)
                        .expect("the shutdown call exists");
                    release_pending(count);
                    release_retained(retained, pending.retained);
                    return CancelToken::Removed;
                }
            }
            Entry::Udp(state) => {
                if let Some(at) = state
                    .receives
                    .iter()
                    .position(|pending| pending.token == token)
                {
                    let pending = state.receives.remove(at).expect("the UDP receive exists");
                    release_pending(count);
                    release_retained(retained, pending.retained);
                    return CancelToken::Removed;
                }
                if let Some(at) = state
                    .writes
                    .iter()
                    .position(|write| write.pending.token == token)
                {
                    let write = state.writes.remove(at).expect("the UDP send exists");
                    release_pending(count);
                    release_retained(retained, write.pending.retained);
                    return CancelToken::Removed;
                }
            }
        }
    }
    CancelToken::Missing
}

fn socket_address(address: HostSocketAddress) -> SocketAddr {
    match address.ip {
        HostIpAddress::V4(bytes) => SocketAddr::new(IpAddr::V4(bytes.into()), address.port),
        HostIpAddress::V6(bytes) => SocketAddr::V6(std::net::SocketAddrV6::new(
            bytes.into(),
            address.port,
            address.flow_info,
            address.scope_id,
        )),
    }
}

fn bind_listener(address: SocketAddr, backlog: usize) -> std::io::Result<TcpListener> {
    let domain = if address.is_ipv4() {
        Domain::IPV4
    } else {
        Domain::IPV6
    };
    let socket = Socket::new(domain, Type::STREAM, Some(Protocol::TCP))?;
    socket.bind(&address.into())?;
    socket.listen(backlog as i32)?;
    socket.set_nonblocking(true)?;
    let listener: std::net::TcpListener = socket.into();
    Ok(TcpListener::from_std(listener))
}

fn host_address(address: SocketAddr) -> HostSocketAddress {
    match address {
        SocketAddr::V4(address) => HostSocketAddress {
            ip: HostIpAddress::V4(address.ip().octets()),
            port: address.port(),
            flow_info: 0,
            scope_id: 0,
        },
        SocketAddr::V6(address) => HostSocketAddress {
            ip: HostIpAddress::V6(address.ip().octets()),
            port: address.port(),
            flow_info: address.flowinfo(),
            scope_id: address.scope_id(),
        },
    }
}

fn net_ok(value: HostValue) -> HostValue {
    HostValue::Ctor(CoreCtor::Ok, vec![value])
}

fn net_closed() -> HostValue {
    HostValue::Ctor(
        CoreCtor::Err,
        vec![HostValue::Ctor(CoreCtor::NetClosed, vec![])],
    )
}

fn net_error(ctor: CoreCtor, message: impl Into<String>) -> HostValue {
    HostValue::Ctor(
        CoreCtor::Err,
        vec![HostValue::Ctor(
            ctor,
            vec![HostValue::Str(message.into().into())],
        )],
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
            vec![HostValue::Str(message.into().into())],
        )],
    )
}

fn tls_network_error(ctor: CoreCtor, message: impl Into<String>) -> HostValue {
    HostValue::Ctor(
        CoreCtor::Err,
        vec![HostValue::Ctor(
            CoreCtor::TlsNetwork,
            vec![HostValue::Ctor(
                ctor,
                vec![HostValue::Str(message.into().into())],
            )],
        )],
    )
}

fn tls_closed() -> HostValue {
    HostValue::Ctor(
        CoreCtor::Err,
        vec![HostValue::Ctor(CoreCtor::TlsClosed, vec![])],
    )
}

fn bounded(value: impl std::fmt::Display) -> String {
    value.to_string().chars().take(512).collect()
}

fn tls_rustls_error(error: rustls::Error, handshaking: bool) -> HostValue {
    let ctor = if matches!(error, rustls::Error::InvalidCertificate(_)) {
        CoreCtor::TlsCertificate
    } else if handshaking {
        CoreCtor::TlsHandshake
    } else {
        CoreCtor::TlsProtocol
    };
    tls_error(ctor, bounded(error))
}

fn tls_io_error(error: std::io::Error) -> HostValue {
    let ctor = match error.kind() {
        std::io::ErrorKind::PermissionDenied => CoreCtor::NetPermissionDenied,
        std::io::ErrorKind::AddrInUse => CoreCtor::NetAddressInUse,
        std::io::ErrorKind::ConnectionRefused => CoreCtor::NetConnectionRefused,
        std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::BrokenPipe => {
            CoreCtor::NetConnectionReset
        }
        std::io::ErrorKind::NotConnected => CoreCtor::NetNotConnected,
        std::io::ErrorKind::TimedOut => CoreCtor::NetTimedOut,
        std::io::ErrorKind::Unsupported => CoreCtor::NetUnsupported,
        _ => CoreCtor::NetFailed,
    };
    tls_network_error(ctor, bounded(error))
}

fn io_error(error: std::io::Error) -> HostValue {
    let ctor = match error.kind() {
        std::io::ErrorKind::PermissionDenied => CoreCtor::NetPermissionDenied,
        std::io::ErrorKind::AddrInUse => CoreCtor::NetAddressInUse,
        std::io::ErrorKind::ConnectionRefused => CoreCtor::NetConnectionRefused,
        std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::BrokenPipe => {
            CoreCtor::NetConnectionReset
        }
        std::io::ErrorKind::NotConnected => CoreCtor::NetNotConnected,
        std::io::ErrorKind::TimedOut => CoreCtor::NetTimedOut,
        std::io::ErrorKind::Unsupported => CoreCtor::NetUnsupported,
        _ => CoreCtor::NetFailed,
    };
    net_error(ctor, bounded(error))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_network_completion_releases_retained_bytes_once() {
        let retained = Arc::new(AtomicUsize::new(9));
        let completion = NetworkCompletion {
            value: HostCompletion {
                key: CompletionKey {
                    machine: lm_vm::TaskKey {
                        vm: 0,
                        generation: 0,
                    },
                    ordinal: 1,
                },
                token: 3,
                result: Ok(HostValue::Unit),
            },
            _retained: Some(RetainedLease {
                budget: Arc::clone(&retained),
                bytes: 9,
            }),
        };

        assert_eq!(completion.into_value().token, 3);
        assert_eq!(retained.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn retained_bytes_have_one_global_limit() {
        let pending = AtomicUsize::new(0);
        let retained = AtomicUsize::new(0);
        assert!(reserve(&pending, &retained, MAX_RETAINED_NETWORK_BYTES - 1));
        assert!(!reserve(&pending, &retained, 2));
        assert_eq!(pending.load(Ordering::Relaxed), 1);
        assert_eq!(
            retained.load(Ordering::Relaxed),
            MAX_RETAINED_NETWORK_BYTES - 1
        );
        release_pending(&pending);
        release_retained(&retained, MAX_RETAINED_NETWORK_BYTES - 1);
        assert_eq!(pending.load(Ordering::Relaxed), 0);
        assert_eq!(retained.load(Ordering::Relaxed), 0);
    }
}
