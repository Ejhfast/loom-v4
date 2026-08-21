//! The readable dump of one snapshot image.
//!
//! Every new external format owes a human-readable dump. The dump
//! reads the decoded image alone, so it repeats exactly for equal
//! bytes and never depends on a heap slot or a scheduler identifier.
//! `lm inspect <file.lms>` prints it, and the deterministic snapshot
//! diff of the test suite compares two dumps line by line.

use super::{
    Image, ImageBlock, ImagePolicyCursor, ImageSlotTarget, ImageTerminal, ImageWaitSource,
    SnapshotImage,
};
use lm_heap::Object;
use lm_value::Value;
use std::fmt::Write as _;

/// The one-line verdict of `lm snapshot verify`.
///
/// The call reads editable image data, so it never assumes a machine
/// exists. Inspection of an invalid image stays total.
pub fn verdict(image: &Image) -> String {
    let state = match image.root_state() {
        Some(state) => state.name(),
        None => "none",
    };
    format!(
        "valid: state={state} machines={} mailboxes={}",
        image.machine_count(),
        image.mailbox_count()
    )
}

/// A readable dump of one admitted image and its container.
pub fn dump(image: &SnapshotImage) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "container {} bytes hash {}",
        image.bytes().map(|b| b.len()).unwrap_or(0),
        image.hash().map(|h| hex(&h)).unwrap_or_default()
    );
    out.push_str(&dump_image(image.world()));
    out
}

/// A readable dump of one editable image, one fact per line.
///
/// The dump never indexes a table from image data, so it prints an
/// invalid image without a panic.
pub fn dump_image(world: &Image) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "format {} abi {} compiler {} verifier {}",
        world.format, world.abi_version, world.compiler_abi, world.verifier_version
    );
    let _ = writeln!(out, "module {}", hex(&world.module_semantic));
    let _ = writeln!(out, "distinguished-run {:?}", world.distinguished);
    let _ = writeln!(out, "full-VM {:?}", world.full_vm);
    let _ = writeln!(out, "result-type {}", hex(&world.result_type));
    let _ = writeln!(out, "{}", verdict(world));
    for (slot, hash) in &world.funcs {
        let _ = writeln!(out, "func {slot} {}", hex(hash));
    }
    for (slot, hash) in &world.classes {
        let _ = writeln!(out, "class {slot} {}", hex(hash));
    }
    for (ordinal, artifact) in world.installations.iter().enumerate() {
        let hash = lm_bytecode::identity::container_hash(artifact);
        let _ = writeln!(
            out,
            "installation {ordinal} bytes {} hash {}",
            artifact.len(),
            hex(&hash)
        );
    }
    let _ = writeln!(
        out,
        "closed-types {} environments {}",
        world.types.len(),
        world.envs.len()
    );
    for (ordinal, image) in world.vm_images.iter().enumerate() {
        let _ = writeln!(
            out,
            "VM image {ordinal} slots {} objects {} instances {}",
            image.slots.len(),
            image.objects.len(),
            image.instances.len()
        );
        for (slot, target) in image.slots.iter().enumerate() {
            let _ = writeln!(out, "  slot {slot} {}", slot_text(*target));
        }
        for (index, instance) in image.instances.iter().enumerate() {
            let _ = writeln!(
                out,
                "  instance {index} installation {} entry {} interface {} semantic {}",
                instance.installation,
                instance.entry,
                instance.interface.as_ref().map_or(0, Vec::len),
                hex(&instance.semantic_hash)
            );
            let _ = writeln!(out, "    functions {:?}", instance.funcs);
            let _ = writeln!(out, "    classes {:?}", instance.classes);
            let _ = writeln!(out, "    slots {:?}", instance.slots);
        }
        for (index, entry) in image.objects.iter().enumerate() {
            let state = if entry.frozen { "frozen" } else { "mutable" };
            let _ = writeln!(
                out,
                "  image-object {index} {} {state} {}",
                entry.object.shape().name,
                payload(&entry.object)
            );
        }
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
                "  frame {idx} func {} block {} ip {} locals {} operands {} env {}",
                frame.func, frame.block, frame.ip, frame.base_local, frame.base_operand, frame.env
            );
        }
        if let Some(pending) = &machine.pending {
            let _ = writeln!(
                out,
                "  pending {} ordinal {} args {}",
                op_text(pending.op),
                pending.ordinal,
                pending.args.len()
            );
        }
        for wait in &machine.waits {
            let source = match wait.source {
                ImageWaitSource::Receive => "receive".to_string(),
                ImageWaitSource::Drive { target } => format!("drive machine {target}"),
                ImageWaitSource::Choice { first, second } => {
                    format!("choice {first} {second}")
                }
            };
            let _ = writeln!(
                out,
                "  wait {} linked {} source {source}",
                wait.token, wait.linked
            );
        }
        if let Some(nested) = machine.nested {
            let _ = writeln!(out, "  nested {nested}");
        }
        if let Some(route) = machine.routed {
            let cursor = match route.cursor {
                ImagePolicyCursor::Table(table) => format!("table {table}"),
                ImagePolicyCursor::Binding => "binding".to_string(),
                ImagePolicyCursor::Root => "root".to_string(),
            };
            let _ = writeln!(out, "  routed {} cursor {cursor}", route.target);
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
            Some(ImageBlock::Wait { token }) => {
                let _ = writeln!(out, "  blocked on wait {token}");
            }
            Some(ImageBlock::Snapshot {
                target,
                remaining,
                retry,
            }) => {
                let _ = writeln!(
                    out,
                    "  blocked on snapshot {target} remaining {remaining} retry {retry}"
                );
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

fn slot_text(target: ImageSlotTarget) -> String {
    match target {
        ImageSlotTarget::Empty => "empty".to_string(),
        ImageSlotTarget::Function(function) => format!("function {function}"),
        ImageSlotTarget::Class(class) => format!("class {class}"),
        ImageSlotTarget::Value(value) => format!("value {}", show(value)),
        ImageSlotTarget::Process { proc, generation } => {
            format!("process {proc}:{generation}")
        }
    }
}

/// The deterministic difference between two dumps.
///
/// The dump is one fact per line and it repeats exactly, so a line
/// comparison is a stable diff. The result names the first line the
/// two images do not share, or `None` when the two are equal.
pub fn diff(left: &SnapshotImage, right: &SnapshotImage) -> Option<String> {
    let a = dump(left);
    let b = dump(right);
    if a == b {
        return None;
    }
    let mut lines_a = a.lines();
    let mut lines_b = b.lines();
    let mut at = 1usize;
    loop {
        match (lines_a.next(), lines_b.next()) {
            (None, None) => return None,
            (Some(x), Some(y)) if x == y => at += 1,
            (x, y) => {
                return Some(format!(
                    "line {at}\n  left  {}\n  right {}",
                    x.unwrap_or("<end>"),
                    y.unwrap_or("<end>")
                ))
            }
        }
    }
}

/// The canonical payload of one captured object, as readable text.
fn payload(object: &Object) -> String {
    match object {
        Object::Str(text) => format!("{text:?}"),
        Object::Instance { class, fields, env } => {
            format!(
                "class {class} env {} fields [{}]",
                env.env().0,
                values(fields)
            )
        }
        Object::List { items, .. } => format!("[{}]", values(items)),
        Object::Tuple { items } => format!("({})", values(items)),
        Object::Map { entries, .. } => {
            let parts: Vec<String> = entries
                .iter()
                .map(|(k, v)| format!("{}: {}", show(*k), show(*v)))
                .collect();
            format!("{{{}}}", parts.join(", "))
        }
        Object::Closure {
            func,
            captures,
            env,
        } => {
            format!(
                "func {func} env {} captures [{}]",
                env.env().0,
                values(captures)
            )
        }
        Object::StrBuilder(text) => match text.byte_len() {
            Some(len) => format!("builder length {len}"),
            None => "finished builder".to_string(),
        },
        Object::ByteBuf(bytes) => match bytes.len() {
            Some(len) => format!("buffer length {len}"),
            None => "finished buffer".to_string(),
        },
        Object::Bytes(bytes) => format!("bytes len {}", bytes.len()),
        Object::Substring(text) => format!("substring {text:?}"),
        Object::NativeVm { image, generation } => format!("VM image {image}:{generation}"),
        Object::NativeRun { vm } => format!("run {vm}"),
        Object::NativeCode(code) => {
            format!(
                "portable {:?} index {} bytes {} interface {}",
                code.kind,
                code.index,
                code.bytes.len(),
                code.interface.as_ref().map_or(0, |bytes| bytes.len())
            )
        }
        Object::NativeCodeHandle {
            image,
            generation,
            instance,
            kind,
            index,
        } => format!(
            "installed {kind:?} {index} in instance {instance} of image {image}:{generation}"
        ),
        Object::NativeTable { vm } => format!("table of machine {vm}"),
        Object::NativeRequest { vm, ordinal } => format!("request {ordinal} of machine {vm}"),
        Object::NativeCall { vm, ordinal, op } => {
            format!("call {ordinal} of machine {vm} for {}", op_text(*op))
        }
        Object::NativeHandle { proc, generation } => format!("proc {proc}.{generation}"),
        Object::NativeFault { code, message, .. } => format!("{code} {message:?}"),
        Object::NativeDigest(bytes) => hex(bytes),
        Object::NativeSnapshot(image) => format!("nested image {} bytes", image.len()),
        Object::NativeSnapshotRef { image } => format!("image handle {image}"),
        Object::NativeFileHandle { resource } => format!("file resource {resource}"),
        Object::NativeResourceHandle { surface, resource } => {
            format!("resource {resource} of machine {surface}")
        }
        Object::NativeWait { owner, token } => format!("wait {token} of machine {owner}"),
        Object::NativeTcpStream { resource } => format!("TCP stream resource {resource}"),
        Object::NativeTcpListener { resource } => format!("TCP listener resource {resource}"),
        Object::NativeTlsStream { resource } => format!("TLS stream resource {resource}"),
        Object::DynValue { value, ty } => {
            format!("dynamic type {ty} value {}", show(*value))
        }
    }
}

/// The name of one operation slot. Image data may name any slot, so
/// the dump never indexes the manifest without a bound.
fn op_text(slot: u32) -> String {
    if slot < lm_abi::OP_COUNT {
        lm_abi::op_name(slot)
    } else {
        format!("<operation {slot}>")
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
        Value::Char(value) => format!("{value:?}"),
        Value::Op(op) => format!("<op {}>", op_text(op)),
        Value::EmptyCase { ty, arm } => format!("<empty type {ty} arm {arm}>"),
        Value::Obj(r) => format!("#{}", r.slot),
        Value::Callback(reference) => format!("<callback {}>", reference.slot),
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
