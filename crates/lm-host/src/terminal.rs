//! Terminal platform operations.

use lm_vm::HostStdStream;

/// One portable terminal failure.
#[derive(Debug)]
pub(crate) enum TerminalError {
    Closed,
    NotTerminal,
    Busy,
    PermissionDenied(String),
    #[cfg(not(target_os = "linux"))]
    Unsupported(String),
    Failed(String),
}

/// One command-line terminal service.
pub(crate) struct TerminalService {
    next_token: u64,
    raw: Option<RawState>,
    #[cfg(unix)]
    streams: [libc::c_int; 3],
}

#[cfg(unix)]
struct RawState {
    token: u64,
    fd: libc::c_int,
    original: libc::termios,
}

#[cfg(not(unix))]
struct RawState {
    token: u64,
}

impl TerminalService {
    pub(crate) fn new() -> TerminalService {
        TerminalService {
            next_token: 1,
            raw: None,
            #[cfg(unix)]
            streams: [libc::STDIN_FILENO, libc::STDOUT_FILENO, libc::STDERR_FILENO],
        }
    }

    pub(crate) fn is_terminal(&self, stream: HostStdStream) -> bool {
        #[cfg(unix)]
        {
            platform_is_terminal(self.stream_fd(stream))
        }
        #[cfg(not(unix))]
        {
            let _ = stream;
            false
        }
    }

    pub(crate) fn size(&self, stream: HostStdStream) -> Result<(i64, i64), TerminalError> {
        #[cfg(unix)]
        {
            platform_size(self.stream_fd(stream))
        }
        #[cfg(not(unix))]
        {
            let _ = stream;
            Err(TerminalError::Unsupported(
                "terminal size is not supported on this platform".to_string(),
            ))
        }
    }

    pub(crate) fn enter_raw(&mut self) -> Result<u64, TerminalError> {
        if self.raw.is_some() {
            return Err(TerminalError::Busy);
        }
        let token = self
            .next_token
            .checked_add(1)
            .map(|next| {
                let token = self.next_token;
                self.next_token = next;
                token
            })
            .ok_or_else(|| TerminalError::Failed("the raw mode token space is exhausted".into()))?;
        #[cfg(unix)]
        let state = platform_enter_raw(token, self.streams[0])?;
        #[cfg(not(unix))]
        let state = platform_enter_raw(token)?;
        self.raw = Some(state);
        Ok(token)
    }

    pub(crate) fn exit_raw(&mut self, token: u64) -> Result<(), TerminalError> {
        let Some(state) = self.raw.as_ref() else {
            return Err(TerminalError::Closed);
        };
        if state.token != token {
            return Err(TerminalError::Closed);
        }
        platform_restore(state)?;
        self.raw = None;
        Ok(())
    }

    pub(crate) fn force_close(&mut self, token: u64) -> bool {
        self.exit_raw(token).is_ok()
    }

    pub(crate) fn raw_active(&self) -> bool {
        self.raw.is_some()
    }

    pub(crate) fn restore_all(&mut self) {
        if let Some(state) = self.raw.as_ref() {
            let _ = platform_restore(state);
        }
        self.raw = None;
    }

    #[cfg(unix)]
    fn stream_fd(&self, stream: HostStdStream) -> libc::c_int {
        let index = match stream {
            HostStdStream::Input => 0,
            HostStdStream::Output => 1,
            HostStdStream::Error => 2,
        };
        self.streams[index]
    }

    #[cfg(all(test, unix))]
    fn with_streams(streams: [libc::c_int; 3]) -> TerminalService {
        TerminalService {
            next_token: 1,
            raw: None,
            streams,
        }
    }
}

impl Drop for TerminalService {
    fn drop(&mut self) {
        self.restore_all();
    }
}

#[cfg(unix)]
fn platform_is_terminal(fd: libc::c_int) -> bool {
    // `isatty` reads process descriptor state and writes no guest state.
    unsafe { libc::isatty(fd) == 1 }
}

#[cfg(unix)]
fn platform_size(fd: libc::c_int) -> Result<(i64, i64), TerminalError> {
    if !platform_is_terminal(fd) {
        return Err(TerminalError::NotTerminal);
    }
    let mut size = std::mem::MaybeUninit::<libc::winsize>::zeroed();
    // `ioctl` initializes `winsize` for one open terminal descriptor.
    let result = unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, size.as_mut_ptr()) };
    if result != 0 {
        return Err(last_error("terminal size query"));
    }
    // The successful call initialized the complete plain C structure.
    let size = unsafe { size.assume_init() };
    if size.ws_col == 0 || size.ws_row == 0 {
        return Err(TerminalError::Failed(
            "the terminal reported an empty size".to_string(),
        ));
    }
    Ok((i64::from(size.ws_col), i64::from(size.ws_row)))
}

#[cfg(unix)]
fn platform_enter_raw(token: u64, fd: libc::c_int) -> Result<RawState, TerminalError> {
    let mut original = std::mem::MaybeUninit::<libc::termios>::uninit();
    // `tcgetattr` initializes `termios` for the standard input descriptor.
    if unsafe { libc::tcgetattr(fd, original.as_mut_ptr()) } != 0 {
        return Err(last_error("terminal state query"));
    }
    // The successful call initialized the complete plain C structure.
    let original = unsafe { original.assume_init() };
    let mut raw = original;
    // `cfmakeraw` changes only the local copy of the terminal settings.
    unsafe { libc::cfmakeraw(&mut raw) };
    // `tcsetattr` applies the reviewed raw settings to standard input.
    if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) } != 0 {
        return Err(last_error("raw terminal mode entry"));
    }
    Ok(RawState {
        token,
        fd,
        original,
    })
}

#[cfg(not(unix))]
fn platform_enter_raw(_token: u64) -> Result<RawState, TerminalError> {
    Err(TerminalError::Unsupported(
        "raw terminal mode is not supported on this platform".to_string(),
    ))
}

#[cfg(unix)]
fn platform_restore(state: &RawState) -> Result<(), TerminalError> {
    // `tcsetattr` restores the exact settings saved before raw mode.
    if unsafe { libc::tcsetattr(state.fd, libc::TCSANOW, &state.original) } != 0 {
        return Err(last_error("terminal state restoration"));
    }
    Ok(())
}

#[cfg(not(unix))]
fn platform_restore(_state: &RawState) -> Result<(), TerminalError> {
    Err(TerminalError::Unsupported(
        "raw terminal mode is not supported on this platform".to_string(),
    ))
}

#[cfg(unix)]
fn last_error(action: &str) -> TerminalError {
    let error = std::io::Error::last_os_error();
    match error.raw_os_error() {
        Some(libc::ENOTTY) => TerminalError::NotTerminal,
        Some(libc::EACCES) | Some(libc::EPERM) => {
            TerminalError::PermissionDenied(format!("{action} was denied"))
        }
        _ => TerminalError::Failed(format!("{action} failed: {error}")),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::fd::{FromRawFd, OwnedFd};

    fn pseudo_terminal() -> (OwnedFd, OwnedFd) {
        let mut master = -1;
        let mut slave = -1;
        // `openpty` creates one private terminal pair for this test.
        let result = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
            )
        };
        assert_eq!(result, 0, "the pseudo-terminal opens");
        // The successful call returns two owned descriptors.
        unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) }
    }

    fn state(fd: libc::c_int) -> libc::termios {
        let mut value = std::mem::MaybeUninit::<libc::termios>::uninit();
        // `tcgetattr` initializes the complete terminal state.
        assert_eq!(unsafe { libc::tcgetattr(fd, value.as_mut_ptr()) }, 0);
        // The successful call initialized the complete C structure.
        unsafe { value.assume_init() }
    }

    fn assert_same(left: &libc::termios, right: &libc::termios) {
        assert_eq!(left.c_iflag, right.c_iflag);
        assert_eq!(left.c_oflag, right.c_oflag);
        assert_eq!(left.c_cflag, right.c_cflag);
        assert_eq!(left.c_lflag, right.c_lflag);
        assert_eq!(left.c_cc, right.c_cc);
    }

    #[test]
    fn service_drop_restores_the_exact_terminal_state() {
        use std::os::fd::AsRawFd;

        let (_master, slave) = pseudo_terminal();
        let fd = slave.as_raw_fd();
        let original = state(fd);
        {
            let mut service = TerminalService::with_streams([fd, fd, fd]);
            assert!(service.is_terminal(HostStdStream::Input));
            service.enter_raw().expect("raw mode starts");
            let raw = state(fd);
            assert_eq!(raw.c_lflag & libc::ICANON, 0);
            assert_eq!(raw.c_lflag & libc::ECHO, 0);
        }
        assert_same(&original, &state(fd));
    }
}
