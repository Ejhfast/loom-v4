//! The snapshot admission suite of
//! `docs/specs/snapshot-image-admission.md`.
//!
//! Every case builds one editable image, damages exactly one typed
//! position, and states the rule the damage breaks. An image is
//! editable data, so each damaged state is representable. Only
//! admission decides whether it can restore.
//!
//! The cases keep the heap canonical after every edit, so the
//! canonical-order rule never fires in place of the type rule under
//! test.

use lm_heap::Object;
use lm_testkit::compile_to_bytes;
use lm_value::{ObjRef, Value};
use lm_vm::snapshot::{codec, Image, ImageMachine, ImageObject, ImageReason, ImageTerminal};
use lm_vm::{load_bytes, LoadedModule, RecordingHost, RootEvent, VmConfig, World};

fn program(source: &str) -> LoadedModule {
    let bytes = compile_to_bytes("admission.lm", source).expect("the program compiles");
    load_bytes(&bytes).expect("the program loads")
}

/// Capture the machine world at each instruction boundary of the root.
///
/// The capture runs from the host, so the test needs no guest snapshot
/// code and reaches every program point of the entry function.
fn boundaries(loaded: &LoadedModule, allow: &[&str], limit: usize) -> Vec<Image> {
    let mut world = World::new(loaded, VmConfig::default(), Box::new(RecordingHost::new(1)));
    for grant in allow {
        world.allow(grant).expect("the grant names a target");
    }
    let mut out: Vec<Image> = Vec::new();
    for _ in 0..limit {
        let gate = world.next_gate();
        if let Ok(image) = world.capture_snapshot(gate, 0, false) {
            out.push(image.world().clone());
        }
        match world.step_root() {
            RootEvent::Ran => {}
            _ => break,
        }
    }
    out
}

/// The first captured world that answers the question.
fn pick(images: &[Image], what: &str, ok: impl Fn(&Image) -> bool) -> Image {
    images
        .iter()
        .find(|image| ok(image))
        .unwrap_or_else(|| panic!("no captured world holds {what}"))
        .clone()
}

/// Admit one container. The result is the rule it broke, or `None`
/// when the container admits.
fn admit(loaded: &LoadedModule, image: &Image) -> Option<ImageReason> {
    let bytes = codec::encode(image, usize::MAX).expect("the image encodes");
    match codec::load_external(&bytes, loaded, lm_vm::snapshot::LoadLimits::default()) {
        Ok(_) => None,
        Err(error) => Some(error.reason),
    }
}

/// Rebuild the heap of one machine in canonical traversal order.
///
/// An edit can drop an object out of the reachable set or change the
/// traversal order. The heap of a canonical image is exactly the
/// traversal of its roots, so every edit runs this pass and the
/// canonical-order rule stays out of the way of the rule under test.
fn recanonicalize(machine: &mut ImageMachine) {
    let roots = lm_vm::snapshot::image_roots(machine);
    let mut order: Vec<u32> = Vec::new();
    let mut seen = vec![false; machine.objects.len()];
    let mut stack: Vec<u32> = roots.iter().rev().copied().collect();
    let mut children: Vec<ObjRef> = Vec::new();
    while let Some(r) = stack.pop() {
        if seen[r as usize] {
            continue;
        }
        seen[r as usize] = true;
        order.push(r);
        children.clear();
        machine.objects[r as usize].object.children(&mut children);
        stack.extend(children.iter().rev().map(|c| c.slot));
    }
    let mut moved = vec![u32::MAX; machine.objects.len()];
    for (idx, r) in order.iter().enumerate() {
        moved[*r as usize] = idx as u32;
    }
    let map = |r: ObjRef| ObjRef {
        slot: moved[r.slot as usize],
        generation: 0,
    };
    let objects: Vec<ImageObject> = order
        .iter()
        .map(|r| {
            let entry = &machine.objects[*r as usize];
            ImageObject {
                frozen: entry.frozen,
                object: entry
                    .object
                    .remap(map)
                    .unwrap_or_else(|| entry.object.clone()),
            }
        })
        .collect();
    let value = |v: &mut Value| {
        if let Value::Obj(r) = v {
            *v = Value::Obj(map(*r));
        }
    };
    for frame in &mut machine.frames {
        frame.closure = frame.closure.map(|o| moved[o as usize]);
    }
    for v in machine.locals.iter_mut().chain(machine.operands.iter_mut()) {
        value(v);
    }
    if let Some(pending) = &mut machine.pending {
        for v in &mut pending.args {
            value(v);
        }
    }
    if let Some(ImageTerminal::Done(v)) = &mut machine.terminal {
        value(v);
    }
    for v in &mut machine.mailbox.queue {
        value(v);
    }
    for literal in &mut machine.literals {
        *literal = literal.map(|o| moved[o as usize]);
    }
    machine.start_body = machine.start_body.map(|o| moved[o as usize]);
    machine.objects = objects;
}

/// The ordinal of the first object of one machine that answers the
/// question.
fn find_object(machine: &ImageMachine, what: &str, ok: impl Fn(&Object) -> bool) -> u32 {
    machine
        .objects
        .iter()
        .position(|entry| ok(&entry.object))
        .unwrap_or_else(|| panic!("the machine holds no {what}")) as u32
}

// ---------------------------------------------------------------
// Substituted and opaque program types.
// ---------------------------------------------------------------

const GENERIC_SOURCE: &str = "\
def choose[T](a: T, b: T, first: Bool): T
  if first
    a
  else
    b
  end
end

choose(1, 2, true)
";

/// A local of a generic function holds the type the call site applies.
/// The verifier proves the body once with the type variable opaque, so
/// the substitution comes from the caller frame. A slot typed by a
/// type variable is not a wildcard.
#[test]
fn a_substituted_local_of_the_wrong_shape_rejects() {
    let loaded = program(GENERIC_SOURCE);
    let images = boundaries(&loaded, &[], 20);
    let image = pick(&images, "a frame inside the generic function", |image| {
        image.machines[0].frames.len() == 2 && image.machines[0].locals.contains(&Value::Int(1))
    });
    let mut broken = image.clone();
    let at = broken.machines[0]
        .locals
        .iter()
        .position(|v| *v == Value::Int(1))
        .expect("the applied argument sits in a local");
    broken.machines[0].locals[at] = Value::Bool(true);
    assert_eq!(admit(&loaded, &broken), Some(ImageReason::Layout));
}

/// An operand of a generic frame carries the applied type as well.
#[test]
fn a_substituted_operand_of_the_wrong_shape_rejects() {
    let loaded = program(GENERIC_SOURCE);
    let images = boundaries(&loaded, &[], 20);
    let image = pick(&images, "an operand inside the generic function", |image| {
        image.machines[0].frames.len() == 2
            && image.machines[0]
                .operands
                .iter()
                .any(|v| matches!(v, Value::Int(_)))
    });
    let mut broken = image.clone();
    let at = broken.machines[0]
        .operands
        .iter()
        .position(|v| matches!(v, Value::Int(_)))
        .expect("the operand stack holds the applied value");
    broken.machines[0].operands[at] = Value::Bool(true);
    assert_eq!(admit(&loaded, &broken), Some(ImageReason::Layout));
}

// ---------------------------------------------------------------
// Initialization.
// ---------------------------------------------------------------

const INIT_SOURCE: &str = "\
def go(): Int
  a = 41
  b = a + 1
  b
end

go()
";

/// A slot the verifier proves initialized holds a value of its proved
/// type. The unit value is a value, not a wildcard.
#[test]
fn a_unit_value_in_a_proved_local_rejects() {
    let loaded = program(INIT_SOURCE);
    let images = boundaries(&loaded, &[], 30);
    let image = pick(&images, "an initialized integer local", |image| {
        image.machines[0].locals.contains(&Value::Int(41))
    });
    let mut broken = image.clone();
    let at = broken.machines[0]
        .locals
        .iter()
        .position(|v| *v == Value::Int(41))
        .expect("the local holds the stored integer");
    broken.machines[0].locals[at] = Value::Unit;
    assert_eq!(admit(&loaded, &broken), Some(ImageReason::Layout));
}

/// The uninitialized marker is not a type wildcard either. It is legal
/// only where the verifier proves the slot holds no value.
#[test]
fn an_uninitialized_marker_in_a_proved_local_rejects() {
    let loaded = program(INIT_SOURCE);
    let images = boundaries(&loaded, &[], 30);
    let image = pick(&images, "an initialized integer local", |image| {
        image.machines[0].locals.contains(&Value::Int(41))
    });
    let mut broken = image.clone();
    let at = broken.machines[0]
        .locals
        .iter()
        .position(|v| *v == Value::Int(41))
        .expect("the local holds the stored integer");
    broken.machines[0].locals[at] = Value::Uninit;
    assert_eq!(admit(&loaded, &broken), Some(ImageReason::Layout));
}

// ---------------------------------------------------------------
// Shared objects under two resolved types.
// ---------------------------------------------------------------

const SHARED_SOURCE: &str = "\
def go(): Int
  xs = [1, 2]
  ys = [\"a\", \"b\"]
  xs.len() + ys.len()
end

go()
";

/// One object can sit under two resolved types, and each visit needs
/// its own proof. A walk that keys on the object ordinal alone proves
/// the first type and trusts the object at every other type.
#[test]
fn a_shared_object_checked_under_a_second_type_rejects() {
    let loaded = program(SHARED_SOURCE);
    let images = boundaries(&loaded, &[], 40);
    let image = pick(&images, "both lists", |image| {
        let machine = &image.machines[0];
        machine
            .objects
            .iter()
            .filter(|entry| matches!(entry.object, Object::List { .. }))
            .count()
            == 2
    });
    let machine = &image.machines[0];
    let strings = find_object(machine, "list of strings", |object| match object {
        Object::List { items } => items
            .iter()
            .all(|v| matches!(v, Value::Obj(_)) && !items.is_empty()),
        _ => false,
    });
    let integers = find_object(machine, "list of integers", |object| match object {
        Object::List { items } => {
            !items.is_empty() && items.iter().all(|v| matches!(v, Value::Int(_)))
        }
        _ => false,
    });
    // The local that names the integer list now names the string list.
    // The integer list falls out of the reachable set, so the heap
    // rebuilds in canonical order.
    let mut broken = image.clone();
    let at = broken.machines[0]
        .locals
        .iter()
        .position(|v| {
            *v == Value::Obj(ObjRef {
                slot: integers,
                generation: 0,
            })
        })
        .expect("one local names the integer list");
    broken.machines[0].locals[at] = Value::Obj(ObjRef {
        slot: strings,
        generation: 0,
    });
    recanonicalize(&mut broken.machines[0]);
    assert_eq!(admit(&loaded, &broken), Some(ImageReason::Layout));
}

// ---------------------------------------------------------------
// Generic instance fields.
// ---------------------------------------------------------------

const BOX_SOURCE: &str = "\
class Box[T]
  v: T

  def init(mut self, v: T)
    self.v = v
  end

  def get(self): T
    self.v
  end
end

def go(): Int
  b = Box(41)
  b.get() + 1
end

go()
";

/// An instance field carries the type its class application names. The
/// raw layout type is a type variable, and a variable is not a
/// wildcard.
#[test]
fn a_generic_instance_field_of_the_wrong_shape_rejects() {
    let loaded = program(BOX_SOURCE);
    let images = boundaries(&loaded, &[], 60);
    let image = pick(&images, "the filled box", |image| {
        image.machines[0].objects.iter().any(|entry| {
            matches!(&entry.object, Object::Instance { fields, .. }
                if fields.first() == Some(&Value::Int(41)))
        })
    });
    let mut broken = image.clone();
    for entry in &mut broken.machines[0].objects {
        if let Object::Instance { fields, .. } = &mut entry.object {
            if fields.first() == Some(&Value::Int(41)) {
                fields[0] = Value::Bool(true);
            }
        }
    }
    assert_eq!(admit(&loaded, &broken), Some(ImageReason::Layout));
}

// ---------------------------------------------------------------
// Native relational types.
// ---------------------------------------------------------------

const TWO_MACHINES_SOURCE: &str = "\
def go(): Int with Vm
  a = sys.vm.Vm().from_object(do ||: Int
    41
  end, args: ())
  b = sys.vm.Vm().from_object(do ||: String
    \"answer\"
  end, args: ())
  case a.run()
  in Done(v)  then v
  in Fault(_) then 0
  end
end

go()
";

/// A machine handle carries the result type of the machine it names.
/// The outer object tag proves nothing about that type, so admission
/// reads the target machine.
#[test]
fn a_machine_handle_that_names_another_result_type_rejects() {
    let loaded = program(TWO_MACHINES_SOURCE);
    let images = boundaries(&loaded, &["Vm"], 60);
    // Two machine handles that name two loaded machines of two result
    // types. The swap then keeps the lifecycle state of both targets,
    // so only the result-type rule catches it.
    let pair = |image: &Image| -> Option<(usize, usize)> {
        let loaded_target = |at: usize| -> Option<u32> {
            match image.machines[0].objects[at].object {
                Object::NativeVm { vm } => {
                    let target = &image.machines[vm as usize];
                    (target.state != lm_vm::snapshot::ImageState::Empty
                        && target.result_type.is_some())
                    .then_some(vm)
                }
                _ => None,
            }
        };
        let count = image.machines[0].objects.len();
        for first in 0..count {
            for second in first + 1..count {
                let (Some(a), Some(b)) = (loaded_target(first), loaded_target(second)) else {
                    continue;
                };
                if image.machines[a as usize].result_type != image.machines[b as usize].result_type
                {
                    return Some((first, second));
                }
            }
        }
        None
    };
    let image = pick(
        &images,
        "two loaded machines of two result types",
        |image| pair(image).is_some(),
    );
    let (first_at, second_at) = pair(&image).expect("the capture holds the pair");
    let target = |at: usize| match image.machines[0].objects[at].object {
        Object::NativeVm { vm } => vm,
        _ => unreachable!("the entry is a machine handle"),
    };
    let mut broken = image.clone();
    broken.machines[0].objects[first_at].object = Object::NativeVm {
        vm: target(second_at),
    };
    broken.machines[0].objects[second_at].object = Object::NativeVm {
        vm: target(first_at),
    };
    assert_eq!(admit(&loaded, &broken), Some(ImageReason::Layout));
}

const EMPTY_VM_SOURCE: &str = "\
def go(): Int with Vm
  empty = sys.vm.Vm()
  loaded = sys.vm.Vm().from_object(do ||: Int
    41
  end, args: ())
  case loaded.run()
  in Done(v)  then v
  in Fault(_) then 0
  end
end

go()
";

/// An empty machine handle names a machine with no loaded program.
/// The type states the lifecycle state, so admission reads the target.
#[test]
fn an_empty_machine_handle_that_names_a_loaded_machine_rejects() {
    let loaded = program(EMPTY_VM_SOURCE);
    let images = boundaries(&loaded, &["Vm"], 60);
    let image = pick(&images, "one empty and one loaded machine", |image| {
        image
            .machines
            .iter()
            .any(|m| m.state == lm_vm::snapshot::ImageState::Empty)
            && image
                .machines
                .iter()
                .any(|m| m.state != lm_vm::snapshot::ImageState::Empty && m.result_type.is_some())
            && image.machines[0]
                .objects
                .iter()
                .filter(|entry| matches!(entry.object, Object::NativeVm { .. }))
                .count()
                >= 2
    });
    let empty = image
        .machines
        .iter()
        .position(|m| m.state == lm_vm::snapshot::ImageState::Empty)
        .expect("one machine is empty") as u32;
    let full = image
        .machines
        .iter()
        .position(|m| m.state != lm_vm::snapshot::ImageState::Empty && m.result_type.is_some())
        .expect("one machine is loaded") as u32;
    let mut broken = image.clone();
    for entry in &mut broken.machines[0].objects {
        if let Object::NativeVm { vm } = &mut entry.object {
            if *vm == empty {
                *vm = full;
            }
        }
    }
    assert_eq!(admit(&loaded, &broken), Some(ImageReason::Layout));
}

// ---------------------------------------------------------------
// Terminal results.
// ---------------------------------------------------------------

const TERMINAL_SOURCE: &str = "\
def go(): Int with Vm
  vm = sys.vm.Vm().from_object(do ||: String
    \"answer\"
  end, args: ())
  case vm.run()
  in Done(_)  then 1
  in Fault(_) then 0
  end
end

go()
";

/// A terminal value carries the exact declared result type of its
/// machine. The unit value takes no exception from that rule.
#[test]
fn a_terminal_unit_at_another_result_type_rejects() {
    let loaded = program(TERMINAL_SOURCE);
    let images = boundaries(&loaded, &["Vm"], 60);
    let image = pick(
        &images,
        "a terminal machine with a stored result",
        |image| {
            image.machines.iter().any(|m| {
                matches!(m.terminal, Some(ImageTerminal::Done(Value::Obj(_))))
                    && m.result_type.is_some()
            })
        },
    );
    let at = image
        .machines
        .iter()
        .position(|m| matches!(m.terminal, Some(ImageTerminal::Done(Value::Obj(_)))))
        .expect("one machine stored an object result");
    let mut broken = image.clone();
    broken.machines[at].terminal = Some(ImageTerminal::Done(Value::Unit));
    recanonicalize(&mut broken.machines[at]);
    assert_eq!(admit(&loaded, &broken), Some(ImageReason::Layout));
}

/// The uninitialized marker is not a terminal result either.
#[test]
fn a_terminal_uninitialized_marker_rejects() {
    let loaded = program(TERMINAL_SOURCE);
    let images = boundaries(&loaded, &["Vm"], 60);
    let image = pick(
        &images,
        "a terminal machine with a stored result",
        |image| {
            image.machines.iter().any(|m| {
                matches!(m.terminal, Some(ImageTerminal::Done(Value::Obj(_))))
                    && m.result_type.is_some()
            })
        },
    );
    let at = image
        .machines
        .iter()
        .position(|m| matches!(m.terminal, Some(ImageTerminal::Done(Value::Obj(_)))))
        .expect("one machine stored an object result");
    let mut broken = image.clone();
    broken.machines[at].terminal = Some(ImageTerminal::Done(Value::Uninit));
    recanonicalize(&mut broken.machines[at]);
    assert_eq!(admit(&loaded, &broken), Some(ImageReason::Layout));
}

/// A proc handle carries the mailbox type and the result type of the
/// proc it names. The outer object tag proves neither.
#[test]
fn a_proc_handle_that_names_another_mailbox_rejects() {
    let source = "\
class Inner < Proc[Int]
  def on_spawn(self): Int with Proc
    case self.receive()
    in Msg(n) then n
    in Closed then 0
    end
  end
end

class Outer < Proc[Handle[Int, Int]]
  def on_spawn(self): Int with Proc
    case self.receive()
    in Msg(h)  then 1
    in Closed  then 0
    end
  end
end

def go(): Int with Proc
  inner = Inner.spawn()
  outer = Outer.spawn()
  outer.send(inner)
  1
end

go()
";
    let loaded = program(source);
    let images = boundaries(&loaded, &["Proc"], 80);
    let image = pick(&images, "two spawned procs", |image| {
        image.machines.len() >= 3
            && image.machines[0]
                .objects
                .iter()
                .filter(|entry| matches!(entry.object, Object::NativeHandle { .. }))
                .count()
                >= 2
    });
    // The root holds one handle to each proc. Swap the two targets, so
    // a handle names a proc with another mailbox type.
    let mut found: Vec<usize> = Vec::new();
    for (idx, entry) in image.machines[0].objects.iter().enumerate() {
        if matches!(entry.object, Object::NativeHandle { .. }) {
            found.push(idx);
        }
    }
    let target = |image: &Image, at: usize| match image.machines[0].objects[at].object {
        Object::NativeHandle { proc, .. } => proc,
        _ => unreachable!("the entry is a proc handle"),
    };
    let first = target(&image, found[0]);
    let second = target(&image, found[1]);
    assert_ne!(first, second);
    let mut broken = image.clone();
    // The generation comes from the target, so the handle stays live.
    broken.machines[0].objects[found[0]].object = Object::NativeHandle {
        proc: second,
        generation: image.machines[second as usize].generation,
    };
    broken.machines[0].objects[found[1]].object = Object::NativeHandle {
        proc: first,
        generation: image.machines[first as usize].generation,
    };
    assert_eq!(admit(&loaded, &broken), Some(ImageReason::Layout));
}

/// A call token carries the argument view and the reply type of the
/// exact operation it names.
#[test]
fn a_call_token_of_another_operation_rejects() {
    let source = "\
def go(): Int with Vm, Rand
  held = sys.vm.Vm().from_object(do ||: Int with Rand.Int
    sys.rand.int(0, 10)
  end, args: ())
  case held.drive()
  in Asked(q)
    case q.as_call(Rand.Int)
    in Some(call) then call.args()[0]
    in None       then 0
    end
  in Done(_)  then 0
  in Fault(_) then 0
  end
end

go()
";
    let loaded = program(source);
    let images = boundaries(&loaded, &["Vm", "Rand"], 120);
    let image = pick(&images, "a call token", |image| {
        image.machines[0]
            .objects
            .iter()
            .any(|entry| matches!(entry.object, Object::NativeCall { .. }))
    });
    let at = find_object(&image.machines[0], "call token", |object| {
        matches!(object, Object::NativeCall { .. })
    });
    let mut broken = image.clone();
    if let Object::NativeCall { vm, op, .. } = broken.machines[0].objects[at as usize].object {
        assert_eq!(op, lm_abi::OP_RAND_INT);
        // A stale token is legal, so the token-agreement rule skips it
        // and the call type rule states the failure on its own.
        broken.machines[0].objects[at as usize].object = Object::NativeCall {
            vm,
            ordinal: u64::MAX,
            op: lm_abi::OP_CLOCK_NOW,
        };
    }
    assert_eq!(admit(&loaded, &broken), Some(ImageReason::Layout));
}

// ---------------------------------------------------------------
// The frame chain.
// ---------------------------------------------------------------

/// A frame names the callee of the call site the frame below stopped
/// inside. The substitution of one activation comes from that call
/// site, so a chain that does not agree resolves no type at all.
#[test]
fn a_frame_that_is_not_the_callee_of_its_call_site_rejects() {
    let loaded = program(GENERIC_SOURCE);
    let images = boundaries(&loaded, &[], 20);
    let image = pick(&images, "a two-frame chain", |image| {
        image.machines[0].frames.len() == 2
    });
    let mut broken = image.clone();
    let caller = broken.machines[0].frames[0].func;
    let locals = loaded.module().funcs[caller as usize].local_count() as usize;
    // The frame runs the caller function instead of the callee the
    // call site names. The local arena follows the new frame chain, so
    // the arena rule stays true and the chain rule fires alone.
    broken.machines[0].frames[1].func = caller;
    broken.machines[0].frames[1].block = 0;
    broken.machines[0].frames[1].ip = 0;
    broken.machines[0].locals.resize(2 * locals, Value::Uninit);
    recanonicalize(&mut broken.machines[0]);
    assert_eq!(admit(&loaded, &broken), Some(ImageReason::Type));
}

// ---------------------------------------------------------------
// Nested snapshots.
// ---------------------------------------------------------------

const NESTED_SOURCE: &str = "\
def go(): Int with Vm
  a = sys.vm.Vm().from_object(do ||: Int
    41
  end, args: ())
  b = sys.vm.Vm().from_object(do ||: String
    \"answer\"
  end, args: ())
  first = a.snapshot()
  second = b.snapshot()
  case first
  in Ok(_)  then 1
  in Err(_) then 0
  end
end

go()
";

/// A nested snapshot stays opaque, and its declared root result type
/// still has to match the `Snapshot[T]` that holds it. Admission reads
/// the nested container header and never trusts the outer image.
#[test]
fn a_nested_snapshot_of_another_root_type_rejects() {
    let loaded = program(NESTED_SOURCE);
    let images = boundaries(&loaded, &["Vm"], 120);
    let image = pick(&images, "two nested snapshots", |image| {
        image.machines[0]
            .objects
            .iter()
            .filter(|entry| matches!(entry.object, Object::NativeSnapshot(_)))
            .count()
            == 2
    });
    let mut found: Vec<usize> = Vec::new();
    for (idx, entry) in image.machines[0].objects.iter().enumerate() {
        if matches!(entry.object, Object::NativeSnapshot(_)) {
            found.push(idx);
        }
    }
    let mut broken = image.clone();
    broken.machines[0].objects.swap(found[0], found[1]);
    // The swap moves two payloads of equal shape, so the canonical
    // order stays exact and only the nested root type rule fires.
    assert_eq!(admit(&loaded, &broken), Some(ImageReason::Layout));
}

// ---------------------------------------------------------------
// Structure, order, and budget.
// ---------------------------------------------------------------

/// A snapshot world holds at least its root machine. The decoder can
/// represent an empty world, and admission rejects it.
#[test]
fn a_world_with_no_machine_rejects() {
    let loaded = program(INIT_SOURCE);
    let images = boundaries(&loaded, &[], 5);
    let mut broken = images[0].clone();
    broken.machines.clear();
    broken.result_type = [0u8; 32];
    assert_eq!(admit(&loaded, &broken), Some(ImageReason::State));
}

/// The stored heap is exactly the canonical traversal of its roots.
///
/// The rotation keeps every reference valid and every type accurate,
/// so the canonical-order rule is the one rule left to catch it.
#[test]
fn a_rotated_heap_rejects_as_non_canonical() {
    let loaded = program(SHARED_SOURCE);
    let images = boundaries(&loaded, &[], 40);
    let image = pick(&images, "a heap of two objects or more", |image| {
        image.machines[0].objects.len() >= 2
    });
    let mut broken = image.clone();
    let count = broken.machines[0].objects.len() as u32;
    // Move every object one place up and renumber every reference.
    let map = |r: ObjRef| ObjRef {
        slot: (r.slot + 1) % count,
        generation: 0,
    };
    let machine = &mut broken.machines[0];
    let mut objects = machine.objects.clone();
    objects.rotate_right(1);
    for entry in &mut objects {
        entry.object = entry
            .object
            .remap(map)
            .unwrap_or_else(|| entry.object.clone());
    }
    let value = |v: &mut Value| {
        if let Value::Obj(r) = v {
            *v = Value::Obj(map(*r));
        }
    };
    for frame in &mut machine.frames {
        frame.closure = frame.closure.map(|o| (o + 1) % count);
    }
    for v in machine.locals.iter_mut().chain(machine.operands.iter_mut()) {
        value(v);
    }
    for literal in &mut machine.literals {
        *literal = literal.map(|o| (o + 1) % count);
    }
    machine.objects = objects;
    assert_eq!(admit(&loaded, &broken), Some(ImageReason::Order));
}

/// Admission charges one aggregate budget, so a compact container can
/// never expand into unbounded checking work.
#[test]
fn an_admission_budget_that_runs_out_rejects() {
    let loaded = program(SHARED_SOURCE);
    let images = boundaries(&loaded, &[], 40);
    let image = pick(&images, "a heap of two objects or more", |image| {
        image.machines[0].objects.len() >= 2
    });
    let mut budget = lm_vm::snapshot::AdmissionBudget::new(1);
    let error = lm_vm::snapshot::admit(image.clone(), &loaded, &mut budget)
        .expect_err("the budget runs out");
    assert_eq!(error.reason, ImageReason::Budget);
    // The same image admits under the default budget.
    let mut budget = lm_vm::snapshot::AdmissionBudget::default();
    lm_vm::snapshot::admit(image, &loaded, &mut budget).expect("the image admits");
    assert!(budget.used() > 0);
}

// ---------------------------------------------------------------
// The trusted interpreter boundary.
// ---------------------------------------------------------------

/// Every operation slot an image names is a manifest slot. The runtime
/// reads the manifest by that slot, so an image that names a slot past
/// the manifest would index it out of range.
#[test]
fn an_operation_slot_past_the_manifest_rejects() {
    let loaded = program(TERMINAL_SOURCE);
    let images = boundaries(&loaded, &["Vm"], 60);
    let image = pick(&images, "a terminal machine", |image| {
        image
            .machines
            .iter()
            .any(|m| m.state == lm_vm::snapshot::ImageState::Done)
    });
    let at = image
        .machines
        .iter()
        .position(|m| m.state == lm_vm::snapshot::ImageState::Done)
        .expect("one machine is done");
    let mut broken = image.clone();
    broken.machines[at].state = lm_vm::snapshot::ImageState::Faulted;
    broken.machines[at].terminal = Some(ImageTerminal::Fault(lm_vm::FaultRec {
        code: lm_vm::FaultCode::BoundaryViolation,
        message: "forged".to_string(),
        op: Some(lm_abi::OP_COUNT + 7),
    }));
    recanonicalize(&mut broken.machines[at]);
    let mut budget = lm_vm::snapshot::AdmissionBudget::default();
    let error = lm_vm::snapshot::admit(broken.clone(), &loaded, &mut budget)
        .expect_err("the operation slot must reject");
    assert_eq!(error.reason, ImageReason::Code);
    // The image has no encoding either: the encoder reports the slot
    // instead of indexing the manifest out of range.
    assert!(codec::encode(&broken, usize::MAX).is_err());
}

/// An abstract class is the closed parent of one enum family, and no
/// verified program allocates one. An instance of it would reach the
/// exhaustive-case backstop of every dispatch on the family.
#[test]
fn an_instance_of_an_abstract_class_rejects() {
    let source = "\
def go(): Int
  a: Option[Int] = None
  case a
  in Some(v) then v
  in None    then 41
  end
end

go()
";
    let loaded = program(source);
    let images = boundaries(&loaded, &[], 60);
    let classes = &loaded.module().classes;
    // One abstract family with a case class of the same field count.
    let image = pick(&images, "an instance of an enum case", |image| {
        image.machines.iter().any(|m| {
            m.objects.iter().any(|entry| match &entry.object {
                Object::Instance { class, fields } => {
                    let parent = classes[*class as usize].parent();
                    parent.is_some_and(|p| {
                        classes[p as usize].kind == lm_bytecode::BcClassKind::Abstract
                            && classes[p as usize].fields.len() == fields.len()
                    })
                }
                _ => false,
            })
        })
    });
    let mut broken = image.clone();
    let mut damaged: Option<u32> = None;
    for machine in &mut broken.machines {
        for entry in &mut machine.objects {
            if let Object::Instance { class, fields } = &mut entry.object {
                let parent = classes[*class as usize].parent();
                if let Some(parent) = parent {
                    if classes[parent as usize].kind == lm_bytecode::BcClassKind::Abstract
                        && classes[parent as usize].fields.len() == fields.len()
                    {
                        *class = parent;
                        damaged = Some(parent);
                    }
                }
            }
        }
    }
    let parent = damaged.expect("the capture holds an enum case instance");
    // The manifest must name the family, so the abstract rule fires
    // instead of the manifest rule.
    let hash = loaded
        .identity()
        .expect("the program has an identity")
        .class_hashes[parent as usize];
    broken.classes.push((parent, hash));
    broken.classes.sort_by_key(|(slot, _)| *slot);
    broken.classes.dedup_by_key(|(slot, _)| *slot);
    assert_eq!(admit(&loaded, &broken), Some(ImageReason::State));
}

// ---------------------------------------------------------------
// The admission identity.
// ---------------------------------------------------------------

/// An old container names an older format or ABI version, and
/// admission rejects it. Version 1 spelled an uninitialized local as a
/// unit value, so its images have another meaning.
#[test]
fn a_container_of_an_older_build_rejects() {
    let loaded = program(INIT_SOURCE);
    let images = boundaries(&loaded, &[], 5);
    for edit in 0..4usize {
        let mut broken = images[0].clone();
        match edit {
            0 => broken.format -= 1,
            1 => broken.abi_version -= 1,
            2 => broken.compiler_abi -= 1,
            _ => broken.verifier_version -= 1,
        }
        let mut budget = lm_vm::snapshot::AdmissionBudget::default();
        let error = lm_vm::snapshot::admit(broken, &loaded, &mut budget)
            .expect_err("an older version must reject");
        assert_eq!(error.reason, ImageReason::Version, "field {edit}");
        assert_eq!(error.stage, lm_vm::snapshot::ImageStage::Admission);
    }
    // The same rule holds at the container stage: an old byte string
    // never reaches admission.
    let bytes = codec::encode(&images[0], usize::MAX).expect("the image encodes");
    let mut old = bytes.clone();
    old[8..12].copy_from_slice(&1u32.to_le_bytes());
    let end = old.len() - 32;
    let hash = codec::container_hash(&old[..end]);
    old[end..].copy_from_slice(&hash);
    let error = codec::load_external(&old, &loaded, lm_vm::snapshot::LoadLimits::default())
        .expect_err("an old container must reject");
    assert_eq!(error.reason, ImageReason::Version);
    assert_eq!(error.stage, lm_vm::snapshot::ImageStage::Decode);
}

/// An admitted image records the program and the ABI it passed
/// against, beside its canonical bytes.
#[test]
fn an_admitted_image_records_its_admission_identity() {
    let loaded = program(INIT_SOURCE);
    let images = boundaries(&loaded, &[], 5);
    let mut budget = lm_vm::snapshot::AdmissionBudget::default();
    let admitted = lm_vm::snapshot::admit(images[0].clone(), &loaded, &mut budget)
        .expect("the capture admits");
    let identity = admitted.identity();
    assert_eq!(
        identity.module_semantic,
        loaded
            .identity()
            .expect("the program has an identity")
            .semantic_hash
    );
    assert_eq!(
        identity.verification,
        lm_bytecode::identity::verification_hash(loaded.module())
    );
    assert_eq!(identity.abi_version, lm_abi::ABI_VERSION);
    assert_eq!(
        admitted.origin(),
        lm_vm::snapshot::Origin::ExternalContainer
    );
}

// ---------------------------------------------------------------
// The unchanged world still admits.
// ---------------------------------------------------------------

/// Every capture of the corpus above admits without an edit. The
/// negative cases therefore state a rule, not a broken helper.
#[test]
fn every_captured_world_of_the_corpus_admits() {
    for (source, allow) in [
        (GENERIC_SOURCE, &[][..]),
        (INIT_SOURCE, &[][..]),
        (SHARED_SOURCE, &[][..]),
        (BOX_SOURCE, &[][..]),
        (TWO_MACHINES_SOURCE, &["Vm"][..]),
        (EMPTY_VM_SOURCE, &["Vm"][..]),
        (TERMINAL_SOURCE, &["Vm"][..]),
    ] {
        let loaded = program(source);
        for image in boundaries(&loaded, allow, 60) {
            assert_eq!(admit(&loaded, &image), None, "a clean capture must admit");
        }
    }
}
