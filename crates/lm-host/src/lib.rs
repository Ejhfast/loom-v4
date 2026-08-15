//! Root host operations for the command line.
//!
//! `CliHost` implements the `lm-vm` completion interface over the
//! real process: stdout, stderr, stdin, the system clocks, and a
//! seeded deterministic PRNG. `lm-vm` never depends on this crate;
//! `lm-cli` wires it in.
//!
//! `Clock.Sleep` uses the asynchronous completion channel: `start`
//! records a deadline and returns `Waiting`; `poll` completes when
//! the deadline passed; `wait` blocks until it passes. Week 9 makes
//! the channel truly asynchronous; the shape is already in place.

use lm_vm::{CoreCtor, Host, HostArg, HostStart, HostValue};
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// The command-line root host.
pub struct CliHost {
    started: Instant,
    rand_state: u64,
    sleeps: HashMap<u64, Instant>,
    next_token: u64,
}

impl CliHost {
    /// Create a host. `rand_seed` seeds the deterministic PRNG.
    pub fn new(rand_seed: u64) -> CliHost {
        CliHost {
            started: Instant::now(),
            rand_state: rand_seed.max(1),
            sleeps: HashMap::new(),
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

fn io_error(message: String) -> HostValue {
    HostValue::Ctor(
        CoreCtor::Err,
        vec![HostValue::Ctor(
            CoreCtor::IoErrorFailed,
            vec![HostValue::Str(message)],
        )],
    )
}

impl Host for CliHost {
    fn start(&mut self, op: u32, args: Vec<HostArg>) -> HostStart {
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
                    .insert(token, Instant::now() + Duration::from_nanos(nanos));
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
            _ => HostStart::Failed(format!(
                "the command-line host does not implement {}",
                lm_abi::op_name(op)
            )),
        }
    }

    fn poll(&mut self, token: u64) -> Option<HostValue> {
        match self.sleeps.get(&token) {
            Some(deadline) if Instant::now() >= *deadline => {
                self.sleeps.remove(&token);
                Some(HostValue::Unit)
            }
            _ => None,
        }
    }

    fn wait(&mut self, token: u64) -> HostValue {
        if let Some(deadline) = self.sleeps.remove(&token) {
            let now = Instant::now();
            if deadline > now {
                std::thread::sleep(deadline - now);
            }
        }
        HostValue::Unit
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rand_is_deterministic_per_seed() {
        let draw = |seed: u64| -> Vec<HostValue> {
            let mut host = CliHost::new(seed);
            (0..4)
                .map(|_| {
                    match host.start(
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
        let out = host.start(lm_abi::OP_RAND_INT, vec![HostArg::Int(5), HostArg::Int(5)]);
        assert!(matches!(out, HostStart::Failed(_)));
    }

    #[test]
    fn sleep_uses_the_completion_channel() {
        let mut host = CliHost::new(1);
        let token = match host.start(lm_abi::OP_CLOCK_SLEEP, vec![HostArg::Int(1)]) {
            HostStart::Waiting(token) => token,
            other => panic!("unexpected start result: {other:?}"),
        };
        assert_eq!(host.wait(token), HostValue::Unit);
        assert_eq!(host.poll(token), None);
    }
}
