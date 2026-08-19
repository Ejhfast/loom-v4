//! Bounded DNS workers and one evented TCP reactor.

use lm_vm::{
    CompletionKey, CoreCtor, HostCompletion, HostIpAddress, HostShutdown, HostSocketAddress,
    HostTcpKind, HostTcpResource, HostValue, SharedBytes,
};
use mio::net::{TcpListener, TcpStream};
use mio::{Events, Interest, Poll, Token, Waker};
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
const MAX_DNS_RESULTS: usize = 64;

pub(crate) struct NetworkService {
    commands: Sender<Command>,
    wake: Arc<Waker>,
    dns: SyncSender<DnsJob>,
    completions: Receiver<HostCompletion>,
    pending: Arc<AtomicUsize>,
    active_dns: Arc<Mutex<HashSet<u64>>>,
    canceled_dns: Arc<Mutex<HashSet<u64>>>,
}

pub(crate) enum TcpRequest {
    Connect {
        stream: u64,
        address: HostSocketAddress,
    },
    Listen {
        listener: u64,
        address: HostSocketAddress,
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

struct DnsJob {
    pending: Pending,
    name: String,
    port: u16,
}

#[derive(Clone, Copy)]
struct Pending {
    key: CompletionKey,
    token: u64,
}

struct PendingWrite {
    pending: Pending,
    bytes: SharedBytes,
}

enum Command {
    Request(Pending, TcpRequest),
    Cancel(u64),
    ForceClose(HostTcpResource),
}

enum Entry {
    Stream(StreamState),
    Listener(ListenerState),
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

impl NetworkService {
    pub(crate) fn new() -> NetworkService {
        let poll = Poll::new().expect("the network poll starts");
        let wake = Arc::new(Waker::new(poll.registry(), WAKE).expect("the network wake starts"));
        let (command_tx, command_rx) = mpsc::channel();
        let (completion_tx, completions) = mpsc::channel();
        let pending = Arc::new(AtomicUsize::new(0));
        let reactor_pending = Arc::clone(&pending);
        let reactor_completions = completion_tx.clone();
        std::thread::Builder::new()
            .name("loom-network".to_string())
            .spawn(move || reactor(poll, command_rx, reactor_completions, reactor_pending))
            .expect("the network reactor starts");

        let (dns, dns_rx) = mpsc::sync_channel(MAX_PENDING_NETWORK);
        let dns_rx = Arc::new(Mutex::new(dns_rx));
        let active_dns = Arc::new(Mutex::new(HashSet::new()));
        let canceled_dns = Arc::new(Mutex::new(HashSet::new()));
        for worker in 0..DNS_WORKERS {
            let jobs = Arc::clone(&dns_rx);
            let completions = completion_tx.clone();
            let pending = Arc::clone(&pending);
            let active = Arc::clone(&active_dns);
            let canceled = Arc::clone(&canceled_dns);
            std::thread::Builder::new()
                .name(format!("loom-dns-{worker}"))
                .spawn(move || dns_worker(jobs, completions, pending, active, canceled))
                .expect("the DNS worker starts");
        }

        NetworkService {
            commands: command_tx,
            wake,
            dns,
            completions,
            pending,
            active_dns,
            canceled_dns,
        }
    }

    pub(crate) fn submit_dns(
        &self,
        key: CompletionKey,
        token: u64,
        name: String,
        port: u16,
    ) -> bool {
        if !reserve(&self.pending) {
            return false;
        }
        self.active_dns
            .lock()
            .expect("the DNS set locks")
            .insert(token);
        let job = DnsJob {
            pending: Pending { key, token },
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
                release(&self.pending);
                false
            }
        }
    }

    pub(crate) fn submit_tcp(&self, key: CompletionKey, token: u64, request: TcpRequest) -> bool {
        if !reserve(&self.pending) {
            return false;
        }
        if self
            .commands
            .send(Command::Request(Pending { key, token }, request))
            .is_err()
        {
            release(&self.pending);
            return false;
        }
        let _ = self.wake.wake();
        true
    }

    pub(crate) fn cancel(&self, token: u64) -> bool {
        if self
            .active_dns
            .lock()
            .expect("the DNS set locks")
            .contains(&token)
        {
            self.canceled_dns
                .lock()
                .expect("the DNS cancel set locks")
                .insert(token);
        }
        let sent = self.commands.send(Command::Cancel(token)).is_ok();
        let _ = self.wake.wake();
        sent
    }

    pub(crate) fn force_close(&self, resource: HostTcpResource) -> bool {
        let sent = self.commands.send(Command::ForceClose(resource)).is_ok();
        let _ = self.wake.wake();
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
}

fn reserve(pending: &AtomicUsize) -> bool {
    pending
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
            (count < MAX_PENDING_NETWORK).then_some(count + 1)
        })
        .is_ok()
}

fn release(pending: &AtomicUsize) {
    let previous = pending.fetch_sub(1, Ordering::Relaxed);
    debug_assert!(previous > 0);
}

fn complete(
    completions: &Sender<HostCompletion>,
    count: &AtomicUsize,
    pending: Pending,
    value: HostValue,
) {
    let _ = completions.send(HostCompletion {
        key: pending.key,
        token: pending.token,
        result: Ok(value),
    });
    release(count);
}

fn dns_worker(
    jobs: Arc<Mutex<Receiver<DnsJob>>>,
    completions: Sender<HostCompletion>,
    pending_count: Arc<AtomicUsize>,
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
            release(&pending_count);
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
    commands: Receiver<Command>,
    completions: Sender<HostCompletion>,
    pending: Arc<AtomicUsize>,
) {
    let mut events = Events::with_capacity(1_024);
    let mut entries: HashMap<u64, Entry> = HashMap::new();
    loop {
        while let Ok(command) = commands.try_recv() {
            handle_command(&poll, &mut entries, &completions, &pending, command);
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
    completions: &Sender<HostCompletion>,
    count: &AtomicUsize,
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
            TcpRequest::Listen { listener, address } => {
                if entries.len() >= MAX_NETWORK_RESOURCES {
                    complete(
                        completions,
                        count,
                        pending,
                        net_error(CoreCtor::NetLimitExceeded, "the socket limit is full"),
                    );
                    return;
                }
                match TcpListener::bind(socket_address(address)) {
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
        Command::Cancel(token) => {
            if let Some(resource) = cancel_token(entries, count, token) {
                close_entry(poll, entries, completions, count, resource);
            }
        }
        Command::ForceClose(resource) => {
            close_entry(poll, entries, completions, count, resource.token);
        }
    }
}

fn drive_entry(
    poll: &Poll,
    entries: &mut HashMap<u64, Entry>,
    completions: &Sender<HostCompletion>,
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
        None => {}
    }
}

fn drive_listener(
    _poll: &Poll,
    entries: &mut HashMap<u64, Entry>,
    completions: &Sender<HostCompletion>,
    count: &AtomicUsize,
    listener: u64,
) {
    loop {
        let accepted = {
            let Some(Entry::Listener(state)) = entries.get_mut(&listener) else {
                return;
            };
            let Some((pending, stream)) = state.accepts.pop_front() else {
                return;
            };
            match state.socket.accept() {
                Ok((socket, address)) => Ok((pending, stream, socket, address)),
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
    completions: &Sender<HostCompletion>,
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

fn close_entry(
    poll: &Poll,
    entries: &mut HashMap<u64, Entry>,
    completions: &Sender<HostCompletion>,
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
    }
    true
}

fn cancel_token(entries: &mut HashMap<u64, Entry>, count: &AtomicUsize, token: u64) -> Option<u64> {
    for (resource, entry) in entries.iter_mut() {
        match entry {
            Entry::Stream(state) => {
                if state.connect.is_some_and(|pending| pending.token == token) {
                    state.connect = None;
                    release(count);
                    return Some(*resource);
                }
                if let Some(at) = state
                    .reads
                    .iter()
                    .position(|(pending, _)| pending.token == token)
                {
                    state.reads.remove(at);
                    release(count);
                    return None;
                }
                if let Some(at) = state
                    .writes
                    .iter()
                    .position(|write| write.pending.token == token)
                {
                    state.writes.remove(at);
                    release(count);
                    return None;
                }
            }
            Entry::Listener(state) => {
                if let Some(at) = state
                    .accepts
                    .iter()
                    .position(|(pending, _)| pending.token == token)
                {
                    state.accepts.remove(at);
                    release(count);
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
    net_error(ctor, error.to_string())
}
