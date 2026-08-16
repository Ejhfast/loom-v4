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
    let image = pick(&images, "two loaded machines", |image| {
        image
            .machines
            .iter()
            .filter(|m| m.result_type.is_some())
            .count()
            >= 2
            && image.machines[0]
                .objects
                .iter()
                .filter(|entry| matches!(entry.object, Object::NativeVm { .. }))
                .count()
                >= 2
    });
    let mut broken = image.clone();
    // Swap the two machine handles. Every ordinal stays in range, so
    // only the relational type rule catches it.
    let mut found: Vec<usize> = Vec::new();
    for (idx, entry) in broken.machines[0].objects.iter().enumerate() {
        if matches!(entry.object, Object::NativeVm { .. }) {
            found.push(idx);
        }
    }
    assert!(found.len() >= 2, "the capture holds two machine handles");
    let first = match broken.machines[0].objects[found[0]].object {
        Object::NativeVm { vm } => vm,
        _ => unreachable!("the entry is a machine handle"),
    };
    let second = match broken.machines[0].objects[found[1]].object {
        Object::NativeVm { vm } => vm,
        _ => unreachable!("the entry is a machine handle"),
    };
    assert_ne!(first, second);
    broken.machines[0].objects[found[0]].object = Object::NativeVm { vm: second };
    broken.machines[0].objects[found[1]].object = Object::NativeVm { vm: first };
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
