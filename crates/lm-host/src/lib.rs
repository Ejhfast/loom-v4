//! Root host operations for the command line.
//!
//! `CliHost` implements the `lm-vm` completion interface over the
//! real process. It handles standard streams, clocks, files, and a
//! seeded deterministic PRNG. `lm-vm` never depends on this crate.
//! `lm-cli` wires it in.
//!
//! `Clock.Sleep` uses the asynchronous completion channel: `start`
//! records a deadline and returns `Waiting`; `poll` completes when
//! the deadline passed; `wait` blocks until it passes. Week 9 makes
//! the channel truly asynchronous; the shape is already in place.

use lm_vm::{
    CompletionKey, CoreCtor, Host, HostArg, HostCompletion, HostOpenOptions, HostSeekFrom,
    HostStart, HostValue,
};
use std::collections::HashMap;
use std::io::{BufRead, Read, Seek, Write};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// The command-line root host.
pub struct CliHost {
    started: Instant,
    rand_state: u64,
    sleeps: HashMap<u64, (CompletionKey, Instant)>,
    next_token: u64,
    files: HashMap<u64, std::fs::File>,
    next_file: u64,
}

impl CliHost {
    /// Create a host. `rand_seed` seeds the deterministic PRNG.
    pub fn new(rand_seed: u64) -> CliHost {
        CliHost {
            started: Instant::now(),
            rand_state: rand_seed.max(1),
            sleeps: HashMap::new(),
            next_token: 1,
            files: HashMap::new(),
            next_file: 1,
        }
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
}

fn io_error(message: String) -> HostValue {
    HostValue::Ctor(
        CoreCtor::Err,
        vec![HostValue::Ctor(
            CoreCtor::IoErrorFailed,
            vec![HostValue::Str(message)],
        )],
    )
}

fn fs_ok(value: HostValue) -> HostValue {
    HostValue::Ctor(CoreCtor::Ok, vec![value])
}

fn fs_error(message: String) -> HostValue {
    HostValue::Ctor(
        CoreCtor::Err,
        vec![HostValue::Ctor(
            CoreCtor::FsErrorFailed,
            vec![HostValue::Str(message)],
        )],
    )
}

const MAX_FILE_IO_BYTES: usize = 16 << 20;

impl Host for CliHost {
    fn start(&mut self, key: CompletionKey, op: u32, args: Vec<HostArg>) -> HostStart {
        match op {
            lm_abi::OP_IO_PRINT => {
                let Some(HostArg::Str(text)) = args.first() else {
                    return HostStart::Failed("Io.Print needs one string".to_string());
                };
                let mut out = std::io::stdout();
                match out.write_all(text.as_bytes()).and_then(|_| out.flush()) {
                    Ok(()) => HostStart::Completed(HostValue::Unit),
                    Err(e) => HostStart::Failed(format!("stdout write failed: {e}")),
                }
            }
            lm_abi::OP_IO_ERROR => {
                let Some(HostArg::Str(text)) = args.first() else {
                    return HostStart::Failed("Io.Error needs one string".to_string());
                };
                let mut out = std::io::stderr();
                match out.write_all(text.as_bytes()).and_then(|_| out.flush()) {
                    Ok(()) => HostStart::Completed(HostValue::Unit),
                    Err(e) => HostStart::Failed(format!("stderr write failed: {e}")),
                }
            }
            lm_abi::OP_IO_READ_LINE => {
                let mut line = String::new();
                let reply = match std::io::stdin().lock().read_line(&mut line) {
                    Ok(0) => {
                        HostValue::Ctor(CoreCtor::Ok, vec![HostValue::Ctor(CoreCtor::None, vec![])])
                    }
                    Ok(_) => {
                        if line.ends_with('\n') {
                            line.pop();
                            if line.ends_with('\r') {
                                line.pop();
                            }
                        }
                        HostValue::Ctor(
                            CoreCtor::Ok,
                            vec![HostValue::Ctor(CoreCtor::Some, vec![HostValue::Str(line)])],
                        )
                    }
                    Err(e) => io_error(format!("stdin read failed: {e}")),
                };
                HostStart::Completed(reply)
            }
            lm_abi::OP_CLOCK_NOW => {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_nanos() as i64)
                    .unwrap_or(0);
                HostStart::Completed(HostValue::Int(now))
            }
            lm_abi::OP_CLOCK_MONOTONIC => {
                HostStart::Completed(HostValue::Int(self.started.elapsed().as_nanos() as i64))
            }
            lm_abi::OP_CLOCK_SLEEP => {
                let Some(HostArg::Int(nanos)) = args.first() else {
                    return HostStart::Failed("Clock.Sleep needs one integer".to_string());
                };
                let nanos = (*nanos).max(0) as u64;
                let token = self.next_token;
                self.next_token += 1;
                self.sleeps
                    .insert(token, (key, Instant::now() + Duration::from_nanos(nanos)));
                HostStart::Waiting(token)
            }
            lm_abi::OP_RAND_INT => {
                let (low, high) = match (args.first(), args.get(1)) {
                    (Some(HostArg::Int(low)), Some(HostArg::Int(high))) => (*low, *high),
                    _ => return HostStart::Failed("Rand.Int needs two integers".to_string()),
                };
                if low >= high {
                    return HostStart::Failed("Rand.Int needs low < high".to_string());
                }
                let span = high.wrapping_sub(low) as u64;
                let value = low.wrapping_add((self.next_rand() % span) as i64);
                HostStart::Completed(HostValue::Int(value))
            }
            lm_abi::OP_FS_OPEN => {
                let (Some(HostArg::Str(path)), Some(HostArg::OpenOptions(options))) =
                    (args.first(), args.get(1))
                else {
                    return HostStart::Failed("Fs.Open needs a path and options".to_string());
                };
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
                    Ok(file) => {
                        let token = self.next_file;
                        let Some(next) = token.checked_add(1) else {
                            return HostStart::Failed(
                                "the file token space is exhausted".to_string(),
                            );
                        };
                        self.next_file = next;
                        self.files.insert(token, file);
                        HostStart::Completed(fs_ok(HostValue::File(token)))
                    }
                    Err(error) => {
                        HostStart::Completed(fs_error(format!("file open failed: {error}")))
                    }
                }
            }
            lm_abi::OP_FS_READ => {
                let (Some(HostArg::File(token)), Some(HostArg::Int(count))) =
                    (args.first(), args.get(1))
                else {
                    return HostStart::Failed("Fs.Read needs a file and count".to_string());
                };
                let Ok(count) = usize::try_from(*count) else {
                    return HostStart::Completed(fs_error(
                        "the read count is negative".to_string(),
                    ));
                };
                if count > MAX_FILE_IO_BYTES {
                    return HostStart::Completed(fs_error(
                        "the read count is too large".to_string(),
                    ));
                }
                let Some(file) = self.files.get_mut(token) else {
                    return HostStart::Completed(fs_error(
                        "the file token is not open".to_string(),
                    ));
                };
                let mut bytes = vec![0; count];
                match file.read(&mut bytes) {
                    Ok(read) => {
                        bytes.truncate(read);
                        HostStart::Completed(fs_ok(HostValue::Bytes(bytes)))
                    }
                    Err(error) => {
                        HostStart::Completed(fs_error(format!("file read failed: {error}")))
                    }
                }
            }
            lm_abi::OP_FS_WRITE => {
                let (Some(HostArg::File(token)), Some(HostArg::Bytes(bytes))) =
                    (args.first(), args.get(1))
                else {
                    return HostStart::Failed("Fs.Write needs a file and bytes".to_string());
                };
                let Some(file) = self.files.get_mut(token) else {
                    return HostStart::Completed(fs_error(
                        "the file token is not open".to_string(),
                    ));
                };
                match file.write(bytes) {
                    Ok(written) => HostStart::Completed(fs_ok(HostValue::Int(written as i64))),
                    Err(error) => {
                        HostStart::Completed(fs_error(format!("file write failed: {error}")))
                    }
                }
            }
            lm_abi::OP_FS_SEEK => {
                let (Some(HostArg::File(token)), Some(HostArg::SeekFrom(from))) =
                    (args.first(), args.get(1))
                else {
                    return HostStart::Failed("Fs.Seek needs a file and origin".to_string());
                };
                let Some(file) = self.files.get_mut(token) else {
                    return HostStart::Completed(fs_error(
                        "the file token is not open".to_string(),
                    ));
                };
                let from = match from {
                    HostSeekFrom::Start(offset) => match u64::try_from(*offset) {
                        Ok(offset) => std::io::SeekFrom::Start(offset),
                        Err(_) => {
                            return HostStart::Completed(fs_error(
                                "the seek position is invalid".to_string(),
                            ))
                        }
                    },
                    HostSeekFrom::Current(offset) => std::io::SeekFrom::Current(*offset),
                    HostSeekFrom::End(offset) => std::io::SeekFrom::End(*offset),
                };
                match file.seek(from) {
                    Ok(position) => match i64::try_from(position) {
                        Ok(position) => HostStart::Completed(fs_ok(HostValue::Int(position))),
                        Err(_) => HostStart::Completed(fs_error(
                            "the seek position is too large".to_string(),
                        )),
                    },
                    Err(error) => {
                        HostStart::Completed(fs_error(format!("file seek failed: {error}")))
                    }
                }
            }
            lm_abi::OP_FS_FLUSH => {
                let Some(HostArg::File(token)) = args.first() else {
                    return HostStart::Failed("Fs.Flush needs a file".to_string());
                };
                let Some(file) = self.files.get_mut(token) else {
                    return HostStart::Completed(fs_error(
                        "the file token is not open".to_string(),
                    ));
                };
                match file.flush() {
                    Ok(()) => HostStart::Completed(fs_ok(HostValue::Unit)),
                    Err(error) => {
                        HostStart::Completed(fs_error(format!("file flush failed: {error}")))
                    }
                }
            }
            lm_abi::OP_FS_CLOSE => {
                let Some(HostArg::File(token)) = args.first() else {
                    return HostStart::Failed("Fs.Close needs a file".to_string());
                };
                if self.files.remove(token).is_none() {
                    return HostStart::Completed(fs_error(
                        "the file token is not open".to_string(),
                    ));
                }
                HostStart::Completed(fs_ok(HostValue::Unit))
            }
            _ => HostStart::Failed(format!(
                "the command-line host does not implement {}",
                lm_abi::op_name(op)
            )),
        }
    }

    fn poll(&mut self) -> Option<HostCompletion> {
        let now = Instant::now();
        let token = self
            .sleeps
            .iter()
            .filter(|(_, (_, deadline))| now >= *deadline)
            .min_by_key(|(token, (_, deadline))| (*deadline, **token))
            .map(|(token, _)| *token)?;
        let (key, _) = self.sleeps.remove(&token)?;
        Some(HostCompletion {
            key,
            token,
            value: HostValue::Unit,
        })
    }

    fn wait(&mut self) -> Option<HostCompletion> {
        let token = self
            .sleeps
            .iter()
            .min_by_key(|(token, (_, deadline))| (*deadline, **token))
            .map(|(token, _)| *token)?;
        let (key, deadline) = self.sleeps.remove(&token)?;
        let now = Instant::now();
        if deadline > now {
            std::thread::sleep(deadline - now);
        }
        Some(HostCompletion {
            key,
            token,
            value: HostValue::Unit,
        })
    }

    fn close_file(&mut self, token: u64) -> bool {
        self.files.remove(&token).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempFile(std::path::PathBuf);

    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn completion() -> CompletionKey {
        CompletionKey {
            machine: lm_vm::TaskKey {
                vm: 0,
                generation: 0,
            },
            ordinal: 1,
        }
    }

    fn fs_ok_value(start: HostStart) -> HostValue {
        match start {
            HostStart::Completed(HostValue::Ctor(CoreCtor::Ok, mut values))
                if values.len() == 1 =>
            {
                values.pop().expect("the success value exists")
            }
            other => panic!("expected one filesystem success, found {other:?}"),
        }
    }

    #[test]
    fn rand_is_deterministic_per_seed() {
        let draw = |seed: u64| -> Vec<HostValue> {
            let mut host = CliHost::new(seed);
            (0..4)
                .map(|_| {
                    match host.start(
                        completion(),
                        lm_abi::OP_RAND_INT,
                        vec![HostArg::Int(0), HostArg::Int(100)],
                    ) {
                        HostStart::Completed(v) => v,
                        other => panic!("unexpected start result: {other:?}"),
                    }
                })
                .collect()
        };
        assert_eq!(draw(7), draw(7));
        assert_ne!(draw(7), draw(8));
    }

    #[test]
    fn rand_rejects_an_empty_range() {
        let mut host = CliHost::new(1);
        let out = host.start(
            completion(),
            lm_abi::OP_RAND_INT,
            vec![HostArg::Int(5), HostArg::Int(5)],
        );
        assert!(matches!(out, HostStart::Failed(_)));
    }

    #[test]
    fn sleep_uses_the_completion_channel() {
        let mut host = CliHost::new(1);
        let token = match host.start(completion(), lm_abi::OP_CLOCK_SLEEP, vec![HostArg::Int(1)]) {
            HostStart::Waiting(token) => token,
            other => panic!("unexpected start result: {other:?}"),
        };
        assert_eq!(host.wait().map(|completion| completion.token), Some(token));
        assert_eq!(host.poll(), None);
    }

    #[test]
    fn files_round_trip_through_the_command_line_host() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time follows the epoch")
            .as_nanos();
        let path = TempFile(
            std::env::temp_dir().join(format!("loom-host-{}-{unique}.tmp", std::process::id())),
        );
        let path_text = path.0.to_string_lossy().into_owned();
        let mut host = CliHost::new(1);

        let token = match fs_ok_value(host.start(
            completion(),
            lm_abi::OP_FS_OPEN,
            vec![
                HostArg::Str(path_text),
                HostArg::OpenOptions(HostOpenOptions::CreateTruncate),
            ],
        )) {
            HostValue::File(token) => token,
            other => panic!("expected a file token, found {other:?}"),
        };
        assert_eq!(
            fs_ok_value(host.start(
                completion(),
                lm_abi::OP_FS_WRITE,
                vec![HostArg::File(token), HostArg::Bytes(b"hello".to_vec())],
            )),
            HostValue::Int(5)
        );
        assert_eq!(
            fs_ok_value(host.start(
                completion(),
                lm_abi::OP_FS_FLUSH,
                vec![HostArg::File(token)],
            )),
            HostValue::Unit
        );
        assert_eq!(
            fs_ok_value(host.start(
                completion(),
                lm_abi::OP_FS_SEEK,
                vec![
                    HostArg::File(token),
                    HostArg::SeekFrom(HostSeekFrom::Start(0))
                ],
            )),
            HostValue::Int(0)
        );
        assert_eq!(
            fs_ok_value(host.start(
                completion(),
                lm_abi::OP_FS_READ,
                vec![HostArg::File(token), HostArg::Int(5)],
            )),
            HostValue::Bytes(b"hello".to_vec())
        );
        assert_eq!(
            fs_ok_value(host.start(
                completion(),
                lm_abi::OP_FS_CLOSE,
                vec![HostArg::File(token)],
            )),
            HostValue::Unit
        );
        assert_eq!(std::fs::read(&path.0).expect("the file reads"), b"hello");
    }
}
