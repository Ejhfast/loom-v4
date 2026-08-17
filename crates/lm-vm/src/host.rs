//! The host-completion interface.
//!
//! `lm-vm` never touches the operating system. A `Host` receives
//! plain-data arguments for one root-granted fixed operation and
//! completes with a plain-data reply, now or later. No Rust reference
//! into guest memory crosses this boundary in either direction.

use crate::CompletionKey;
use std::collections::BTreeMap;

/// One plain-data operation argument.
#[derive(Debug, Clone, PartialEq)]
pub enum HostArg {
    Int(i64),
    Str(String),
}

/// One core constructor a host reply may name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoreCtor {
    Some,
    None,
    Ok,
    Err,
    IoErrorFailed,
}

/// One plain-data operation reply. `Ctor` builds a pinned core enum
/// value inside the performing machine.
#[derive(Debug, Clone, PartialEq)]
pub enum HostValue {
    Unit,
    Int(i64),
    Str(String),
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
    /// The plain-data reply.
    pub value: HostValue,
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

/// A deterministic in-memory host for tests. Print and Error append
/// to buffers; Clock.Now counts calls; Clock.Sleep completes through
/// the asynchronous completion channel after a fixed number of
/// polls; Rand.Int is a seeded PRNG.
pub struct RecordingHost {
    pub printed: Vec<String>,
    pub errors: Vec<String>,
    pub input: Vec<String>,
    now: i64,
    monotonic: i64,
    rand_state: u64,
    sleeps: BTreeMap<u64, (CompletionKey, u32)>,
    next_token: u64,
}

impl Default for RecordingHost {
    fn default() -> RecordingHost {
        RecordingHost::new(1)
    }
}

impl RecordingHost {
    pub fn new(seed: u64) -> RecordingHost {
        RecordingHost {
            printed: Vec::new(),
            errors: Vec::new(),
            input: Vec::new(),
            now: 1_000,
            monotonic: 0,
            rand_state: seed.max(1),
            sleeps: BTreeMap::new(),
            next_token: 1,
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

impl Host for RecordingHost {
    fn start(&mut self, key: CompletionKey, op: u32, args: Vec<HostArg>) -> HostStart {
        match op {
            lm_abi::OP_IO_PRINT => {
                if let Some(HostArg::Str(text)) = args.first() {
                    self.printed.push(text.clone());
                }
                HostStart::Completed(HostValue::Unit)
            }
            lm_abi::OP_IO_ERROR => {
                if let Some(HostArg::Str(text)) = args.first() {
                    self.errors.push(text.clone());
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
                        vec![HostValue::Ctor(CoreCtor::Some, vec![HostValue::Str(line)])],
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
            lm_abi::OP_CLOCK_SLEEP => {
                let token = self.next_token;
                self.next_token += 1;
                self.sleeps.insert(token, (key, 1));
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
                let span = (high - low) as u64;
                let value = low + (self.next_rand() % span) as i64;
                HostStart::Completed(HostValue::Int(value))
            }
            _ => HostStart::Failed(format!(
                "the test host does not implement {}",
                lm_abi::op_name(op)
            )),
        }
    }

    fn poll(&mut self) -> Option<HostCompletion> {
        let ready = self
            .sleeps
            .iter()
            .find_map(|(token, (_, remaining))| (*remaining == 0).then_some(*token));
        match ready {
            Some(token) => {
                let (key, _) = self.sleeps.remove(&token)?;
                Some(HostCompletion {
                    key,
                    token,
                    value: HostValue::Unit,
                })
            }
            None => {
                for (_, remaining) in self.sleeps.values_mut() {
                    *remaining = remaining.saturating_sub(1);
                }
                None
            }
        }
    }

    fn wait(&mut self) -> Option<HostCompletion> {
        let token = self.sleeps.keys().next().copied()?;
        let (key, _) = self.sleeps.remove(&token)?;
        Some(HostCompletion {
            key,
            token,
            value: HostValue::Unit,
        })
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
}
