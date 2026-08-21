//! Bounded DNS workers and one evented TCP reactor.

use lm_vm::{
    CompletionKey, CoreCtor, HostCompletion, HostIpAddress, HostShutdown, HostSocketAddress,
    HostTcpKind, HostTcpResource, HostValue, SharedBytes,
};
use mio::net::{TcpListener, TcpStream};
use mio::{Events, Interest, Poll, Token, Waker};
use rustls::pki_types::{CertificateDer, ServerName};
use socket2::{Domain, Protocol, Socket, Type};
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{Read, Write};
use std::net::{IpAddr, Shutdown, SocketAddr, ToSocketAddrs};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const WAKE: Token = Token(0);
const DNS_WORKERS: usize = 2;
const MAX_PENDING_NETWORK: usize = 4_096;
const MAX_NETWORK_RESOURCES: usize = 4_096;
const MAX_RETAINED_NETWORK_BYTES: usize = 64 << 20;
const MAX_DNS_RESULTS: usize = 64;
const TLS_CONFIG_OVERHEAD: usize = 256 << 10;

pub(crate) struct NetworkService {
    requests: SyncSender<Command>,
    controls: Sender<Control>,
    wake: Arc<Waker>,
    dns: SyncSender<DnsJob>,
    completions: Receiver<NetworkCompletion>,
    pending: Arc<AtomicUsize>,
    retained: Arc<AtomicUsize>,
    active_dns: Arc<Mutex<HashSet<u64>>>,
    canceled_dns: Arc<Mutex<HashSet<u64>>>,
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

pub(crate) enum TlsRequest {
    Handshake {
        stream: u64,
        settings: TlsClientSettings,
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

struct DnsJob {
    pending: Pending,
    name: String,
    port: u16,
}

#[derive(Clone, Copy)]
struct Pending {
    key: CompletionKey,
    token: u64,
    retained: usize,
}

struct NetworkCompletion {
    value: HostCompletion,
    retained: usize,
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

enum Command {
    Request(Pending, TcpRequest),
    Tls(Pending, TlsRequest),
}

enum Control {
    Cancel(u64),
    ForceClose(HostTcpResource),
    ForceCloseTls(u64),
    Stop,
}

enum Entry {
    Stream(StreamState),
    Listener(ListenerState),
    Tls(Box<TlsState>),
}

struct StreamState {
    socket: TcpStream,
    registered: bool,
    connect: Option<Pending>,
    reads: VecDeque<(Pending, usize)>,
    writes: VecDeque<PendingWrite>,
    read_shutdown: bool,
    write_shutdown: bool,
}

struct ListenerState {
    socket: TcpListener,
    accepts: VecDeque<(Pending, u64)>,
}

struct TlsState {
    socket: TcpStream,
    connection: rustls::ClientConnection,
    registered: bool,
    handshake: Option<Pending>,
    reads: VecDeque<(Pending, usize)>,
    writes: VecDeque<PendingWrite>,
    shutdowns: VecDeque<Pending>,
    peer_closed: bool,
    socket_eof: bool,
    write_shutdown: bool,
    close_notify_sent: bool,
    _retained: RetainedLease,
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

impl TlsRequest {
    fn retained_bytes(&self) -> usize {
        match self {
            TlsRequest::Handshake { settings, .. } => settings.retained_bytes(),
            TlsRequest::Read { count, .. } => *count,
            TlsRequest::Write { bytes, .. } => bytes.retained_capacity(),
            _ => 0,
        }
    }
}

impl NetworkService {
    pub(crate) fn new() -> NetworkService {
        let poll = Poll::new().expect("the network poll starts");
        let wake = Arc::new(Waker::new(poll.registry(), WAKE).expect("the network wake starts"));
        let (request_tx, request_rx) = mpsc::sync_channel(MAX_PENDING_NETWORK);
        let (control_tx, control_rx) = mpsc::channel();
        let (completion_tx, completions) = mpsc::channel();
        let pending = Arc::new(AtomicUsize::new(0));
        let retained = Arc::new(AtomicUsize::new(0));
        let reactor_pending = Arc::clone(&pending);
        let reactor_retained = Arc::clone(&retained);
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
            completions,
            pending,
            retained,
            active_dns,
            canceled_dns,
            reactor: Some(reactor),
        }
    }

    pub(crate) fn submit_dns(
        &self,
        key: CompletionKey,
        token: u64,
        name: String,
        port: u16,
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
            },
            name,
            port,
        };
        match self.dns.try_send(job) {
            Ok(()) => true,
            Err(TrySendError::Full(job)) | Err(TrySendError::Disconnected(job)) => {
                self.active_dns
                    .lock()
                    .expect("the DNS set locks")
                    .remove(&job.pending.token);
                release_pending(&self.pending);
                release_retained(&self.retained, job.pending.retained);
                false
            }
        }
    }

    pub(crate) fn submit_tcp(&self, key: CompletionKey, token: u64, request: TcpRequest) -> bool {
        let retained = request.retained_bytes();
        if !reserve(&self.pending, &self.retained, retained) {
            return false;
        }
        let pending = Pending {
            key,
            token,
            retained,
        };
        match self.requests.try_send(Command::Request(pending, request)) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                release_pending(&self.pending);
                release_retained(&self.retained, retained);
                return false;
            }
        }
        let _ = self.wake.wake();
        true
    }

    pub(crate) fn submit_tls(&self, key: CompletionKey, token: u64, request: TlsRequest) -> bool {
        let retained = request.retained_bytes();
        if !reserve(&self.pending, &self.retained, retained) {
            return false;
        }
        let pending = Pending {
            key,
            token,
            retained,
        };
        match self.requests.try_send(Command::Tls(pending, request)) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
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

    pub(crate) fn poll(&self) -> Option<HostCompletion> {
        let completion = self.completions.try_recv().ok()?;
        release_retained(&self.retained, completion.retained);
        Some(completion.value)
    }

    pub(crate) fn wait_timeout(
        &self,
        duration: Duration,
    ) -> Result<HostCompletion, RecvTimeoutError> {
        self.completions.recv_timeout(duration).map(|completion| {
            release_retained(&self.retained, completion.retained);
            completion.value
        })
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

fn complete(
    completions: &Sender<NetworkCompletion>,
    count: &AtomicUsize,
    pending: Pending,
    value: HostValue,
) {
    let _ = completions.send(NetworkCompletion {
        value: HostCompletion {
            key: pending.key,
            token: pending.token,
            result: Ok(value),
        },
        retained: pending.retained,
    });
    release_pending(count);
}

fn dns_worker(
    jobs: Arc<Mutex<Receiver<DnsJob>>>,
    completions: Sender<NetworkCompletion>,
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
    completions: Sender<NetworkCompletion>,
    pending: Arc<AtomicUsize>,
    retained: Arc<AtomicUsize>,
) {
    let mut events = Events::with_capacity(1_024);
    let mut entries: HashMap<u64, Entry> = HashMap::new();
    loop {
        while let Ok(control) = controls.try_recv() {
            match control {
                Control::Cancel(token) => {
                    if let Some(resource) = cancel_token(&mut entries, &pending, &retained, token) {
                        close_entry(&poll, &mut entries, &completions, &pending, resource);
                    }
                }
                Control::ForceClose(resource) => {
                    close_entry(&poll, &mut entries, &completions, &pending, resource.token);
                }
                Control::ForceCloseTls(stream) => {
                    close_entry(&poll, &mut entries, &completions, &pending, stream);
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
    completions: &Sender<NetworkCompletion>,
    count: &AtomicUsize,
    retained: &Arc<AtomicUsize>,
    command: Command,
) {
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
                        // Nagle holds a small second write until the
                        // peer acknowledges the first. A TLS record
                        // needs more than one write, so the delayed
                        // acknowledgement adds 40 ms to each exchange.
                        //
                        // The option tunes speed alone. A platform
                        // that refuses it still moves every byte, so
                        // this code drops the error on purpose. A
                        // refused connection would be the worse
                        // answer.
                        let _ = socket.set_nodelay(true);
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
    }
}

fn handle_tls_request(
    poll: &Poll,
    entries: &mut HashMap<u64, Entry>,
    completions: &Sender<NetworkCompletion>,
    count: &AtomicUsize,
    retained: &Arc<AtomicUsize>,
    mut pending: Pending,
    request: TlsRequest,
) {
    match request {
        TlsRequest::Handshake { stream, settings } => {
            let connection = match make_tls_client(settings) {
                Ok(connection) => connection,
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

fn drive_entry(
    poll: &Poll,
    entries: &mut HashMap<u64, Entry>,
    completions: &Sender<NetworkCompletion>,
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
        None => {}
    }
}

fn drive_listener(
    _poll: &Poll,
    entries: &mut HashMap<u64, Entry>,
    completions: &Sender<NetworkCompletion>,
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
            match state.socket.accept() {
                Ok((socket, address)) => {
                    // An accepted stream needs the same treatment as
                    // a connected one. The error drops for the same
                    // reason.
                    let _ = socket.set_nodelay(true);
                    Ok((pending, stream, socket, address))
                }
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
                        read_shutdown: false,
                        write_shutdown: false,
                    }),
                );
                complete(
                    completions,
                    count,
                    pending,
                    net_ok(HostValue::Ctor(
                        CoreCtor::Pair,
                        vec![
                            HostValue::TcpStream(stream),
                            HostValue::SocketAddress(host_address(address)),
                        ],
                    )),
                );
            }
            Err((pending, error)) => complete(completions, count, pending, io_error(error)),
        }
    }
}

fn drive_stream(
    poll: &Poll,
    entries: &mut HashMap<u64, Entry>,
    completions: &Sender<NetworkCompletion>,
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
        if let Some(pending) = state.connect {
            if readable || writable {
                match state.socket.take_error() {
                    Ok(Some(error)) => {
                        state.connect = None;
                        complete(completions, count, pending, io_error(error));
                        failed = Some(());
                    }
                    Ok(None) if state.socket.peer_addr().is_ok() => {
                        state.connect = None;
                        complete(
                            completions,
                            count,
                            pending,
                            net_ok(HostValue::TcpStream(stream)),
                        );
                    }
                    Ok(None) => {}
                    Err(error) => {
                        state.connect = None;
                        complete(completions, count, pending, io_error(error));
                        failed = Some(());
                    }
                }
            }
        }
        if state.connect.is_none() && failed.is_none() && readable {
            while let Some((pending, size)) = state.reads.pop_front() {
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
                        complete(
                            completions,
                            count,
                            pending,
                            net_ok(HostValue::Ctor(
                                CoreCtor::TcpReadData,
                                vec![HostValue::Bytes(bytes.into())],
                            )),
                        );
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
    completions: &Sender<NetworkCompletion>,
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
                        complete(
                            completions,
                            count,
                            pending,
                            tls_ok(HostValue::Ctor(
                                CoreCtor::TcpReadData,
                                vec![HostValue::Bytes(bytes.into())],
                            )),
                        );
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
    completions: &Sender<NetworkCompletion>,
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
    completions: &Sender<NetworkCompletion>,
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
    }
    true
}

fn cancel_token(
    entries: &mut HashMap<u64, Entry>,
    count: &AtomicUsize,
    retained: &AtomicUsize,
    token: u64,
) -> Option<u64> {
    for (resource, entry) in entries.iter_mut() {
        match entry {
            Entry::Stream(state) => {
                if state.connect.is_some_and(|pending| pending.token == token) {
                    let pending = state.connect.take().expect("the connect call exists");
                    release_pending(count);
                    release_retained(retained, pending.retained);
                    return Some(*resource);
                }
                if let Some(at) = state
                    .reads
                    .iter()
                    .position(|(pending, _)| pending.token == token)
                {
                    let (pending, _) = state.reads.remove(at).expect("the read call exists");
                    release_pending(count);
                    release_retained(retained, pending.retained);
                    return None;
                }
                if let Some(at) = state
                    .writes
                    .iter()
                    .position(|write| write.pending.token == token)
                {
                    let write = state.writes.remove(at).expect("the write call exists");
                    release_pending(count);
                    release_retained(retained, write.pending.retained);
                    return None;
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
                    return None;
                }
            }
            Entry::Tls(state) => {
                if state
                    .handshake
                    .is_some_and(|pending| pending.token == token)
                {
                    let pending = state.handshake.take().expect("the handshake call exists");
                    release_pending(count);
                    release_retained(retained, pending.retained);
                    return Some(*resource);
                }
                if let Some(at) = state
                    .reads
                    .iter()
                    .position(|(pending, _)| pending.token == token)
                {
                    let (pending, _) = state.reads.remove(at).expect("the read call exists");
                    release_pending(count);
                    release_retained(retained, pending.retained);
                    return None;
                }
                if let Some(at) = state
                    .writes
                    .iter()
                    .position(|write| write.pending.token == token)
                {
                    let write = state.writes.remove(at).expect("the write call exists");
                    release_pending(count);
                    release_retained(retained, write.pending.retained);
                    return None;
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
                    return None;
                }
            }
        }
    }
    None
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
