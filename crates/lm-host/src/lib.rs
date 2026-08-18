//! Root host operations for the command line.
//!
//! `CliHost` implements the `lm-vm` completion interface over the
//! real process. It handles standard streams, clocks, files, and a
//! seeded deterministic PRNG. `lm-vm` never depends on this crate.
//! `lm-cli` wires it in.
//!
//! Potentially blocking file and stream work runs in a fixed I/O
//! service. `start` submits work and returns `Waiting`.

mod io_service;

use io_service::{FileRequest, IoService, StreamRequest};
use lm_vm::{CompletionKey, Host, HostArg, HostCompletion, HostStart, HostValue};
use std::collections::HashMap;
use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// The command-line root host.
pub struct CliHost {
    started: Instant,
    rand_state: u64,
    sleeps: HashMap<u64, (CompletionKey, Instant)>,
    next_token: u64,
    next_file: u64,
    io: IoService,
}

impl CliHost {
    /// Create a host. `rand_seed` seeds the deterministic PRNG.
    pub fn new(rand_seed: u64) -> CliHost {
        CliHost {
            started: Instant::now(),
            rand_state: rand_seed.max(1),
            sleeps: HashMap::new(),
            next_token: 1,
            next_file: 1,
            io: IoService::new(),
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

    fn start_file(&mut self, key: CompletionKey, request: FileRequest) -> HostStart {
        let Some(token) = self.take_token() else {
            return HostStart::Failed("the completion token space is exhausted".to_string());
        };
        if self.io.submit_file(key, token, request) {
            HostStart::Waiting(token)
        } else {
            HostStart::Completed(fs_error("the I/O queue is full".to_string()))
        }
    }

    fn start_stream(&mut self, key: CompletionKey, request: StreamRequest) -> HostStart {
        let Some(token) = self.take_token() else {
            return HostStart::Failed("the completion token space is exhausted".to_string());
        };
        if self.io.submit_stream(key, token, request) {
            HostStart::Waiting(token)
        } else {
            HostStart::Failed("the I/O queue is full".to_string())
        }
    }

    fn take_token(&mut self) -> Option<u64> {
        let token = self.next_token;
        self.next_token = token.checked_add(1)?;
        Some(token)
    }
}

fn fs_error(message: String) -> HostValue {
    HostValue::Ctor(
        lm_vm::CoreCtor::Err,
        vec![HostValue::Ctor(
            lm_vm::CoreCtor::FsErrorFailed,
            vec![HostValue::Str(message.into())],
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
                self.start_stream(key, StreamRequest::Print(text.clone()))
            }
            lm_abi::OP_IO_ERROR => {
                let Some(HostArg::Str(text)) = args.first() else {
                    return HostStart::Failed("Io.Error needs one string".to_string());
                };
                self.start_stream(key, StreamRequest::Error(text.clone()))
            }
            lm_abi::OP_IO_READ_LINE => self.start_stream(key, StreamRequest::ReadLine),
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
                let Some(token) = self.take_token() else {
                    return HostStart::Failed(
                        "the completion token space is exhausted".to_string(),
                    );
                };
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
                let file = self.next_file;
                let Some(next) = file.checked_add(1) else {
                    return HostStart::Failed("the file token space is exhausted".to_string());
                };
                self.next_file = next;
                self.start_file(
                    key,
                    FileRequest::Open {
                        file,
                        path: path.to_string(),
                        options: *options,
                    },
                )
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
                self.start_file(
                    key,
                    FileRequest::Read {
                        file: *token,
                        count,
                    },
                )
            }
            lm_abi::OP_FS_WRITE => {
                let (Some(HostArg::File(token)), Some(HostArg::Bytes(bytes))) =
                    (args.first(), args.get(1))
                else {
                    return HostStart::Failed("Fs.Write needs a file and bytes".to_string());
                };
                self.start_file(
                    key,
                    FileRequest::Write {
                        file: *token,
                        bytes: bytes.clone(),
                    },
                )
            }
            lm_abi::OP_FS_SEEK => {
                let (Some(HostArg::File(token)), Some(HostArg::SeekFrom(from))) =
                    (args.first(), args.get(1))
                else {
                    return HostStart::Failed("Fs.Seek needs a file and origin".to_string());
                };
                self.start_file(
                    key,
                    FileRequest::Seek {
                        file: *token,
                        from: *from,
                    },
                )
            }
            lm_abi::OP_FS_FLUSH => {
                let Some(HostArg::File(token)) = args.first() else {
                    return HostStart::Failed("Fs.Flush needs a file".to_string());
                };
                self.start_file(key, FileRequest::Flush { file: *token })
            }
            lm_abi::OP_FS_CLOSE => {
                let Some(HostArg::File(token)) = args.first() else {
                    return HostStart::Failed("Fs.Close needs a file".to_string());
                };
                self.start_file(key, FileRequest::Close { file: *token })
            }
            _ => HostStart::Failed(format!(
                "the command-line host does not implement {}",
                lm_abi::op_name(op)
            )),
        }
    }

    fn poll(&mut self) -> Option<HostCompletion> {
        if let Some(completion) = self.io.poll() {
            return Some(completion);
        }
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
            result: Ok(HostValue::Unit),
        })
    }

    fn wait(&mut self) -> Option<HostCompletion> {
        if let Some(completion) = self.poll() {
            return Some(completion);
        }
        let token = self
            .sleeps
            .iter()
            .min_by_key(|(token, (_, deadline))| (*deadline, **token))
            .map(|(token, (_, deadline))| (*token, *deadline));
        let Some((sleep_token, deadline)) = token else {
            return self.io.wait();
        };
        let duration = deadline.saturating_duration_since(Instant::now());
        match self.io.wait_timeout(duration) {
            Ok(completion) => Some(completion),
            Err(RecvTimeoutError::Timeout) => {
                let (key, _) = self.sleeps.remove(&sleep_token)?;
                Some(HostCompletion {
                    key,
                    token: sleep_token,
                    result: Ok(HostValue::Unit),
                })
            }
            Err(RecvTimeoutError::Disconnected) => None,
        }
    }

    fn close_file(&mut self, token: u64) -> bool {
        self.io.force_close(token)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lm_vm::{CoreCtor, HostOpenOptions, HostSeekFrom};

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

    fn run_host(host: &mut CliHost, op: u32, args: Vec<HostArg>) -> HostValue {
        let token = match host.start(completion(), op, args) {
            HostStart::Completed(value) => return value,
            HostStart::Waiting(token) => token,
            HostStart::Failed(message) => panic!("the host start failed: {message}"),
        };
        let completion = host.wait().expect("the host operation completes");
        assert_eq!(completion.token, token);
        completion.result.expect("the host operation succeeds")
    }

    fn fs_ok_value(value: HostValue) -> HostValue {
        match value {
            HostValue::Ctor(CoreCtor::Ok, mut values) if values.len() == 1 => {
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

        let token = match fs_ok_value(run_host(
            &mut host,
            lm_abi::OP_FS_OPEN,
            vec![
                HostArg::Str(path_text.into()),
                HostArg::OpenOptions(HostOpenOptions::CreateTruncate),
            ],
        )) {
            HostValue::File(token) => token,
            other => panic!("expected a file token, found {other:?}"),
        };
        assert_eq!(
            fs_ok_value(run_host(
                &mut host,
                lm_abi::OP_FS_WRITE,
                vec![HostArg::File(token), HostArg::Bytes(b"hello".into())],
            )),
            HostValue::Int(5)
        );
        assert_eq!(
            fs_ok_value(run_host(
                &mut host,
                lm_abi::OP_FS_FLUSH,
                vec![HostArg::File(token)],
            )),
            HostValue::Unit
        );
        assert_eq!(
            fs_ok_value(run_host(
                &mut host,
                lm_abi::OP_FS_SEEK,
                vec![
                    HostArg::File(token),
                    HostArg::SeekFrom(HostSeekFrom::Start(0))
                ],
            )),
            HostValue::Int(0)
        );
        assert_eq!(
            fs_ok_value(run_host(
                &mut host,
                lm_abi::OP_FS_READ,
                vec![HostArg::File(token), HostArg::Int(5)],
            )),
            HostValue::Bytes(b"hello".into())
        );
        assert_eq!(
            fs_ok_value(run_host(
                &mut host,
                lm_abi::OP_FS_CLOSE,
                vec![HostArg::File(token)],
            )),
            HostValue::Unit
        );
        assert_eq!(std::fs::read(&path.0).expect("the file reads"), b"hello");
    }
}
