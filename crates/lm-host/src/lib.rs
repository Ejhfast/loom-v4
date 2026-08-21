//! Root host operations for the command line.
//!
//! `CliHost` implements the `lm-vm` completion interface over the
//! real process. It handles standard streams, clocks, files, and a
//! seeded deterministic PRNG. `lm-vm` never depends on this crate.
//! `lm-cli` wires it in.
//!
//! Potentially blocking file and stream work runs in a fixed I/O
//! service. `start` submits work and returns `Waiting`.

mod compiler_service;
mod io_service;
mod network_service;

use compiler_service::{CompileRequest, CompilerService};
use io_service::{FileRequest, IoService, StreamRequest};
use lm_vm::{
    CompletionKey, CoreCtor, Host, HostArg, HostCompletion, HostParseStatus, HostStart,
    HostSyntaxDiagnostic, HostTcpKind, HostTcpResource, HostValue, SharedBytes,
};
use network_service::{NetworkService, TcpRequest, TlsClientSettings, TlsRequest};
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
    next_tcp: u64,
    io: Option<IoService>,
    network: Option<NetworkService>,
    compiler: Option<CompilerService>,
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
            next_tcp: 1,
            io: None,
            network: None,
            compiler: None,
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
        if self.io().submit_file(key, token, request) {
            HostStart::Waiting(token)
        } else {
            HostStart::Completed(fs_error("the I/O queue is full".to_string()))
        }
    }

    fn start_stream(&mut self, key: CompletionKey, request: StreamRequest) -> HostStart {
        let Some(token) = self.take_token() else {
            return HostStart::Failed("the completion token space is exhausted".to_string());
        };
        if self.io().submit_stream(key, token, request) {
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

    fn take_tcp(&mut self) -> Option<u64> {
        let token = self.next_tcp;
        self.next_tcp = token.checked_add(1)?;
        Some(token)
    }

    fn start_dns(&mut self, key: CompletionKey, name: String, port: u16) -> HostStart {
        let Some(token) = self.take_token() else {
            return HostStart::Failed("the completion token space is exhausted".to_string());
        };
        if self.network().submit_dns(key, token, name, port) {
            HostStart::Waiting(token)
        } else {
            HostStart::Completed(net_error(
                CoreCtor::NetLimitExceeded,
                "the DNS queue is full",
            ))
        }
    }

    fn start_tcp(&mut self, key: CompletionKey, request: TcpRequest) -> HostStart {
        let Some(token) = self.take_token() else {
            return HostStart::Failed("the completion token space is exhausted".to_string());
        };
        if self.network().submit_tcp(key, token, request) {
            HostStart::Waiting(token)
        } else {
            HostStart::Completed(net_error(
                CoreCtor::NetLimitExceeded,
                "the network queue is full",
            ))
        }
    }

    fn start_tls(&mut self, key: CompletionKey, request: TlsRequest) -> HostStart {
        let Some(token) = self.take_token() else {
            return HostStart::Failed("the completion token space is exhausted".to_string());
        };
        if self.network().submit_tls(key, token, request) {
            HostStart::Waiting(token)
        } else {
            HostStart::Completed(tls_error(
                CoreCtor::TlsLimitExceeded,
                "the network queue is full",
            ))
        }
    }

    fn network(&mut self) -> &mut NetworkService {
        self.network.get_or_insert_with(NetworkService::new)
    }

    fn io(&mut self) -> &mut IoService {
        self.io.get_or_insert_with(IoService::new)
    }

    fn compiler(&mut self) -> &mut CompilerService {
        self.compiler.get_or_insert_with(CompilerService::new)
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

fn net_error(ctor: CoreCtor, message: impl Into<String>) -> HostValue {
    HostValue::Ctor(
        CoreCtor::Err,
        vec![HostValue::Ctor(
            ctor,
            vec![HostValue::Str(message.into().into())],
        )],
    )
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

fn byte_list(value: &HostArg) -> Option<Vec<SharedBytes>> {
    let HostArg::List(values) = value else {
        return None;
    };
    values
        .iter()
        .map(|value| match value {
            HostArg::Bytes(bytes) => Some(bytes.clone()),
            _ => None,
        })
        .collect()
}

const MAX_FILE_IO_BYTES: usize = 16 << 20;
const MAX_NETWORK_IO_BYTES: usize = 16 << 20;

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
                self.start_dns(key, name.to_string(), port)
            }
            lm_abi::OP_TCP_CONNECT => {
                let Some(HostArg::SocketAddress(address)) = args.first() else {
                    return HostStart::Failed("Tcp.Connect needs an address".to_string());
                };
                let Some(stream) = self.take_tcp() else {
                    return HostStart::Failed("the TCP token space is exhausted".to_string());
                };
                self.start_tcp(
                    key,
                    TcpRequest::Connect {
                        stream,
                        address: *address,
                    },
                )
            }
            lm_abi::OP_TCP_LISTEN => {
                let (Some(HostArg::SocketAddress(address)), Some(HostArg::Int(backlog))) =
                    (args.first(), args.get(1))
                else {
                    return HostStart::Failed(
                        "Tcp.Listen needs an address and backlog".to_string(),
                    );
                };
                if !(1..=65_535).contains(backlog) {
                    return HostStart::Completed(net_error(
                        CoreCtor::NetInvalidInput,
                        "the backlog is outside 1 through 65535",
                    ));
                }
                let Some(listener) = self.take_tcp() else {
                    return HostStart::Failed("the TCP token space is exhausted".to_string());
                };
                self.start_tcp(
                    key,
                    TcpRequest::Listen {
                        listener,
                        address: *address,
                        backlog: *backlog as usize,
                    },
                )
            }
            lm_abi::OP_TCP_ACCEPT => {
                let Some(HostArg::Tcp(resource)) = args.first() else {
                    return HostStart::Failed("Tcp.Accept needs a listener".to_string());
                };
                if resource.kind != HostTcpKind::Listener {
                    return HostStart::Failed("Tcp.Accept needs a listener".to_string());
                }
                let Some(stream) = self.take_tcp() else {
                    return HostStart::Failed("the TCP token space is exhausted".to_string());
                };
                self.start_tcp(
                    key,
                    TcpRequest::Accept {
                        listener: resource.token,
                        stream,
                    },
                )
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
                self.start_tcp(
                    key,
                    TcpRequest::Read {
                        stream: resource.token,
                        count,
                    },
                )
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
                if bytes.len() > MAX_NETWORK_IO_BYTES {
                    return HostStart::Completed(net_error(
                        CoreCtor::NetLimitExceeded,
                        "the write value is too large",
                    ));
                }
                self.start_tcp(
                    key,
                    TcpRequest::Write {
                        stream: resource.token,
                        bytes: bytes.clone(),
                    },
                )
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
                self.start_tcp(
                    key,
                    TcpRequest::Shutdown {
                        stream: resource.token,
                        direction: *direction,
                    },
                )
            }
            lm_abi::OP_TCP_LOCAL_ADDRESS => {
                let Some(HostArg::Tcp(resource)) = args.first() else {
                    return HostStart::Failed("Tcp.LocalAddress needs a resource".to_string());
                };
                self.start_tcp(key, TcpRequest::LocalAddress(*resource))
            }
            lm_abi::OP_TCP_PEER_ADDRESS => {
                let Some(HostArg::Tcp(resource)) = args.first() else {
                    return HostStart::Failed("Tcp.PeerAddress needs a stream".to_string());
                };
                if resource.kind != HostTcpKind::Stream {
                    return HostStart::Failed("Tcp.PeerAddress needs a stream".to_string());
                }
                self.start_tcp(
                    key,
                    TcpRequest::PeerAddress {
                        stream: resource.token,
                    },
                )
            }
            lm_abi::OP_TCP_CLOSE => {
                let Some(HostArg::Tcp(resource)) = args.first() else {
                    return HostStart::Failed("Tcp.Close needs a resource".to_string());
                };
                self.start_tcp(key, TcpRequest::Close(*resource))
            }
            lm_abi::OP_TLS_HANDSHAKE => {
                let (
                    Some(HostArg::Tcp(resource)),
                    Some(HostArg::Str(server_name)),
                    Some(HostArg::Int(root_mode)),
                    Some(roots),
                    Some(alpn),
                    Some(HostArg::Int(minimum_version)),
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
                let (Some(roots), Some(alpn)) = (byte_list(roots), byte_list(alpn)) else {
                    return HostStart::Failed("Tls.Handshake needs byte lists".to_string());
                };
                let Ok(buffer_limit) = usize::try_from(*buffer_limit) else {
                    self.network().force_close(*resource);
                    return HostStart::Completed(tls_error(
                        CoreCtor::TlsInvalidConfig,
                        "the TLS buffer limit is invalid",
                    ));
                };
                if buffer_limit == 0 || buffer_limit > 1_048_576 {
                    self.network().force_close(*resource);
                    return HostStart::Completed(tls_error(
                        CoreCtor::TlsInvalidConfig,
                        "the TLS buffer limit is invalid",
                    ));
                }
                let result = self.start_tls(
                    key,
                    TlsRequest::Handshake {
                        stream: resource.token,
                        settings: TlsClientSettings {
                            server_name: server_name.to_string(),
                            root_mode: *root_mode,
                            roots,
                            alpn,
                            minimum_version: *minimum_version,
                            buffer_limit,
                        },
                    },
                );
                if matches!(&result, HostStart::Completed(_)) {
                    self.network().force_close(*resource);
                }
                result
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
                self.start_tls(
                    key,
                    TlsRequest::Read {
                        stream: *stream,
                        count,
                    },
                )
            }
            lm_abi::OP_TLS_WRITE => {
                let (Some(HostArg::Tls(stream)), Some(HostArg::Bytes(bytes))) =
                    (args.first(), args.get(1))
                else {
                    return HostStart::Failed("Tls.Write needs a stream and bytes".to_string());
                };
                if bytes.len() > MAX_NETWORK_IO_BYTES {
                    return HostStart::Completed(tls_error(
                        CoreCtor::TlsLimitExceeded,
                        "the TLS write value is too large",
                    ));
                }
                self.start_tls(
                    key,
                    TlsRequest::Write {
                        stream: *stream,
                        bytes: bytes.clone(),
                    },
                )
            }
            lm_abi::OP_TLS_SHUTDOWN => {
                let Some(HostArg::Tls(stream)) = args.first() else {
                    return HostStart::Failed("Tls.Shutdown needs a stream".to_string());
                };
                self.start_tls(key, TlsRequest::Shutdown { stream: *stream })
            }
            lm_abi::OP_TLS_LOCAL_ADDRESS => {
                let Some(HostArg::Tls(stream)) = args.first() else {
                    return HostStart::Failed("Tls.LocalAddress needs a stream".to_string());
                };
                self.start_tls(key, TlsRequest::LocalAddress { stream: *stream })
            }
            lm_abi::OP_TLS_PEER_ADDRESS => {
                let Some(HostArg::Tls(stream)) = args.first() else {
                    return HostStart::Failed("Tls.PeerAddress needs a stream".to_string());
                };
                self.start_tls(key, TlsRequest::PeerAddress { stream: *stream })
            }
            lm_abi::OP_TLS_CLOSE => {
                let Some(HostArg::Tls(stream)) = args.first() else {
                    return HostStart::Failed("Tls.Close needs a stream".to_string());
                };
                self.start_tls(key, TlsRequest::Close { stream: *stream })
            }
            lm_abi::OP_COMPILER_COMPILE => {
                let [HostArg::Str(module_name), HostArg::Str(source_name), HostArg::Str(source), HostArg::CompileEnv(env), HostArg::CompileOptions(options)] =
                    args.as_slice()
                else {
                    return HostStart::Failed(
                        "Compiler.Compile needs a module name, source name, source, environment, and options"
                            .to_string(),
                    );
                };
                let Some(token) = self.take_token() else {
                    return HostStart::Failed(
                        "the completion token space is exhausted".to_string(),
                    );
                };
                let request = CompileRequest {
                    module_name: module_name.clone(),
                    source_name: source_name.clone(),
                    source: source.clone(),
                    env: env.clone(),
                    options: options.clone(),
                };
                if self.compiler().submit(key, token, request) {
                    HostStart::Waiting(token)
                } else {
                    HostStart::Failed("the compiler queue is full".to_string())
                }
            }
            lm_abi::OP_COMPILER_COMPILE_SYNTAX => {
                let [HostArg::Str(module_name), HostArg::Str(source_name), HostArg::Syntax {
                    source,
                    records,
                    index,
                }, HostArg::CompileEnv(env), HostArg::CompileOptions(options)] = args.as_slice()
                else {
                    return HostStart::Failed(
                        "Compiler.CompileSyntax needs a module name, source name, syntax, environment, and options"
                            .to_string(),
                    );
                };
                let view = match lm_abi::syntax::SyntaxView::new(records.as_slice(), source.len()) {
                    Ok(view) => view,
                    Err(error) => {
                        return HostStart::Failed(format!("invalid compiler syntax: {error}"));
                    }
                };
                let record = match view.record(*index) {
                    Ok(record)
                        if matches!(
                            record.class,
                            lm_abi::syntax::SyntaxClass::Node
                                | lm_abi::syntax::SyntaxClass::Invalid
                        ) =>
                    {
                        record
                    }
                    _ => {
                        return HostStart::Failed(
                            "Compiler.CompileSyntax needs one syntax node".to_string(),
                        );
                    }
                };
                let Some(source) = source.slice(record.lo as usize, record.hi as usize) else {
                    return HostStart::Failed(
                        "Compiler.CompileSyntax found an invalid source range".to_string(),
                    );
                };
                let Some(token) = self.take_token() else {
                    return HostStart::Failed(
                        "the completion token space is exhausted".to_string(),
                    );
                };
                let request = CompileRequest {
                    module_name: module_name.clone(),
                    source_name: source_name.clone(),
                    source,
                    env: env.clone(),
                    options: options.clone(),
                };
                if self.compiler().submit(key, token, request) {
                    HostStart::Waiting(token)
                } else {
                    HostStart::Failed("the compiler queue is full".to_string())
                }
            }
            lm_abi::OP_REFLECT_PARSE_SYNTAX => {
                let [HostArg::Str(source)] = args.as_slice() else {
                    return HostStart::Failed("Reflect.ParseSyntax needs one string".to_string());
                };
                let parsed = lm_source::syntax::parse_public_syntax(source.as_str());
                let status = match parsed.status {
                    lm_source::syntax::ParseStatus::Complete => HostParseStatus::Complete,
                    lm_source::syntax::ParseStatus::Incomplete => HostParseStatus::Incomplete,
                    lm_source::syntax::ParseStatus::Invalid => HostParseStatus::Invalid,
                };
                let file = lm_source::SourceFile::new("<syntax>", source.as_str());
                let diagnostics = parsed
                    .diagnostics
                    .into_iter()
                    .map(|diagnostic| HostSyntaxDiagnostic {
                        start: diagnostic.span.lo,
                        stop: diagnostic.span.hi,
                        message: diagnostic.render(&file).into(),
                    })
                    .collect();
                HostStart::Completed(HostValue::SyntaxParse {
                    source: source.clone(),
                    records: parsed.records.into(),
                    status,
                    diagnostics,
                })
            }
            _ => HostStart::Failed(format!(
                "the command-line host does not implement {}",
                lm_abi::op_name(op)
            )),
        }
    }

    fn poll(&mut self) -> Option<HostCompletion> {
        if let Some(io) = &self.io {
            if let Some(completion) = io.poll() {
                return Some(completion);
            }
        }
        if let Some(network) = &self.network {
            if let Some(completion) = network.poll() {
                return Some(completion);
            }
        }
        if let Some(compiler) = &self.compiler {
            if let Some(completion) = compiler.poll() {
                return Some(completion);
            }
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
        loop {
            if let Some(completion) = self.poll() {
                return Some(completion);
            }
            let deadline = self.sleeps.values().map(|(_, deadline)| *deadline).min();
            let quantum = Duration::from_millis(10);
            let duration = deadline
                .map(|deadline| {
                    deadline
                        .saturating_duration_since(Instant::now())
                        .min(quantum)
                })
                .unwrap_or(quantum);
            if let Some(compiler) = &self.compiler {
                match compiler.wait_timeout(duration) {
                    Ok(completion) => return Some(completion),
                    Err(RecvTimeoutError::Timeout) => continue,
                    Err(RecvTimeoutError::Disconnected) => {}
                }
            }
            match &self.network {
                Some(network) => match network.wait_timeout(duration) {
                    Ok(completion) => return Some(completion),
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => {
                        return self.io.as_ref().and_then(IoService::wait);
                    }
                },
                None => {
                    if deadline.is_none() {
                        if let Some(io) = &self.io {
                            return io.wait();
                        }
                    }
                    std::thread::sleep(duration);
                }
            }
        }
    }

    fn close_file(&mut self, token: u64) -> bool {
        self.io.as_ref().is_some_and(|io| io.force_close(token))
    }

    fn cancel(&mut self, token: u64) -> bool {
        self.sleeps.remove(&token).is_some()
            || self
                .network
                .as_ref()
                .is_some_and(|network| network.cancel(token))
    }

    fn close_tcp(&mut self, resource: HostTcpResource) -> bool {
        self.network
            .as_ref()
            .is_some_and(|network| network.force_close(resource))
    }

    fn close_tls(&mut self, token: u64) -> bool {
        self.network
            .as_ref()
            .is_some_and(|network| network.force_close_tls(token))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lm_testkit::compile_to_bytes;
    use lm_vm::{load_bytes, CoreCtor, HostOpenOptions, HostSeekFrom, VmConfig, World};
    use rcgen::{generate_simple_self_signed, CertifiedKey};
    use rustls::pki_types::PrivatePkcs8KeyDer;
    use std::io::{Read, Write};
    use std::sync::Arc;

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

    fn net_ok_value(value: HostValue) -> HostValue {
        match value {
            HostValue::Ctor(CoreCtor::Ok, mut values) if values.len() == 1 => {
                values.pop().expect("the success value exists")
            }
            other => panic!("expected one network success, found {other:?}"),
        }
    }

    fn tls_ok_value(value: HostValue) -> HostValue {
        match value {
            HostValue::Ctor(CoreCtor::Ok, mut values) if values.len() == 1 => {
                values.pop().expect("the success value exists")
            }
            other => panic!("expected one TLS success, found {other:?}"),
        }
    }

    fn loopback(port: u16) -> lm_vm::HostSocketAddress {
        lm_vm::HostSocketAddress {
            ip: lm_vm::HostIpAddress::V4([127, 0, 0, 1]),
            port,
            flow_info: 0,
            scope_id: 0,
        }
    }

    fn tcp(kind: HostTcpKind, token: u64) -> HostArg {
        HostArg::Tcp(HostTcpResource { kind, token })
    }

    fn start_token(host: &mut CliHost, op: u32, args: Vec<HostArg>) -> u64 {
        match host.start(completion(), op, args) {
            HostStart::Waiting(token) => token,
            other => panic!("expected a pending host operation, found {other:?}"),
        }
    }

    fn connected_pair(host: &mut CliHost) -> (u64, u64, u64) {
        let listener = match net_ok_value(run_host(
            host,
            lm_abi::OP_TCP_LISTEN,
            vec![HostArg::SocketAddress(loopback(0)), HostArg::Int(16)],
        )) {
            HostValue::TcpListener(token) => token,
            other => panic!("expected a listener, found {other:?}"),
        };
        let address = match net_ok_value(run_host(
            host,
            lm_abi::OP_TCP_LOCAL_ADDRESS,
            vec![tcp(HostTcpKind::Listener, listener)],
        )) {
            HostValue::SocketAddress(address) => address,
            other => panic!("expected a socket address, found {other:?}"),
        };
        let client = match net_ok_value(run_host(
            host,
            lm_abi::OP_TCP_CONNECT,
            vec![HostArg::SocketAddress(address)],
        )) {
            HostValue::TcpStream(token) => token,
            other => panic!("expected a client stream, found {other:?}"),
        };
        let server = match net_ok_value(run_host(
            host,
            lm_abi::OP_TCP_ACCEPT,
            vec![tcp(HostTcpKind::Listener, listener)],
        )) {
            HostValue::Ctor(CoreCtor::Pair, mut values) if values.len() == 2 => {
                match values.remove(0) {
                    HostValue::TcpStream(token) => token,
                    other => panic!("expected a server stream, found {other:?}"),
                }
            }
            other => panic!("expected an accepted pair, found {other:?}"),
        };
        (listener, client, server)
    }

    fn tls_server<F>(body: F) -> (u16, Vec<u8>, std::thread::JoinHandle<()>)
    where
        F: FnOnce(&mut rustls::StreamOwned<rustls::ServerConnection, std::net::TcpStream>)
            + Send
            + 'static,
    {
        let CertifiedKey { cert, signing_key } =
            generate_simple_self_signed(["localhost".to_string()])
                .expect("the test certificate generates");
        let certificate = cert.der().clone();
        let root = certificate.as_ref().to_vec();
        let key = PrivatePkcs8KeyDer::from(signing_key.serialize_der());
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut config = rustls::ServerConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&rustls::version::TLS13, &rustls::version::TLS12])
            .expect("the TLS versions are valid")
            .with_no_client_auth()
            .with_single_cert(vec![certificate], key.into())
            .expect("the TLS server certificate is valid");
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        let config = Arc::new(config);
        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .expect("the TLS test listener binds");
        let port = listener
            .local_addr()
            .expect("the TLS test address exists")
            .port();
        let thread = std::thread::spawn(move || {
            let (socket, _) = listener.accept().expect("the TLS client connects");
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("the TLS test sets its read timeout");
            socket
                .set_write_timeout(Some(Duration::from_secs(5)))
                .expect("the TLS test sets its write timeout");
            let connection = rustls::ServerConnection::new(config).expect("the TLS server starts");
            let mut stream = rustls::StreamOwned::new(connection, socket);
            body(&mut stream);
        });
        (port, root, thread)
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

    #[test]
    fn dns_resolves_localhost_without_external_network_access() {
        let mut host = CliHost::new(1);
        let value = net_ok_value(run_host(
            &mut host,
            lm_abi::OP_DNS_RESOLVE,
            vec![HostArg::Str("localhost".into()), HostArg::Int(80)],
        ));
        let HostValue::List(addresses) = value else {
            panic!("expected a DNS address list");
        };
        assert!(!addresses.is_empty());
        assert!(addresses.into_iter().all(|value| {
            matches!(value, HostValue::SocketAddress(address) if address.port == 80)
        }));
    }

    #[test]
    fn tcp_loopback_reports_data_and_orderly_end() {
        let mut host = CliHost::new(1);
        let (listener, client, server) = connected_pair(&mut host);

        assert_eq!(
            net_ok_value(run_host(
                &mut host,
                lm_abi::OP_TCP_WRITE,
                vec![
                    tcp(HostTcpKind::Stream, client),
                    HostArg::Bytes(b"hello".into()),
                ],
            )),
            HostValue::Int(5)
        );
        assert_eq!(
            net_ok_value(run_host(
                &mut host,
                lm_abi::OP_TCP_READ,
                vec![tcp(HostTcpKind::Stream, server), HostArg::Int(16)],
            )),
            HostValue::Ctor(
                CoreCtor::TcpReadData,
                vec![HostValue::Bytes(b"hello".into())]
            )
        );
        assert_eq!(
            net_ok_value(run_host(
                &mut host,
                lm_abi::OP_TCP_SHUTDOWN,
                vec![
                    tcp(HostTcpKind::Stream, client),
                    HostArg::Shutdown(lm_vm::HostShutdown::Write),
                ],
            )),
            HostValue::Unit
        );
        assert_eq!(
            net_ok_value(run_host(
                &mut host,
                lm_abi::OP_TCP_READ,
                vec![tcp(HostTcpKind::Stream, server), HostArg::Int(16)],
            )),
            HostValue::Ctor(CoreCtor::TcpReadEnd, vec![])
        );

        for resource in [
            HostTcpResource {
                kind: HostTcpKind::Stream,
                token: client,
            },
            HostTcpResource {
                kind: HostTcpKind::Stream,
                token: server,
            },
            HostTcpResource {
                kind: HostTcpKind::Listener,
                token: listener,
            },
        ] {
            assert_eq!(
                net_ok_value(run_host(
                    &mut host,
                    lm_abi::OP_TCP_CLOSE,
                    vec![HostArg::Tcp(resource)],
                )),
                HostValue::Unit
            );
        }
    }

    #[test]
    fn tls_loopback_checks_the_certificate_and_moves_data() {
        let (port, root, server) = tls_server(|stream| {
            let mut request = [0_u8; 4];
            stream
                .read_exact(&mut request)
                .expect("the TLS server reads a request");
            assert_eq!(&request, b"ping");
            assert_eq!(stream.conn.alpn_protocol(), Some(b"http/1.1".as_slice()));
            stream
                .write_all(b"pong")
                .expect("the TLS server writes a response");
            stream.conn.send_close_notify();
            stream.flush().expect("the TLS server flushes its response");
        });
        let mut host = CliHost::new(1);
        let tcp_token = match net_ok_value(run_host(
            &mut host,
            lm_abi::OP_TCP_CONNECT,
            vec![HostArg::SocketAddress(loopback(port))],
        )) {
            HostValue::TcpStream(token) => token,
            other => panic!("expected a TCP stream, found {other:?}"),
        };
        let tls = match tls_ok_value(run_host(
            &mut host,
            lm_abi::OP_TLS_HANDSHAKE,
            vec![
                tcp(HostTcpKind::Stream, tcp_token),
                HostArg::Str("localhost".into()),
                HostArg::Int(1),
                HostArg::List(vec![HostArg::Bytes(root.into())]),
                HostArg::List(vec![HostArg::Bytes(b"http/1.1".into())]),
                HostArg::Int(12),
                HostArg::Int(65_536),
            ],
        )) {
            HostValue::TlsStream(token) => token,
            other => panic!("expected a TLS stream, found {other:?}"),
        };
        assert_eq!(
            tls_ok_value(run_host(
                &mut host,
                lm_abi::OP_TLS_WRITE,
                vec![HostArg::Tls(tls), HostArg::Bytes(Vec::<u8>::new().into())],
            )),
            HostValue::Int(0)
        );
        assert_eq!(
            tls_ok_value(run_host(
                &mut host,
                lm_abi::OP_TLS_WRITE,
                vec![HostArg::Tls(tls), HostArg::Bytes(b"ping".into())],
            )),
            HostValue::Int(4)
        );
        assert_eq!(
            tls_ok_value(run_host(
                &mut host,
                lm_abi::OP_TLS_READ,
                vec![HostArg::Tls(tls), HostArg::Int(16)],
            )),
            HostValue::Ctor(
                CoreCtor::TcpReadData,
                vec![HostValue::Bytes(b"pong".into())]
            )
        );
        let local = tls_ok_value(run_host(
            &mut host,
            lm_abi::OP_TLS_LOCAL_ADDRESS,
            vec![HostArg::Tls(tls)],
        ));
        assert!(matches!(local, HostValue::SocketAddress(address) if address.port != 0));
        assert_eq!(
            tls_ok_value(run_host(
                &mut host,
                lm_abi::OP_TLS_PEER_ADDRESS,
                vec![HostArg::Tls(tls)],
            )),
            HostValue::SocketAddress(loopback(port))
        );
        assert_eq!(
            tls_ok_value(run_host(
                &mut host,
                lm_abi::OP_TLS_READ,
                vec![HostArg::Tls(tls), HostArg::Int(16)],
            )),
            HostValue::Ctor(CoreCtor::TcpReadEnd, vec![])
        );
        assert_eq!(
            tls_ok_value(run_host(
                &mut host,
                lm_abi::OP_TLS_SHUTDOWN,
                vec![HostArg::Tls(tls)],
            )),
            HostValue::Unit
        );
        assert_eq!(
            tls_ok_value(run_host(
                &mut host,
                lm_abi::OP_TLS_CLOSE,
                vec![HostArg::Tls(tls)],
            )),
            HostValue::Unit
        );
        server.join().expect("the TLS server exits");
    }

    #[test]
    fn oversized_network_writes_stop_before_submission() {
        let mut host = CliHost::new(1);
        let bytes: SharedBytes = vec![0_u8; MAX_NETWORK_IO_BYTES + 1].into();
        let tcp_value = host.start(
            completion(),
            lm_abi::OP_TCP_WRITE,
            vec![tcp(HostTcpKind::Stream, 1), HostArg::Bytes(bytes.clone())],
        );
        assert!(matches!(
            tcp_value,
            HostStart::Completed(HostValue::Ctor(CoreCtor::Err, ref values))
                if matches!(values.as_slice(), [HostValue::Ctor(CoreCtor::NetLimitExceeded, _)])
        ));

        let tls_value = host.start(
            completion(),
            lm_abi::OP_TLS_WRITE,
            vec![HostArg::Tls(1), HostArg::Bytes(bytes)],
        );
        assert!(matches!(
            tls_value,
            HostStart::Completed(HostValue::Ctor(CoreCtor::Err, ref values))
                if matches!(values.as_slice(), [HostValue::Ctor(CoreCtor::TlsLimitExceeded, _)])
        ));
    }

    #[test]
    fn tls_rejects_a_certificate_for_another_name() {
        let (port, root, server) = tls_server(|stream| {
            let mut byte = [0_u8; 1];
            assert!(stream.read(&mut byte).is_err());
        });
        let mut host = CliHost::new(1);
        let tcp_token = match net_ok_value(run_host(
            &mut host,
            lm_abi::OP_TCP_CONNECT,
            vec![HostArg::SocketAddress(loopback(port))],
        )) {
            HostValue::TcpStream(token) => token,
            other => panic!("expected a TCP stream, found {other:?}"),
        };
        let value = run_host(
            &mut host,
            lm_abi::OP_TLS_HANDSHAKE,
            vec![
                tcp(HostTcpKind::Stream, tcp_token),
                HostArg::Str("wrong.example".into()),
                HostArg::Int(1),
                HostArg::List(vec![HostArg::Bytes(root.into())]),
                HostArg::List(vec![]),
                HostArg::Int(13),
                HostArg::Int(65_536),
            ],
        );
        match value {
            HostValue::Ctor(CoreCtor::Err, values)
                if matches!(
                    values.as_slice(),
                    [HostValue::Ctor(CoreCtor::TlsCertificate, _)]
                ) => {}
            other => panic!("expected a TLS certificate error, found {other:?}"),
        }
        let stale = run_host(
            &mut host,
            lm_abi::OP_TCP_CLOSE,
            vec![tcp(HostTcpKind::Stream, tcp_token)],
        );
        assert!(matches!(
            stale,
            HostValue::Ctor(CoreCtor::Err, ref values)
                if matches!(values.as_slice(), [HostValue::Ctor(CoreCtor::NetClosed, _)])
        ));
        server.join().expect("the TLS server exits");
    }

    #[test]
    fn secure_http_runs_from_loom_through_the_real_tls_host() {
        let (port, root, server) = tls_server(|stream| {
            let mut request = Vec::new();
            while !request.windows(4).any(|part| part == b"\r\n\r\n") {
                let mut bytes = [0_u8; 1024];
                let read = stream
                    .read(&mut bytes)
                    .expect("the HTTPS server reads a request");
                assert!(read > 0);
                request.extend_from_slice(&bytes[..read]);
                assert!(request.len() <= 16_384);
            }
            assert!(request.starts_with(b"GET /demo HTTP/1.1\r\n"));
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\npong")
                .expect("the HTTPS server writes its response");
            stream.conn.send_close_notify();
            stream
                .flush()
                .expect("the HTTPS server flushes its response");
        });
        let root_bytes: String = root
            .iter()
            .map(|byte| format!("root.append({byte})\n"))
            .collect();
        let source = format!(
            r#"
use std.tls.TlsClientConfig
use std.tls.TlsRoots
use std.tls.TlsVersion
use std.http.Http
use std.http.HttpRequest

root = ByteBuffer()
{root_bytes}
config = TlsClientConfig(
  "localhost",
  TlsRoots.Custom([root.finish()]),
  [Bytes("http/1.1")],
  TlsVersion.Tls13,
  65536
).freeze()
request = HttpRequest("GET", "/demo", [], Bytes())
case Http().send_secure(
  "localhost",
  {port},
  request,
  config,
  Http().default_limits()
)
in Ok(response) then "{{response.status}} {{response.body.text()}}"
in Err(error) then error.message()
end
"#
        );
        let bytes =
            compile_to_bytes("local_https.lm", &source).expect("the HTTPS program compiles");
        let loaded = load_bytes(&bytes).expect("the HTTPS program loads");
        let mut world = World::new(&loaded, VmConfig::default(), Box::new(CliHost::new(1)));
        world
            .allow("Http.Client")
            .expect("the secure HTTP grant exists");
        let outcome = lm_proc::run_world(&mut world);
        assert_eq!(world.show_outcome(&outcome), "Done(\"200 pong\")");
        assert_eq!(world.resource_count(0), 0);
        server.join().expect("the HTTPS server exits");
    }

    #[test]
    fn a_blocked_read_does_not_delay_a_write_or_consume_canceled_data() {
        let mut host = CliHost::new(1);
        let (listener, client, server) = connected_pair(&mut host);
        let read = start_token(
            &mut host,
            lm_abi::OP_TCP_READ,
            vec![tcp(HostTcpKind::Stream, server), HostArg::Int(32)],
        );
        let write = start_token(
            &mut host,
            lm_abi::OP_TCP_WRITE,
            vec![
                tcp(HostTcpKind::Stream, server),
                HostArg::Bytes(b"response".into()),
            ],
        );
        let completion = host.wait().expect("the write completes");
        assert_eq!(completion.token, write);
        assert_eq!(
            net_ok_value(completion.result.expect("the host write succeeds")),
            HostValue::Int(8)
        );
        assert_eq!(
            net_ok_value(run_host(
                &mut host,
                lm_abi::OP_TCP_READ,
                vec![tcp(HostTcpKind::Stream, client), HostArg::Int(32)],
            )),
            HostValue::Ctor(
                CoreCtor::TcpReadData,
                vec![HostValue::Bytes(b"response".into())]
            )
        );
        assert!(host.cancel(read));
        assert_eq!(
            net_ok_value(run_host(
                &mut host,
                lm_abi::OP_TCP_WRITE,
                vec![
                    tcp(HostTcpKind::Stream, client),
                    HostArg::Bytes(b"retained".into()),
                ],
            )),
            HostValue::Int(8)
        );
        assert_eq!(
            net_ok_value(run_host(
                &mut host,
                lm_abi::OP_TCP_READ,
                vec![tcp(HostTcpKind::Stream, server), HostArg::Int(32)],
            )),
            HostValue::Ctor(
                CoreCtor::TcpReadData,
                vec![HostValue::Bytes(b"retained".into())]
            )
        );
        for resource in [
            HostTcpResource {
                kind: HostTcpKind::Stream,
                token: client,
            },
            HostTcpResource {
                kind: HostTcpKind::Stream,
                token: server,
            },
            HostTcpResource {
                kind: HostTcpKind::Listener,
                token: listener,
            },
        ] {
            assert!(host.close_tcp(resource));
        }
    }

    #[test]
    fn close_wakes_a_pending_stream_read() {
        let mut host = CliHost::new(1);
        let (listener, client, server) = connected_pair(&mut host);
        let read = start_token(
            &mut host,
            lm_abi::OP_TCP_READ,
            vec![tcp(HostTcpKind::Stream, server), HostArg::Int(32)],
        );
        let close = start_token(
            &mut host,
            lm_abi::OP_TCP_CLOSE,
            vec![tcp(HostTcpKind::Stream, server)],
        );

        let read_completion = host.wait().expect("the closed read completes");
        assert_eq!(read_completion.token, read);
        assert!(matches!(
            read_completion.result.expect("the host returns a value"),
            HostValue::Ctor(CoreCtor::Err, ref values)
                if matches!(values.as_slice(), [HostValue::Ctor(CoreCtor::NetClosed, _)])
        ));
        let close_completion = host.wait().expect("the close completes");
        assert_eq!(close_completion.token, close);
        assert_eq!(
            net_ok_value(close_completion.result.expect("the host close succeeds")),
            HostValue::Unit
        );

        for resource in [
            HostTcpResource {
                kind: HostTcpKind::Stream,
                token: client,
            },
            HostTcpResource {
                kind: HostTcpKind::Listener,
                token: listener,
            },
        ] {
            assert_eq!(
                net_ok_value(run_host(
                    &mut host,
                    lm_abi::OP_TCP_CLOSE,
                    vec![HostArg::Tcp(resource)],
                )),
                HostValue::Unit
            );
        }
    }
}
