//! Cancellation-safe process signal delivery.

use crate::ReadySender;
use lm_vm::{
    CompletionKey, CoreCtor, HostCompletion, HostSignalKind, HostStart, HostValue, HostWaitCancel,
};
use std::collections::{BTreeMap, VecDeque};

/// One signal service failure.
#[derive(Debug)]
pub(crate) enum SignalServiceError {
    Busy,
    #[cfg(not(target_os = "linux"))]
    Unsupported(String),
    Failed(String),
}

#[derive(Debug)]
struct PendingSignal {
    key: CompletionKey,
    stream: u64,
    wait_source: bool,
}

#[derive(Debug)]
enum RetainedSignal {
    Notification(HostSignalKind),
    Closed,
}

/// One command-line signal service.
pub(crate) struct SignalService {
    stream: Option<u64>,
    interrupt: bool,
    terminate: bool,
    queued: VecDeque<HostSignalKind>,
    pending: BTreeMap<u64, PendingSignal>,
    ready: VecDeque<HostCompletion>,
    retained: BTreeMap<u64, RetainedSignal>,
    platform: Option<PlatformSignals>,
    interrupt_seen: bool,
    force_signal: Option<HostSignalKind>,
    notifier: ReadySender,
}

impl SignalService {
    pub(crate) fn guardian(notifier: ReadySender) -> Result<SignalService, SignalServiceError> {
        SignalService::build(None, false, false, notifier)
    }

    pub(crate) fn open(
        stream: u64,
        interrupt: bool,
        terminate: bool,
        notifier: ReadySender,
    ) -> Result<SignalService, SignalServiceError> {
        SignalService::build(Some(stream), interrupt, terminate, notifier)
    }

    fn build(
        stream: Option<u64>,
        interrupt: bool,
        terminate: bool,
        notifier: ReadySender,
    ) -> Result<SignalService, SignalServiceError> {
        let platform = PlatformSignals::open(true, true, notifier.clone())?;
        Ok(SignalService {
            stream,
            interrupt,
            terminate,
            queued: VecDeque::new(),
            pending: BTreeMap::new(),
            ready: VecDeque::new(),
            retained: BTreeMap::new(),
            platform: Some(platform),
            interrupt_seen: false,
            force_signal: None,
            notifier,
        })
    }

    pub(crate) fn has_stream(&self) -> bool {
        self.stream.is_some()
    }

    pub(crate) fn is_idle(&self) -> bool {
        self.pending.is_empty() && self.ready.is_empty() && self.retained.is_empty()
    }

    pub(crate) fn attach(&mut self, stream: u64, interrupt: bool, terminate: bool) -> bool {
        self.observe();
        if self.has_stream() || !self.is_idle() || self.force_signal.is_some() {
            return false;
        }
        self.stream = Some(stream);
        self.interrupt = interrupt;
        self.terminate = terminate;
        self.interrupt_seen = false;
        self.force_signal = None;
        true
    }

    pub(crate) fn start_next(
        &mut self,
        key: CompletionKey,
        token: u64,
        stream: u64,
        wait_source: bool,
    ) -> HostStart {
        if Some(stream) != self.stream {
            return HostStart::Completed(signal_error(CoreCtor::SignalClosed, None));
        }
        self.observe();
        if !wait_source {
            if let Some(kind) = self.queued.pop_front() {
                return HostStart::Completed(signal_value(kind));
            }
        }
        self.pending.insert(
            token,
            PendingSignal {
                key,
                stream,
                wait_source,
            },
        );
        self.dispatch();
        HostStart::Waiting(token)
    }

    pub(crate) fn poll(&mut self) -> Option<HostCompletion> {
        self.observe();
        self.dispatch();
        self.ready.pop_front()
    }

    pub(crate) fn refresh(&mut self) {
        self.observe();
        self.dispatch();
    }

    pub(crate) fn cancel(&mut self, token: u64) -> bool {
        self.pending.remove(&token).is_some()
    }

    pub(crate) fn commit_wait(&mut self, token: u64) -> bool {
        self.retained.remove(&token).is_some()
    }

    pub(crate) fn cancel_wait(&mut self, token: u64) -> HostWaitCancel {
        if self.pending.remove(&token).is_some() {
            return HostWaitCancel::Cancelled;
        }
        let Some(retained) = self.retained.remove(&token) else {
            return HostWaitCancel::Missing;
        };
        self.ready.retain(|completion| completion.token != token);
        if let RetainedSignal::Notification(kind) = retained {
            if !self.queued.contains(&kind) {
                self.queued.push_front(kind);
            }
        }
        HostWaitCancel::ReadyRestored
    }

    pub(crate) fn close(&mut self, stream: u64) -> bool {
        if Some(stream) != self.stream {
            return false;
        }
        self.stream = None;
        self.interrupt = false;
        self.terminate = false;
        self.interrupt_seen = false;
        self.force_signal = None;
        self.queued.clear();
        let closed = signal_error(CoreCtor::SignalClosed, None);
        for completion in &mut self.ready {
            completion.result = Ok(closed.clone());
            if self.retained.contains_key(&completion.token) {
                self.retained
                    .insert(completion.token, RetainedSignal::Closed);
            }
        }
        let pending = std::mem::take(&mut self.pending);
        let added = pending.len();
        for (token, request) in pending {
            if request.wait_source {
                self.retained.insert(token, RetainedSignal::Closed);
            }
            self.ready.push_back(HostCompletion {
                key: request.key,
                token,
                result: Ok(closed.clone()),
            });
        }
        for _ in 0..added {
            if !self.notifier.signal() {
                break;
            }
        }
        true
    }

    pub(crate) fn forced_signal(&self) -> Option<HostSignalKind> {
        self.force_signal
    }

    pub(crate) fn force_signal(&mut self, kind: HostSignalKind) -> ! {
        self.platform = None;
        // Raw terminal state is restored before this call.
        #[cfg(target_os = "linux")]
        unsafe {
            let (signal, status) = match kind {
                HostSignalKind::Interrupt => (libc::SIGINT, 130),
                HostSignalKind::Terminate => (libc::SIGTERM, 143),
            };
            libc::signal(signal, libc::SIG_DFL);
            libc::raise(signal);
            libc::_exit(status);
        }
        #[cfg(not(target_os = "linux"))]
        std::process::exit(match kind {
            HostSignalKind::Interrupt => 130,
            HostSignalKind::Terminate => 143,
        })
    }

    fn observe(&mut self) {
        let Some(platform) = &mut self.platform else {
            return;
        };
        for kind in platform.drain() {
            let requested = match kind {
                HostSignalKind::Interrupt => self.interrupt,
                HostSignalKind::Terminate => self.terminate,
            } && self.stream.is_some();
            if !requested {
                self.force_signal.get_or_insert(kind);
                continue;
            }
            if kind == HostSignalKind::Interrupt {
                if self.interrupt_seen {
                    self.force_signal = Some(HostSignalKind::Interrupt);
                } else {
                    self.interrupt_seen = true;
                }
            }
            if !self.queued.contains(&kind) {
                self.queued.push_back(kind);
            }
        }
    }

    fn dispatch(&mut self) {
        let Some(stream) = self.stream else {
            return;
        };
        let before = self.ready.len();
        while let Some(kind) = self.queued.pop_front() {
            let Some(token) = self
                .pending
                .iter()
                .find_map(|(token, request)| (request.stream == stream).then_some(*token))
            else {
                self.queued.push_front(kind);
                break;
            };
            let request = self
                .pending
                .remove(&token)
                .expect("the pending signal request exists");
            if request.wait_source {
                self.retained
                    .insert(token, RetainedSignal::Notification(kind));
            }
            self.ready.push_back(HostCompletion {
                key: request.key,
                token,
                result: Ok(signal_value(kind)),
            });
        }
        for _ in before..self.ready.len() {
            if !self.notifier.signal() {
                break;
            }
        }
    }
}

fn signal_value(kind: HostSignalKind) -> HostValue {
    let ctor = match kind {
        HostSignalKind::Interrupt => CoreCtor::SignalInterrupt,
        HostSignalKind::Terminate => CoreCtor::SignalTerminate,
    };
    HostValue::Ctor(CoreCtor::Ok, vec![HostValue::Ctor(ctor, Vec::new())])
}

fn signal_error(ctor: CoreCtor, message: Option<String>) -> HostValue {
    let fields = message
        .map(|message| vec![HostValue::Str(message.into())])
        .unwrap_or_default();
    HostValue::Ctor(CoreCtor::Err, vec![HostValue::Ctor(ctor, fields)])
}

#[cfg(target_os = "linux")]
static SIGNAL_LEASE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
#[cfg(target_os = "linux")]
static SIGNAL_WRITE_FD: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);

#[cfg(all(test, target_os = "linux"))]
pub(crate) static SIGNAL_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(target_os = "linux")]
const MAX_FORWARDED_SIGNALS: usize = 256;

#[cfg(target_os = "linux")]
#[derive(Default)]
struct SignalInbox {
    signals: VecDeque<HostSignalKind>,
    notified: bool,
}

#[cfg(target_os = "linux")]
extern "C" fn record_signal(signal: libc::c_int) {
    let byte = match signal {
        libc::SIGINT => 1u8,
        libc::SIGTERM => 2u8,
        _ => return,
    };
    let fd = SIGNAL_WRITE_FD.load(std::sync::atomic::Ordering::Relaxed);
    if fd < 0 {
        return;
    }
    // `write` is async-signal-safe and writes one bounded byte.
    unsafe {
        let _ = libc::write(fd, (&byte as *const u8).cast(), 1);
    }
}

#[cfg(target_os = "linux")]
struct PlatformSignals {
    read_fd: libc::c_int,
    write_fd: libc::c_int,
    old_actions: Vec<(libc::c_int, libc::sigaction)>,
    inbox: std::sync::Arc<std::sync::Mutex<SignalInbox>>,
    forwarder: Option<std::thread::JoinHandle<()>>,
}

#[cfg(not(target_os = "linux"))]
struct PlatformSignals;

#[cfg(target_os = "linux")]
impl PlatformSignals {
    fn open(
        interrupt: bool,
        terminate: bool,
        notifier: ReadySender,
    ) -> Result<PlatformSignals, SignalServiceError> {
        use std::sync::atomic::Ordering;
        if SIGNAL_LEASE
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(SignalServiceError::Busy);
        }
        let mut fds = [-1; 2];
        // The signal handler needs one nonblocking write descriptor.
        if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
            SIGNAL_LEASE.store(false, Ordering::Release);
            return Err(SignalServiceError::Failed(format!(
                "signal pipe creation failed: {}",
                std::io::Error::last_os_error()
            )));
        }
        let write_flags = unsafe { libc::fcntl(fds[1], libc::F_GETFL) };
        if write_flags < 0
            || unsafe { libc::fcntl(fds[1], libc::F_SETFL, write_flags | libc::O_NONBLOCK) } != 0
        {
            unsafe {
                libc::close(fds[0]);
                libc::close(fds[1]);
            }
            SIGNAL_LEASE.store(false, Ordering::Release);
            return Err(SignalServiceError::Failed(format!(
                "signal pipe setup failed: {}",
                std::io::Error::last_os_error()
            )));
        }
        SIGNAL_WRITE_FD.store(fds[1], Ordering::Release);
        let inbox = std::sync::Arc::new(std::sync::Mutex::new(SignalInbox::default()));
        let mut platform = PlatformSignals {
            read_fd: fds[0],
            write_fd: fds[1],
            old_actions: Vec::new(),
            inbox: std::sync::Arc::clone(&inbox),
            forwarder: None,
        };
        for signal in [
            interrupt.then_some(libc::SIGINT),
            terminate.then_some(libc::SIGTERM),
        ]
        .into_iter()
        .flatten()
        {
            if let Err(error) = platform.install(signal) {
                drop(platform);
                return Err(error);
            }
        }
        let read_fd = platform.read_fd;
        let forwarder = std::thread::Builder::new()
            .name("loom-signal".to_string())
            .spawn(move || forward_signals(read_fd, inbox, notifier))
            .map_err(|error| {
                SignalServiceError::Failed(format!("signal forwarder creation failed: {error}"))
            })?;
        platform.forwarder = Some(forwarder);
        Ok(platform)
    }

    fn install(&mut self, signal: libc::c_int) -> Result<(), SignalServiceError> {
        let mut action = unsafe { std::mem::zeroed::<libc::sigaction>() };
        action.sa_sigaction = record_signal as usize;
        action.sa_flags = libc::SA_RESTART;
        // `sigemptyset` initializes the handler mask.
        unsafe { libc::sigemptyset(&mut action.sa_mask) };
        let mut old = unsafe { std::mem::zeroed::<libc::sigaction>() };
        // `sigaction` installs the bounded notification handler.
        if unsafe { libc::sigaction(signal, &action, &mut old) } != 0 {
            return Err(SignalServiceError::Failed(format!(
                "signal handler installation failed: {}",
                std::io::Error::last_os_error()
            )));
        }
        self.old_actions.push((signal, old));
        Ok(())
    }

    fn drain(&mut self) -> Vec<HostSignalKind> {
        let mut inbox = self.inbox.lock().expect("the signal inbox locks");
        inbox.notified = false;
        inbox.signals.drain(..).collect()
    }
}

#[cfg(target_os = "linux")]
fn forward_signals(
    read_fd: libc::c_int,
    inbox: std::sync::Arc<std::sync::Mutex<SignalInbox>>,
    notifier: ReadySender,
) {
    let mut bytes = [0u8; 64];
    loop {
        let count = unsafe {
            libc::read(
                read_fd,
                bytes.as_mut_ptr().cast(),
                bytes.len() as libc::size_t,
            )
        };
        if count == 0 {
            return;
        }
        if count < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return;
        }
        let mut send = false;
        {
            let mut inbox = inbox.lock().expect("the signal inbox locks");
            for byte in &bytes[..count as usize] {
                let kind = match byte {
                    1 => HostSignalKind::Interrupt,
                    2 => HostSignalKind::Terminate,
                    _ => continue,
                };
                if inbox.signals.len() < MAX_FORWARDED_SIGNALS {
                    inbox.signals.push_back(kind);
                }
            }
            if !inbox.signals.is_empty() && !inbox.notified {
                inbox.notified = true;
                send = true;
            }
        }
        if send && !notifier.signal() {
            return;
        }
    }
}

#[cfg(target_os = "linux")]
impl Drop for PlatformSignals {
    fn drop(&mut self) {
        use std::sync::atomic::Ordering;
        SIGNAL_WRITE_FD.store(-1, Ordering::Release);
        for (signal, action) in self.old_actions.drain(..).rev() {
            // `sigaction` restores the disposition saved during open.
            unsafe {
                libc::sigaction(signal, &action, std::ptr::null_mut());
            }
        }
        // Closing the writer wakes the blocking forwarder with EOF.
        unsafe {
            libc::close(self.write_fd);
        }
        if let Some(forwarder) = self.forwarder.take() {
            let _ = forwarder.join();
        }
        unsafe {
            libc::close(self.read_fd);
        }
        SIGNAL_LEASE.store(false, Ordering::Release);
    }
}

#[cfg(not(target_os = "linux"))]
impl PlatformSignals {
    fn open(
        _interrupt: bool,
        _terminate: bool,
        _notifier: ReadySender,
    ) -> Result<PlatformSignals, SignalServiceError> {
        Err(SignalServiceError::Unsupported(
            "signal streams are not supported on this platform".to_string(),
        ))
    }

    fn drain(&mut self) -> Vec<HostSignalKind> {
        Vec::new()
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use lm_vm::TaskKey;

    fn readiness() -> (ReadySender, std::sync::mpsc::Receiver<crate::HostReady>) {
        let (sender, receiver, _, _) = crate::ready_channel();
        (sender, receiver)
    }

    fn wait_for_signal(receiver: &std::sync::mpsc::Receiver<crate::HostReady>) {
        let ready = receiver
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("the signal marker arrives");
        assert!(matches!(ready, crate::HostReady::Signal));
    }

    fn completion() -> CompletionKey {
        CompletionKey {
            machine: TaskKey {
                vm: 0,
                generation: 0,
            },
            ordinal: 1,
        }
    }

    #[test]
    fn a_cancelled_ready_wait_restores_the_platform_signal() {
        let _guard = SIGNAL_TEST_LOCK.lock().expect("the test lock works");
        let (notifier, ready) = readiness();
        let mut service = SignalService::open(9, false, true, notifier).expect("the stream opens");
        assert_eq!(
            service.start_next(completion(), 7, 9, true),
            HostStart::Waiting(7)
        );
        // Call the handler directly because test runners can block process signals.
        record_signal(libc::SIGTERM);
        wait_for_signal(&ready);
        let ready = service.poll().expect("the signal becomes ready");
        assert_eq!(ready.token, 7);
        assert_eq!(service.cancel_wait(7), HostWaitCancel::ReadyRestored);

        let direct = service.start_next(completion(), 8, 9, false);
        assert_eq!(
            direct,
            HostStart::Completed(HostValue::Ctor(
                CoreCtor::Ok,
                vec![HostValue::Ctor(CoreCtor::SignalTerminate, Vec::new())],
            ))
        );
        assert!(service.close(9));
    }

    #[test]
    fn a_guardian_retains_an_unrequested_signal() {
        let _guard = SIGNAL_TEST_LOCK.lock().expect("the test lock works");
        let (notifier, ready) = readiness();
        let mut service = SignalService::guardian(notifier).expect("the guardian opens");
        // Call the handler directly because test runners can block process signals.
        record_signal(libc::SIGTERM);
        wait_for_signal(&ready);
        assert_eq!(service.poll(), None);
        assert_eq!(service.forced_signal(), Some(HostSignalKind::Terminate));
    }

    #[test]
    fn closing_a_stream_completes_its_armed_wait() {
        let _guard = SIGNAL_TEST_LOCK.lock().expect("the test lock works");
        let (notifier, _ready) = readiness();
        let mut service = SignalService::open(9, true, false, notifier).expect("the stream opens");
        assert_eq!(
            service.start_next(completion(), 7, 9, true),
            HostStart::Waiting(7)
        );
        assert!(service.close(9));
        assert_eq!(
            service.poll(),
            Some(HostCompletion {
                key: completion(),
                token: 7,
                result: Ok(signal_error(CoreCtor::SignalClosed, None)),
            })
        );
        assert!(service.commit_wait(7));
        assert!(service.is_idle());
    }
}
