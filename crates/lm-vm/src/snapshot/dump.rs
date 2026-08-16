//! The readable dump of one snapshot image.
//!
//! Every new external format owes a human-readable dump. The dump
//! reads the decoded image alone, so it repeats exactly for equal
//! bytes and never depends on a heap slot or a scheduler identifier.
//! `lm inspect <file.lms>` prints it, and the deterministic snapshot
//! diff of the test suite compares two dumps line by line.

use super::{Image, ImageBlock, ImageTerminal, SnapshotImage};
use lm_heap::Object;
use lm_value::Value;
use std::fmt::Write as _;

/// The one-line verdict of `lm snapshot verify`.
pub fn verdict(image: &Image) -> String {
    format!(
        "valid: state={} machines={} mailboxes={}",
        image.root_state().name(),
        image.machine_count(),
        image.mailbox_count()
    )
}

/// A readable dump of one image, one fact per line.
pub fn dump(image: &SnapshotImage) -> String {
    let world = image.world();
    let mut out = String::new();
    let _ = writeln!(
        out,
        "container {} bytes hash {}",
        image.bytes().len(),
        hex(&image.hash())
    );
    let _ = writeln!(
        out,
        "format {} abi {} compiler {} verifier {}",
        world.format, world.abi_version, world.compiler_abi, world.verifier_version
    );
    let _ = writeln!(out, "module {}", hex(&world.module_semantic));
    let _ = writeln!(out, "result-type {}", hex(&world.result_type));
    let _ = writeln!(out, "{}", verdict(world));
    for (slot, hash) in &world.funcs {
        let _ = writeln!(out, "func {slot} {}", hex(hash));
    }
    for (slot, hash) in &world.classes {
        let _ = writeln!(out, "class {slot} {}", hex(hash));
    }
    for (ordinal, machine) in world.machines.iter().enumerate() {
        let parent = match machine.parent {
            None => "outside".to_string(),
            Some(p) => p.to_string(),
        };
        let _ = writeln!(
            out,
            "machine {ordinal} state {} parent {parent} owner {} paused {} gen {} fuel {} \
             objects {} frames {}",
            machine.state.name(),
            if machine.scheduler_owned {
                "scheduler"
            } else {
                "holder"
            },
            machine.paused,
            machine.generation,
            machine.fuel,
            machine.objects.len(),
            machine.frames.len()
        );
        for (idx, frame) in machine.frames.iter().enumerate() {
            let _ = writeln!(
                out,
                "  frame {idx} func {} block {} ip {} locals {} operands {}",
                frame.func, frame.block, frame.ip, frame.base_local, frame.base_operand
            );
        }
        if let Some(pending) = &machine.pending {
            let _ = writeln!(
                out,
                "  pending {} ordinal {} args {}",
                lm_abi::op_name(pending.op),
                pending.ordinal,
                pending.args.len()
            );
        }
        match &machine.terminal {
            None => {}
            Some(ImageTerminal::Done(value)) => {
                let _ = writeln!(out, "  terminal done {}", show(*value));
            }
            Some(ImageTerminal::Fault(rec)) => {
                let _ = writeln!(out, "  terminal fault {} {}", rec.code, rec.message);
            }
        }
        let _ = writeln!(
            out,
            "  mailbox limit {} queued {} closed {} accepted {} delivered {}",
            machine.mailbox.limit,
            machine.mailbox.queue.len(),
            machine.mailbox.closed,
            machine.mailbox.accepted,
            machine.mailbox.delivered
        );
        match machine.block {
            None => {}
            Some(ImageBlock::Receive) => {
                let _ = writeln!(out, "  blocked on receive");
            }
            Some(ImageBlock::Send { target }) => {
                let _ = writeln!(out, "  blocked on send to machine {target}");
            }
            Some(ImageBlock::Done { target }) => {
                let _ = writeln!(out, "  blocked on done of machine {target}");
            }
        }
        for (idx, entry) in machine.objects.iter().enumerate() {
            let state = if entry.frozen { "frozen" } else { "mutable" };
            let _ = writeln!(
                out,
                "  obj {idx} {} {state} {}",
                entry.object.shape().name,
                payload(&entry.object)
            );
        }
    }
    out
}

/// The canonical payload of one captured object, as readable text.
fn payload(object: &Object) -> String {
    match object {
        Object::Str(text) => format!("{text:?}"),
        Object::Instance { class, fields } => {
            format!("class {class} fields [{}]", values(fields))
        }
        Object::List { items } => format!("[{}]", values(items)),
        Object::Tuple { items } => format!("({})", values(items)),
        Object::Map { entries, .. } => {
            let parts: Vec<String> = entries
                .iter()
                .map(|(k, v)| format!("{}: {}", show(*k), show(*v)))
                .collect();
            format!("{{{}}}", parts.join(", "))
        }
        Object::Closure { func, captures } => {
            format!("func {func} captures [{}]", values(captures))
        }
        Object::StrBuilder(text) => format!("builder len {}", text.len()),
        Object::ByteBuf(bytes) => format!("buffer len {}", bytes.len()),
        Object::NativeVm { vm } => format!("machine {vm}"),
        Object::NativeTable { vm } => format!("table of machine {vm}"),
        Object::NativeRequest { vm, ordinal } => format!("request {ordinal} of machine {vm}"),
        Object::NativeCall { vm, ordinal, op } => format!(
            "call {ordinal} of machine {vm} for {}",
            lm_abi::op_name(*op)
        ),
        Object::NativeHandle { proc, generation } => format!("proc {proc}.{generation}"),
        Object::NativeFault { code, message, .. } => format!("{code} {message:?}"),
        Object::NativeDigest(bytes) => hex(bytes),
        Object::NativeSnapshot(image) => format!("nested image {} bytes", image.len()),
    }
}

fn values(items: &[Value]) -> String {
    let parts: Vec<String> = items.iter().map(|v| show(*v)).collect();
    parts.join(", ")
}

/// One captured value. An object reference prints as its ordinal.
fn show(value: Value) -> String {
    match value {
        Value::Unit => "()".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Int(v) => v.to_string(),
        Value::Op(op) => format!("<op {}>", lm_abi::op_name(op)),
        Value::Obj(r) => format!("#{}", r.slot),
        Value::Uninit => "<uninit>".to_string(),
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}
