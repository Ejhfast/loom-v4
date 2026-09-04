//! The snapshot admission suite of
//! `docs/language-spec.md` section 17.8.
//!
//! Every case builds one editable image, damages exactly one position,
//! and states what the damage produces. An image is editable data, so
//! each damaged state is representable.
//!
//! Admission proves structure. A structural edit rejects at admission.
//! A wrong type admits, because no rule here derives a type from
//! container data. The interpreter tests the tag at each accessor and
//! the world checks each VM boundary, so a wrong type stops one
//! machine and leaves the host running. Each type case therefore
//! states containment instead of rejection.
//!
//! The cases keep the heap canonical after every edit, so the
//! canonical-order rule never fires in place of the rule under test.

use lm_bytecode::artifact::Artifact;
use lm_heap::Object;
use lm_host::CliHost;
use lm_testkit::{compile_text, load_snapshot_for_artifact, publish_artifact};
use lm_value::{ObjRef, Value};
use lm_vm::snapshot::{codec, Image, ImageMachine, ImageObject, ImageReason, ImageTerminal};
use lm_vm::{FaultCode, Outcome, RecordingHost, RootEvent, VmConfig, World};

fn program(source: &str) -> Artifact {
    compile_text("admission.lm", source).expect("the program compiles")
}

/// Capture the machine world at each instruction boundary of the root.
///
/// The capture runs from the host, so the test needs no guest snapshot
/// code and reaches every program point of the entry function.
fn boundaries(loaded: &Artifact, allow: &[&str], limit: usize) -> Vec<Image> {
    let (arena, namespace) = publish_artifact(loaded).expect("the artifact publishes");
    let mut world = World::new(
        arena,
        namespace,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
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
fn admit(loaded: &Artifact, image: &Image) -> Option<ImageReason> {
    let bytes = codec::encode(image, usize::MAX).expect("the image encodes");
    match load_snapshot_for_artifact(loaded, &bytes, lm_vm::snapshot::LoadLimits::default()) {
        Ok(_) => None,
        Err(error) => Some(error.reason),
    }
}

fn admit_image(
    loaded: &Artifact,
    image: Image,
    budget: &mut lm_vm::snapshot::AdmissionBudget,
) -> Result<lm_vm::snapshot::SnapshotImage, lm_vm::snapshot::ImageError> {
    let (arena, namespace) = publish_artifact(loaded).map_err(|message| {
        lm_vm::snapshot::ImageError::admission(lm_vm::snapshot::ImageReason::Code, message)
    })?;
    let available = arena
        .namespace(namespace)
        .cloned()
        .expect("the namespace exists");
    lm_vm::snapshot::admit(image, Some(available), budget)
}

fn code_tables(loaded: &Artifact) -> std::sync::Arc<lm_bytecode::CodeTables> {
    let (arena, namespace) = publish_artifact(loaded).expect("the artifact publishes");
    arena
        .namespace(namespace)
        .expect("the namespace exists")
        .table_store()
}

#[test]
fn repeated_namespace_admission_rechecks_mutable_image_state() {
    let loaded = program(
        "def add(value: Int): Int\n  value + 1\nend\ndef go(): Int with Vm\n  image = sys.vm.Vm()\n  case image.install(add)\n  in Ok(_) then 42\n  in Err(_) then 0\n  end\nend\ngo()\n",
    );
    let images = boundaries(&loaded, &["Vm"], 200);
    // The root image rides along with the installed image.
    let image = pick(&images, "one installed definition", |image| {
        image.vm_images.iter().any(|vm| vm.instances.len() == 1)
    });
    let bytes = codec::encode(&image, usize::MAX).expect("the image encodes");
    let limits = lm_vm::snapshot::LoadLimits::default();
    load_snapshot_for_artifact(&loaded, &bytes, limits).expect("the first image admits");
    load_snapshot_for_artifact(&loaded, &bytes, limits).expect("the repeated image admits");

    let mut broken = image;
    let installed = broken
        .vm_images
        .iter_mut()
        .find(|vm| vm.instances.len() == 1)
        .expect("the installed image exists");
    installed.instances[0].artifact = u32::MAX;
    let bytes = codec::encode(&broken, usize::MAX).expect("the changed image encodes");
    let error = load_snapshot_for_artifact(&loaded, &bytes, limits)
        .expect_err("the changed instance must reject");
    assert_eq!(error.reason, ImageReason::Reference);
}

#[test]
fn verified_portable_code_cannot_diverge_from_its_artifact_table() {
    let loaded = program(
        "def add(value: Int): Int\n  value + 1\nend\n\
         def keep(): Int\n  code = codeof(add)\n  code.definition().slots.len()\nend\nkeep()\n",
    );
    let images = boundaries(&loaded, &[], 100);
    let image = pick(&images, "one portable function", |image| {
        image.machines.iter().any(|machine| {
            machine.objects.iter().any(|entry| {
                matches!(
                    &entry.object,
                    Object::NativeCode(code) if code.kind == lm_heap::PortableCodeKind::Function
                )
            })
        })
    });
    let limits = lm_vm::snapshot::LoadLimits::default();
    let bytes = codec::encode(&image, usize::MAX).expect("the image encodes");
    load_snapshot_for_artifact(&loaded, &bytes, limits).expect("the first image admits");
    load_snapshot_for_artifact(&loaded, &bytes, limits).expect("the repeated image admits");

    let mut changed = codec::decode(&bytes, limits).expect("the container decodes");
    let artifact = changed
        .artifacts
        .first_mut()
        .expect("the container carries an artifact");
    artifact[0] ^= 1;
    codec::encode(&changed, usize::MAX)
        .expect_err("verified code cannot diverge from its artifact table");
}

#[test]
fn canonical_code_reuse_compares_complete_artifact_bytes() {
    let loaded = program("def go(): Int\n  42\nend\ngo()\n");
    let image = boundaries(&loaded, &[], 10)
        .into_iter()
        .next()
        .expect("the program has one boundary");
    let bytes = codec::encode(&image, usize::MAX).expect("the image encodes");
    let (arena, namespace) = publish_artifact(&loaded).expect("the artifact publishes");
    let mut world = World::new(
        arena,
        namespace,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    world
        .load_snapshot_bytes(&bytes)
        .expect("the original container admits");

    let mut changed = codec::decode(&bytes, lm_vm::snapshot::LoadLimits::default())
        .expect("the container decodes");
    let artifact = changed
        .artifacts
        .first_mut()
        .expect("the container carries an artifact");
    let last = artifact.len() - 1;
    artifact[last] ^= 1;
    let changed = codec::encode(&changed, usize::MAX).expect("the changed image encodes");
    let error = world
        .load_snapshot_bytes(&changed)
        .expect_err("changed artifact bytes must miss the canonical code cache");
    assert_eq!(error.reason, ImageReason::Code);
}

/// Restore one container into a fresh world and drive the restored
/// root to a stop.
///
/// The call answers the fault the restored root took, or `None` when
/// it reached a value. The host must survive either answer, so a
/// return of any kind is the containment this suite states.
fn restore_and_drive(loaded: &Artifact, image: &Image, allow: &[&str]) -> Option<FaultCode> {
    let bytes = codec::encode(image, usize::MAX).expect("the image encodes");
    let admitted =
        load_snapshot_for_artifact(loaded, &bytes, lm_vm::snapshot::LoadLimits::default())
            .expect("the container admits");
    let (arena, namespace) = publish_artifact(loaded).expect("the artifact publishes");
    let mut world = World::new(
        arena,
        namespace,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    // A restored machine starts default-deny and passes through the
    // table of the machine that restored it, so the restorer holds the
    // grants the capture ran under.
    for grant in allow {
        world.allow(grant).expect("the grant names a target");
    }
    let target = world.new_child(0).expect("a child budget");
    let root = world
        .restore_image(0, target, &admitted)
        .expect("the image restores");
    // Every restored machine starts default-deny, so the holder grants
    // the restored root what the capture ran under. `lm snapshot run`
    // does the same.
    for vm in world.machine_ids() {
        for grant in allow {
            world.allow_on(vm, grant).expect("the grant names a target");
        }
    }
    // A restored world may hold procs, so the loop drives the root and
    // every runnable proc. The round bound keeps a restored world that
    // makes no progress from spinning.
    let mut fault: Option<FaultCode> = None;
    let mut root_done = false;
    for _ in 0..64 {
        if !root_done {
            match world.run_machine(root) {
                RootEvent::Fault(rec) => {
                    fault = Some(rec.code);
                    break;
                }
                RootEvent::Done(_) => root_done = true,
                _ => {}
            }
        }
        world.poll_blocked();
        let runnable = world.runnable_procs();
        if runnable.is_empty() {
            break;
        }
        for vm in runnable {
            world.drive_proc(vm);
        }
    }
    // A proc that faulted stops the world it belongs to, so the scan
    // reports the first fault of any machine.
    if fault.is_none() {
        for vm in world.machine_ids() {
            if let Some(rec) = world.fault_of(vm) {
                fault = Some(rec.code);
                break;
            }
        }
    }
    fault
}

/// A wrong-typed container admits, restores, and stops the machine it
/// damaged instead of the host.
fn contained(loaded: &Artifact, image: &Image) {
    contained_with(loaded, image, &[]);
}

fn contained_with(loaded: &Artifact, image: &Image, allow: &[&str]) {
    assert_eq!(
        admit(loaded, image),
        None,
        "admission proves structure, so a wrong type admits"
    );
    restore_and_drive(loaded, image, allow);
}

/// A wrong-typed container admits, and the restored world faults.
fn faults(loaded: &Artifact, image: &Image, allow: &[&str]) -> FaultCode {
    assert_eq!(
        admit(loaded, image),
        None,
        "admission proves structure, so a wrong type admits"
    );
    restore_and_drive(loaded, image, allow).expect("the restored world faults")
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
        if let Some(closure) = &mut frame.closure {
            value(closure);
        }
    }
    for callback in &mut machine.callbacks {
        for capture in &mut callback.captures {
            value(capture);
        }
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
    contained(&loaded, &broken);
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
    contained(&loaded, &broken);
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
    contained(&loaded, &broken);
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
    contained(&loaded, &broken);
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
        Object::List { items, .. } => items
            .iter()
            .all(|v| matches!(v, Value::Obj(_)) && !items.is_empty()),
        _ => false,
    });
    let integers = find_object(machine, "list of integers", |object| match object {
        Object::List { items, .. } => {
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
    contained(&loaded, &broken);
}

// ---------------------------------------------------------------
// Object type coherence.
// ---------------------------------------------------------------

const ALIAS_SOURCE: &str = "\
def go(): Int
  xs: [Int] = []
  ys: [String] = []
  xs.len() + ys.len()
end

go()
";

/// The ordinals of the objects one local of the root machine names and
/// that answer the question, in local order.
fn local_objects(image: &Image, ok: impl Fn(&Object) -> bool) -> Vec<u32> {
    let machine = &image.machines[0];
    let mut out: Vec<u32> = Vec::new();
    for value in &machine.locals {
        let Value::Obj(r) = value else { continue };
        if out.contains(&r.slot) {
            continue;
        }
        if ok(&machine.objects[r.slot as usize].object) {
            out.push(r.slot);
        }
    }
    out
}

/// Point the local that names `what` at the object `to`, and rebuild
/// the heap in canonical order.
fn alias_local(image: &Image, what: u32, to: u32) -> Image {
    let mut broken = image.clone();
    let at = broken.machines[0]
        .locals
        .iter()
        .position(|v| {
            *v == Value::Obj(ObjRef {
                slot: what,
                generation: 0,
            })
        })
        .expect("one local names the object");
    broken.machines[0].locals[at] = Value::Obj(ObjRef {
        slot: to,
        generation: 0,
    });
    recanonicalize(&mut broken.machines[0]);
    broken
}

// ---------------------------------------------------------------
// The type confusion cases that must stop one machine.
// ---------------------------------------------------------------

const READ_LIST_SOURCE: &str = "\
def go(): Int
  xs = [1, 2]
  ys = [\"a\", \"b\"]
  n = ys.len()
  n = n + xs.at(0)
  n
end

go()
";

/// A local typed `[Int]` that names a list of strings faults.
///
/// Admission proves structure, so the container admits. The restored
/// machine then reads element zero and adds it to an integer. The
/// integer reader of the interpreter tests the tag, so the machine
/// stops and the host keeps running.
#[test]
fn a_list_local_that_names_a_list_of_strings_faults() {
    let loaded = program(READ_LIST_SOURCE);
    let images = boundaries(&loaded, &[], 60);
    let ints = |image: &Image| -> Vec<u32> {
        local_objects(image, |object| {
            matches!(object, Object::List { items, .. }
                if !items.is_empty() && items.iter().all(|v| matches!(v, Value::Int(_))))
        })
    };
    let strings = |image: &Image| -> Vec<u32> {
        local_objects(image, |object| {
            matches!(object, Object::List { items, .. }
                if !items.is_empty() && items.iter().all(|v| matches!(v, Value::Obj(_))))
        })
    };
    // The first capture that holds both lists sits before the read.
    let image = pick(&images, "both lists in locals", |image| {
        !ints(image).is_empty() && !strings(image).is_empty()
    });
    let broken = alias_local(&image, ints(&image)[0], strings(&image)[0]);
    assert_eq!(faults(&loaded, &broken, &[]), FaultCode::TypeMismatch);
}

const TWO_CLASSES_SOURCE: &str = "\
class Counter
  n: Int = 7
end

class Label
  text: String = \"x\"
end

def go(): Int
  a = Counter()
  b = Label()
  m = 1
  m + a.n
end

go()
";

/// An instance of one class at a position of another class faults.
///
/// The local typed `Counter` names a `Label`, so the field read
/// answers a string where the code proved an integer. The interpreter
/// tests the tag at the addition.
#[test]
fn an_instance_at_a_position_of_another_class_faults() {
    let loaded = program(TWO_CLASSES_SOURCE);
    let images = boundaries(&loaded, &[], 60);
    let counters = |image: &Image| -> Vec<u32> {
        local_objects(image, |object| {
            matches!(object, Object::Instance { fields, .. }
                if fields.len() == 1 && matches!(fields[0], Value::Int(_)))
        })
    };
    let labels = |image: &Image| -> Vec<u32> {
        local_objects(image, |object| {
            matches!(object, Object::Instance { fields, .. }
                if fields.len() == 1 && matches!(fields[0], Value::Obj(_)))
        })
    };
    let image = pick(&images, "both instances in locals", |image| {
        !counters(image).is_empty() && !labels(image).is_empty()
    });
    let broken = alias_local(&image, counters(&image)[0], labels(&image)[0]);
    assert_eq!(faults(&loaded, &broken, &[]), FaultCode::TypeMismatch);
}

/// One typed edge proves that edge alone. Two locals typed `[Int]` and
/// `[String]` can name one empty mutable list, and each edge passes
/// because the list holds no element. Verified code then appends an
/// integer through the first local and reads a string through the
/// second.
#[test]
fn one_empty_list_under_two_element_types_rejects() {
    let loaded = program(ALIAS_SOURCE);
    let images = boundaries(&loaded, &[], 40);
    let empty = |image: &Image| -> Vec<u32> {
        local_objects(
            image,
            |object| matches!(object, Object::List { items, .. } if items.is_empty()),
        )
    };
    let image = pick(&images, "two empty lists in locals", |image| {
        empty(image).len() == 2
    });
    let found = empty(&image);
    let broken = alias_local(&image, found[1], found[0]);
    contained(&loaded, &broken);
}

const BAG_SOURCE: &str = "\
class Bag[T]
  v: [T]

  def init(mut self, v: [T])
    self.v = v
  end

  def size(self): Int
    self.v.len()
  end
end

def go(): Int
  a: Bag[Int] = Bag([])
  b: Bag[String] = Bag([])
  a.size() + b.size()
end

go()
";

/// A generic instance with an empty collection field carries the same
/// defect. The exact type of an instance is its concrete class with its
/// concrete arguments, so `Bag[Int]` and `Bag[String]` cannot name one
/// object.
#[test]
fn one_generic_instance_under_two_arguments_rejects() {
    let loaded = program(BAG_SOURCE);
    let images = boundaries(&loaded, &[], 80);
    let bags = |image: &Image| -> Vec<u32> {
        local_objects(image, |object| {
            matches!(object, Object::Instance { fields, .. }
                if fields.len() == 1 && matches!(fields[0], Value::Obj(_)))
        })
    };
    let image = pick(&images, "two bags in locals", |image| {
        bags(image).len() == 2
    });
    let found = bags(&image);
    let broken = alias_local(&image, found[1], found[0]);
    contained(&loaded, &broken);
}

const SUBCLASS_ALIAS_SOURCE: &str = "\
class Beast
  legs: Int = 4
end

class Hound < Beast
  def bark(self): Int
    self.legs
  end
end

def go(): Int
  d = Hound()
  a: Beast = d
  d.bark() + a.legs
end

go()
";

/// A subclass instance reached from a parent-typed edge and a
/// child-typed edge is ordinary code. The coherence rule normalizes
/// both edges through the concrete class, so the world still admits.
#[test]
fn one_instance_at_a_parent_and_a_child_edge_admits() {
    let loaded = program(SUBCLASS_ALIAS_SOURCE);
    let images = boundaries(&loaded, &[], 80);
    let mut count = 0usize;
    for image in &images {
        let shared = image.machines[0]
            .objects
            .iter()
            .enumerate()
            .any(|(idx, entry)| {
                matches!(entry.object, Object::Instance { .. })
                    && image.machines[0]
                        .locals
                        .iter()
                        .filter(|v| {
                            **v == Value::Obj(ObjRef {
                                slot: idx as u32,
                                generation: 0,
                            })
                        })
                        .count()
                        == 2
            });
        if !shared {
            continue;
        }
        count += 1;
        assert_eq!(admit(&loaded, image), None, "one instance at two edges");
    }
    assert!(count > 0, "no capture aliased the instance from two locals");
}

const INHERITED_PARENT_SOURCE: &str = "\
class Box[T]
  v: T

  def init(mut self, v: T)
    self.v = v
  end
end

class IntBox < Box[Int]
  def init(mut self, v: Int)
    super.init(v)
  end
end

def go(): Int
  s: Box[String] = Box(\"hello\")
  i: IntBox = IntBox(7)
  j = i.v
  k = s.v
  j
end

go()
";

/// Point the local that names the string box at the `IntBox`.
///
/// The image then states that a position the verifier proved
/// `Box[String]` holds an instance of a class that inherits
/// `Box[Int]`. The class test alone accepts it, because `IntBox`
/// inherits `Box`. Only the argument test refuses it.
fn forge_inherited_parent(images: &[Image]) -> Vec<Image> {
    let mut out: Vec<Image> = Vec::new();
    for image in images {
        let machine = &image.machines[0];
        let boxes: Vec<u32> = machine
            .objects
            .iter()
            .enumerate()
            .filter_map(|(idx, entry)| match &entry.object {
                Object::Instance { fields, .. } if fields.len() == 1 => Some(idx as u32),
                _ => None,
            })
            .collect();
        let int_box = boxes.iter().copied().find(|slot| {
            matches!(machine.objects[*slot as usize].object,
                Object::Instance { ref fields, .. } if fields[0] == Value::Int(7))
        });
        let str_box = boxes.iter().copied().find(|slot| {
            matches!(&machine.objects[*slot as usize].object,
                Object::Instance { fields, .. } if matches!(fields[0], Value::Obj(_)))
        });
        let (Some(int_box), Some(str_box)) = (int_box, str_box) else {
            continue;
        };
        if !machine.locals.contains(&Value::Obj(ObjRef {
            slot: str_box,
            generation: 0,
        })) {
            continue;
        }
        out.push(alias_local(image, str_box, int_box));
    }
    out
}
/// An instance of an inherited generic parent at another argument
/// admits, and the restored world stays contained.
///
/// `IntBox` inherits `Box[Int]` and takes no type parameter of its
/// own, so a nominal test accepts it in a `Box[String]` position.
/// Admission proves structure alone, so the container admits. The
/// restored world then reads an `Int` where the code proved a
/// `String`, and the interpreter tests the tag at that read.
#[test]
fn an_inherited_generic_parent_at_another_argument_stays_contained() {
    let loaded = program(INHERITED_PARENT_SOURCE);
    let images = boundaries(&loaded, &[], 200);
    let forged = forge_inherited_parent(&images);
    assert!(!forged.is_empty(), "no capture held both boxes");
    for broken in &forged {
        contained(&loaded, broken);
    }
}

// ---------------------------------------------------------------
// Dynamic dispatch.
// ---------------------------------------------------------------

const OVERRIDE_SOURCE: &str = "\
class Animal
  name: String = \"animal\"

  def speak(self): Int
    0
  end
end

class Dog < Animal
  def speak(self): Int
    self.legs() + 1
  end

  def legs(self): Int
    4
  end
end

def go(): Int
  a: Animal = Dog()
  a.speak()
end

go()
";

/// A method call dispatches on the class of the receiver value, not on
/// the static type of the call site. A frame inside an overriding
/// method therefore names a function the static type does not resolve.
///
/// A capture inside an overriding method admits and restores.
#[test]
fn a_frame_inside_an_overridden_method_admits() {
    let loaded = program(OVERRIDE_SOURCE);
    let images = boundaries(&loaded, &[], 60);
    // The overriding method is the one `Dog` declares. A frame that
    // names it never resolves through the static receiver type
    // `Animal`, because `Animal` resolves the selector to its own
    // method.
    let tables = code_tables(&loaded);
    let dog = tables
        .classes
        .iter()
        .find(|c| c.name == "Dog")
        .expect("the program declares Dog");
    let overrides: Vec<u32> = dog.methods.iter().map(|(_, func)| *func).collect();
    let mut count = 0usize;
    for image in &images {
        if !image.machines[0]
            .frames
            .iter()
            .any(|f| overrides.contains(&f.func))
        {
            continue;
        }
        count += 1;
        assert_eq!(admit(&loaded, image), None, "a frame inside an override");
    }
    assert!(count > 0, "no capture stopped inside the override");
}

const INTERFACE_FRAME_SOURCE: &str = "\
interface Priced
  def price(self): Int
end

final class Book implements Priced
  def price(self): Int
    12
  end
end

def describe[P: Priced](item: P): Int
  item.price()
end

describe(Book())
";

/// An interface call selects one method through a generic bound.
/// Every frame inside that method is a valid stop point.
#[test]
fn a_frame_inside_a_generic_interface_call_admits() {
    let loaded = program(INTERFACE_FRAME_SOURCE);
    let tables = code_tables(&loaded);
    let method = tables
        .classes
        .iter()
        .find(|class| class.name == "Book")
        .and_then(|class| class.methods.first())
        .map(|(_, function)| *function)
        .expect("the program declares Book.price");
    let images = boundaries(&loaded, &[], 80);
    let mut count = 0usize;
    for image in &images {
        if !image.machines[0]
            .frames
            .iter()
            .any(|frame| frame.func == method)
        {
            continue;
        }
        count += 1;
        assert_eq!(admit(&loaded, image), None, "a generic interface frame");
    }
    assert!(count > 0, "no capture stopped inside Book.price");
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
    contained(&loaded, &broken);
}

// ---------------------------------------------------------------
// Native relational types.
// ---------------------------------------------------------------

const TWO_MACHINES_SOURCE: &str = "\
def go(): Int with Vm
  a = sys.vm.Vm().activate_or_fault(do ||: Int
    41
  end, args: ())
  b = sys.vm.Vm().activate_or_fault(do ||: String
    \"answer\"
  end, args: ())
  case a.run()
  in Ok(v)  then v
  in Err(_) then 0
  end
end

go()
";

/// A run handle carries the result type of the run it names.
/// The outer object tag proves nothing about that type, so admission
/// reads the target machine.
#[test]
fn a_machine_handle_that_names_another_result_type_faults() {
    let loaded = program(TWO_MACHINES_SOURCE);
    let images = boundaries(&loaded, &["Vm"], 60);
    let tables = code_tables(&loaded);
    // Two run handles name two loaded machines of two result
    // types. The swap then keeps the lifecycle state of both targets,
    // so only the result-type rule catches it.
    let pair = |image: &Image| -> Option<(usize, usize)> {
        let loaded_target = |at: usize| -> Option<u32> {
            match image.machines[0].objects[at].object {
                Object::NativeRun { vm } => {
                    let target = &image.machines[vm as usize];
                    (target.state != lm_vm::snapshot::ImageState::Empty
                        && target.body_func.is_some())
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
                // The machine witness names the body function, and the
                // result type of that function is the machine result
                // type.
                let ret = |vm: u32| -> Option<u32> {
                    let func = image.machines[vm as usize].body_func?;
                    Some(tables.funcs[func as usize].ret)
                };
                if ret(a) != ret(b) {
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
        Object::NativeRun { vm } => vm,
        _ => unreachable!("the entry is a run handle"),
    };
    let mut broken = image.clone();
    broken.machines[0].objects[first_at].object = Object::NativeRun {
        vm: target(second_at),
    };
    broken.machines[0].objects[second_at].object = Object::NativeRun {
        vm: target(first_at),
    };
    assert_eq!(faults(&loaded, &broken, &["Vm"]), FaultCode::TypeMismatch);
}

const VM_IMAGE_SOURCE: &str = "\
def go(): Int with Vm
  empty = sys.vm.Vm()
  loaded = sys.vm.Vm().activate_or_fault(do ||: Int
    41
  end, args: ())
  case loaded.run()
  in Ok(v)  then v
  in Err(_) then 0
  end
end

go()
";

/// A portable VM image handle always uses generation zero.
#[test]
fn a_vm_image_handle_with_a_live_generation_rejects() {
    let loaded = program(VM_IMAGE_SOURCE);
    let images = boundaries(&loaded, &["Vm"], 60);
    let image = pick(&images, "a VM image handle", |image| {
        image.machines.iter().any(|machine| {
            machine
                .objects
                .iter()
                .any(|entry| matches!(entry.object, Object::NativeVm { .. }))
        })
    });
    let mut broken = image.clone();
    for machine in &mut broken.machines {
        for entry in &mut machine.objects {
            if let Object::NativeVm { generation, .. } = &mut entry.object {
                *generation = 1;
            }
        }
    }
    assert_eq!(admit(&loaded, &broken), Some(ImageReason::Reference));
}

// ---------------------------------------------------------------
// Terminal results.
// ---------------------------------------------------------------

const TERMINAL_SOURCE: &str = "\
def go(): Int with Vm
  vm = sys.vm.Vm().activate_or_fault(do ||: String
    \"answer\"
  end, args: ())
  case vm.run()
  in Ok(_)  then 1
  in Err(_) then 0
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
                    && m.body_func.is_some()
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
    contained(&loaded, &broken);
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
                    && m.body_func.is_some()
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
    contained(&loaded, &broken);
}

const PROC_SOURCE: &str = "\
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
  case inner.done()
  in Ok(v)  then v
  in Err(_) then 0
  end
end

go()
";

/// A proc handle carries the mailbox type and the result type of the
/// proc it names. The outer object tag proves neither.
#[test]
fn a_proc_handle_that_names_another_mailbox_faults() {
    let loaded = program(PROC_SOURCE);
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
    assert_eq!(faults(&loaded, &broken, &["Proc"]), FaultCode::TypeMismatch);
}

const CALL_TOKEN_SOURCE: &str = "\
def go(): Int with Vm, Rand
  held = sys.vm.Vm().activate_or_fault(do ||: Int with Rand.Int
    sys.rand.int(0, 10)
  end, args: ())
  case held.drive()
  in Asked(q)
    case q
    in Call(Rand.Int, _, (low, _)) then low
    in _                           then 0
    end
  in Done(_)  then 0
  in Fault(_) then 0
  end
end

go()
";

/// A call token carries the argument view and the reply type of the
/// exact operation it names.
#[test]
fn a_call_token_of_another_operation_rejects() {
    let loaded = program(CALL_TOKEN_SOURCE);
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
            ordinal: 0,
            op: lm_abi::OP_CLOCK_NOW,
        };
    }
    contained(&loaded, &broken);
}

#[test]
fn a_future_request_ordinal_rejects() {
    let loaded = program(CALL_TOKEN_SOURCE);
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
        let future = broken.machines[vm as usize].next_ordinal;
        broken.machines[0].objects[at as usize].object = Object::NativeCall {
            vm,
            ordinal: future,
            op,
        };
    }
    assert_eq!(admit(&loaded, &broken), Some(ImageReason::State));
}

// ---------------------------------------------------------------
// The frame chain.
// ---------------------------------------------------------------

/// A frame names the callee of the call site the frame below stopped
/// inside. A chain that does not agree states a stop the runtime never
/// reaches.
#[test]
fn a_frame_that_is_not_the_callee_of_its_call_site_rejects() {
    let loaded = program(GENERIC_SOURCE);
    let images = boundaries(&loaded, &[], 20);
    let image = pick(&images, "a two-frame chain", |image| {
        image.machines[0].frames.len() == 2
    });
    let mut broken = image.clone();
    let caller = broken.machines[0].frames[0].func;
    let tables = code_tables(&loaded);
    let locals = tables.funcs[caller as usize].local_count() as usize;
    // The frame runs the caller function instead of the callee the
    // call site names. The local arena follows the new frame chain, so
    // the arena rule stays true and the chain rule fires alone.
    broken.machines[0].frames[1].func = caller;
    broken.machines[0].frames[1].block = 0;
    broken.machines[0].frames[1].ip = 0;
    broken.machines[0].locals.resize(2 * locals, Value::Uninit);
    recanonicalize(&mut broken.machines[0]);
    assert_eq!(admit(&loaded, &broken), Some(ImageReason::Layout));
}

// ---------------------------------------------------------------
// The operand partition.
// ---------------------------------------------------------------

const PICK_SOURCE: &str = "\
def pick(a: Int, b: Int): Int
  a
end

def go(): Int
  xs = [10, 20]
  xs.at(pick(0, 1))
end

go()
";

/// A frame retains exactly the operands its program point proves, less
/// every operand the instruction it stopped inside consumed.
///
/// The edit gives the caller one extra operand under the value the
/// callee returns. Every type rule passes, because the inserted value
/// takes the type of the first call argument. The extra value survives
/// the return, and the caller then reads an integer where the verifier
/// proved an object.
#[test]
fn an_operand_hidden_under_a_call_rejects() {
    let loaded = program(PICK_SOURCE);
    let images = boundaries(&loaded, &[], 60);
    let image = pick(&images, "a call two frames deep", |image| {
        image.machines[0].frames.len() >= 3
    });
    let mut broken = image.clone();
    let machine = &mut broken.machines[0];
    let top = machine.frames.len() - 1;
    let base = machine.frames[top].base_operand as usize;
    machine.operands.insert(base, Value::Int(0));
    machine.frames[top].base_operand += 1;
    contained(&loaded, &broken);
}

/// The operand arena starts at the base of the bottom frame. A value
/// below that base belongs to no frame, so no program point proves it.
#[test]
fn a_value_below_the_bottom_frame_base_rejects() {
    let loaded = program(PICK_SOURCE);
    let images = boundaries(&loaded, &[], 60);
    let image = pick(&images, "a machine with a frame", |image| {
        !image.machines[0].frames.is_empty()
    });
    let mut broken = image.clone();
    let machine = &mut broken.machines[0];
    machine.operands.insert(0, Value::Int(0x4141_4141));
    for frame in &mut machine.frames {
        frame.base_operand += 1;
    }
    assert_eq!(admit(&loaded, &broken), Some(ImageReason::Layout));
}

/// The number of arguments one perform records comes from the perform
/// instruction, never from the record.
#[test]
fn a_pending_request_with_another_argument_count_rejects() {
    let loaded = program(CALL_TOKEN_SOURCE);
    let images = boundaries(&loaded, &["Vm", "Rand"], 120);
    let image = pick(&images, "a machine stopped inside a perform", |image| {
        image
            .machines
            .iter()
            .any(|m| m.pending.as_ref().is_some_and(|p| !p.args.is_empty()))
    });
    let at = image
        .machines
        .iter()
        .position(|m| m.pending.as_ref().is_some_and(|p| !p.args.is_empty()))
        .expect("one machine holds a pending request");
    let mut broken = image.clone();
    let pending = broken.machines[at]
        .pending
        .as_mut()
        .expect("the machine holds a pending request");
    // The extra argument is an immediate, so the heap stays canonical
    // and the count rule states the failure alone.
    pending.args.push(Value::Int(0));
    assert_eq!(admit(&loaded, &broken), Some(ImageReason::State));
}

// ---------------------------------------------------------------
// Terminal and stopped states.
// ---------------------------------------------------------------

const FAULTED_SOURCE: &str = "\
def go(): String with Vm
  vm = sys.vm.Vm().activate_or_fault(do || with Io.Write
    print(\"hi\\n\")
  end, args: ())
  case vm.run()
  in Ok(_)  then \"done\"
  in Err(f) then f.code()
  end
end

go()
";

/// A fault leaves every frame in place, so a faulted machine holds the
/// frames it stopped in. Those frames are diagnostic state: the machine
/// never executes again.
///
/// A world with a faulted machine admits and restores.
#[test]
fn a_faulted_machine_that_holds_frames_admits() {
    let loaded = program(FAULTED_SOURCE);
    let images = boundaries(&loaded, &["Vm"], 80);
    let mut count = 0usize;
    for image in &images {
        let faulted = image
            .machines
            .iter()
            .any(|m| m.state == lm_vm::snapshot::ImageState::Faulted && !m.frames.is_empty());
        if !faulted {
            continue;
        }
        count += 1;
        assert_eq!(admit(&loaded, image), None, "a faulted machine with frames");
    }
    assert!(count > 0, "no capture held a faulted machine with frames");
}

/// A `Done` machine reaches its terminal by returning its last frame,
/// so it holds none.
#[test]
fn a_done_machine_that_holds_a_frame_rejects() {
    let loaded = program(TERMINAL_SOURCE);
    let images = boundaries(&loaded, &["Vm"], 60);
    let image = pick(&images, "a done machine", |image| {
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
    let live = image
        .machines
        .iter()
        .find(|m| !m.frames.is_empty())
        .expect("one machine holds a frame");
    let mut broken = image.clone();
    broken.machines[at].frames = vec![live.frames[0].clone()];
    broken.machines[at].frames[0].closure = None;
    assert_eq!(admit(&loaded, &broken), Some(ImageReason::State));
}

const ASKED_SOURCE: &str = "\
def go(): Int with Vm, Io
  vm = sys.vm.Vm().activate_or_fault(do ||: Int with Io.Write
    print(\"hi\\n\")
    41
  end, args: ())
  case vm.drive()
  in Asked(_)  then 1
  in Done(_)   then 2
  in Fault(_)  then 3
  end
end

go()
";

/// A machine stopped `Asked` records its request before any host
/// attachment opens, and the holder answers it. The live attachment
/// belongs to `Waiting`, and the capture refuses that state.
///
/// An asked machine on a host operation admits and restores.
#[test]
fn an_asked_machine_on_a_host_operation_admits() {
    let loaded = program(ASKED_SOURCE);
    let images = boundaries(&loaded, &["Vm", "Io"], 120);
    let mut count = 0usize;
    for image in &images {
        let asked = image.machines.iter().any(|m| {
            m.state == lm_vm::snapshot::ImageState::Asked
                && m.pending
                    .as_ref()
                    .is_some_and(|p| lm_abi::op(p.op).suspends())
        });
        if !asked {
            continue;
        }
        count += 1;
        assert_eq!(admit(&loaded, image), None, "an asked machine on Io.Write");
    }
    assert!(
        count > 0,
        "no capture stopped a machine on a host operation"
    );
}

// ---------------------------------------------------------------
// Nested snapshots.
// ---------------------------------------------------------------

const NESTED_SOURCE: &str = "\
def go(): Int with Vm
  a = sys.vm.Vm().activate_or_fault(do ||: Int
    41
  end, args: ())
  b = sys.vm.Vm().activate_or_fault(do ||: String
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
/// still has to match the `RunSnapshot[T]` that holds it. Admission reads
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
    contained(&loaded, &broken);
}

// ---------------------------------------------------------------
// Structure, order, and budget.
// ---------------------------------------------------------------

/// A run snapshot holds its distinguished machine.
#[test]
fn a_world_with_no_machine_rejects() {
    let loaded = program(INIT_SOURCE);
    let images = boundaries(&loaded, &[], 5);
    let mut broken = images[0].clone();
    broken.machines.clear();
    broken.result_type = [0u8; 32];
    assert_eq!(admit(&loaded, &broken), Some(ImageReason::SectionBounds));
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
        if let Some(closure) = &mut frame.closure {
            value(closure);
        }
    }
    for callback in &mut machine.callbacks {
        for capture in &mut callback.captures {
            value(capture);
        }
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
    let error = admit_image(&loaded, image.clone(), &mut budget).expect_err("the budget runs out");
    assert_eq!(error.reason, ImageReason::Budget);
    // The same image admits under the default budget.
    let mut budget = lm_vm::snapshot::AdmissionBudget::default();
    admit_image(&loaded, image, &mut budget).expect("the image admits");
    assert!(budget.used() > 0);
}

/// The ledger charges every stored object and every table entry.
///
/// Admission walks the canonical order of each machine heap and
/// resolves both witness tables. A container with many objects
/// therefore costs many units, and a ledger below that count stops
/// the pass.
#[test]
fn the_budget_charges_every_stored_object() {
    let loaded = program(SHARED_SOURCE);
    let images = boundaries(&loaded, &[], 40);
    let image = pick(&images, "a heap with objects", |image| {
        image.machines[0].objects.len() >= 4
    });
    let objects = image.machines[0].objects.len() as u64;
    let mut budget = lm_vm::snapshot::AdmissionBudget::default();
    admit_image(&loaded, image.clone(), &mut budget).expect("the image admits");
    assert!(
        budget.used() >= objects,
        "the ledger charged {} units for {objects} objects",
        budget.used()
    );
    // A ledger below the object count stops the pass.
    let mut small = lm_vm::snapshot::AdmissionBudget::new(objects / 2);
    let error = admit_image(&loaded, image, &mut small).expect_err("the small budget runs out");
    assert_eq!(error.reason, ImageReason::Budget);
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
        trace: Vec::new(),
    }));
    recanonicalize(&mut broken.machines[at]);
    let mut budget = lm_vm::snapshot::AdmissionBudget::default();
    let error = admit_image(&loaded, broken.clone(), &mut budget)
        .expect_err("the operation slot must reject");
    assert_eq!(error.reason, ImageReason::Code);
    // The image has no encoding either: the encoder reports the slot
    // instead of indexing the manifest out of range.
    assert!(codec::encode(&broken, usize::MAX).is_err());
}

/// The seal reports the rule the encoder broke. A container past its
/// byte limit breaks the limit rule, and an operation slot the manifest
/// has not breaks the code rule.
#[test]
fn a_sealed_image_past_the_byte_limit_reports_the_limit_rule() {
    let loaded = program(SHARED_SOURCE);
    let images = boundaries(&loaded, &[], 40);
    let image = pick(&images, "a heap of two objects or more", |image| {
        image.machines[0].objects.len() >= 2
    });
    let mut budget = lm_vm::snapshot::AdmissionBudget::default().with_byte_limit(64);
    let error =
        admit_image(&loaded, image, &mut budget).expect_err("the container passes the byte limit");
    assert_eq!(error.reason, ImageReason::LimitExceeded);
    assert_eq!(error.stage, lm_vm::snapshot::ImageStage::Admission);
}

const TABLE_SOURCE: &str = "\
def go(): Int with Vm, Io
  held = sys.vm.Vm().activate_or_fault(do ||: Int
    41
  end, args: ())
  held.table().pass(Io)
  case held.run()
  in Ok(v)  then v
  in Err(_) then 0
  end
end

go()
";

/// A policy table handle comes from a machine handle, and no operation
/// mints a handle to the performing machine. A machine that held a
/// table handle to itself could pass any effect group to itself, past
/// the fresh default-deny table of specification 17.5.
#[test]
fn a_policy_table_handle_to_its_own_machine_rejects() {
    let loaded = program(TABLE_SOURCE);
    let images = boundaries(&loaded, &["Vm", "Io"], 80);
    let image = pick(&images, "a policy table handle", |image| {
        image.machines[0]
            .objects
            .iter()
            .any(|entry| matches!(entry.object, Object::NativeTable { .. }))
    });
    let mut broken = image.clone();
    for entry in &mut broken.machines[0].objects {
        if let Object::NativeTable { vm } = &mut entry.object {
            *vm = 0;
        }
    }
    assert_eq!(admit(&loaded, &broken), Some(ImageReason::Reference));
}

/// An abstract class is the closed parent of one enum family, and no
/// verified program allocates one. An instance of it would reach the
/// exhaustive-case backstop of every dispatch on the family.
const ABSTRACT_SOURCE: &str = "\
def go(): Int
  a: Ordering = Ordering.Equal
  case a
  in Less    then 0
  in Equal   then 41
  in Greater then 2
  end
end

go()
";

#[test]
fn an_instance_of_an_abstract_class_rejects() {
    let loaded = program(ABSTRACT_SOURCE);
    let images = boundaries(&loaded, &[], 60);
    let tables = code_tables(&loaded);
    let classes = &tables.classes;
    // One abstract family with a case class of the same field count.
    let image = pick(&images, "an instance of an enum case", |image| {
        image.machines.iter().any(|m| {
            m.objects.iter().any(|entry| match &entry.object {
                Object::Instance { class, fields, .. } => {
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
            if let Object::Instance { class, fields, .. } = &mut entry.object {
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
    assert!(parent < classes.len() as u32);
    assert_eq!(admit(&loaded, &broken), Some(ImageReason::State));
}

// ---------------------------------------------------------------
// The admission identity.
// ---------------------------------------------------------------

/// Admission rejects a container with another format or ABI version.
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
        let error =
            admit_image(&loaded, broken, &mut budget).expect_err("an older version must reject");
        assert_eq!(error.reason, ImageReason::Version, "field {edit}");
        assert_eq!(error.stage, lm_vm::snapshot::ImageStage::Admission);
    }
    // A container with another version never reaches admission.
    let bytes = codec::encode(&images[0], usize::MAX).expect("the image encodes");
    let mut old = bytes.clone();
    old[8..12].copy_from_slice(&1u32.to_le_bytes());
    let end = old.len() - 32;
    let hash = codec::container_hash(&old[..end]);
    old[end..].copy_from_slice(&hash);
    let error = load_snapshot_for_artifact(&loaded, &old, lm_vm::snapshot::LoadLimits::default())
        .expect_err("an old container must reject");
    assert_eq!(error.reason, ImageReason::Version);
    assert_eq!(error.stage, lm_vm::snapshot::ImageStage::Decode);
}

/// An admitted image records the format and ABI it passed.
#[test]
fn an_admitted_image_records_its_admission_identity() {
    let loaded = program(INIT_SOURCE);
    let images = boundaries(&loaded, &[], 5);
    let mut budget = lm_vm::snapshot::AdmissionBudget::default();
    let admitted =
        admit_image(&loaded, images[0].clone(), &mut budget).expect("the capture admits");
    let identity = admitted.identity();
    assert_eq!(identity.format, lm_vm::snapshot::FORMAT_VERSION);
    assert_eq!(identity.abi_version, lm_abi::ABI_VERSION);
    assert_eq!(identity.bundle_digest, lm_abi::standard_bundle().digest());
    assert_eq!(
        admitted.origin(),
        lm_vm::snapshot::Origin::ExternalContainer
    );
}

/// A self-contained image restores in another program.
#[test]
fn a_restore_of_another_program_uses_the_image_artifacts() {
    let captured = program(INIT_SOURCE);
    let running = program(SHARED_SOURCE);
    let images = boundaries(&captured, &[], 10);
    let mut budget = lm_vm::snapshot::AdmissionBudget::default();
    let admitted = admit_image(&captured, images[1].clone(), &mut budget)
        .expect("the capture admits against its own program");
    let (arena, namespace) = publish_artifact(&running).expect("the artifact publishes");
    let mut world = World::new(
        arena,
        namespace,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    let target = world.new_child(0).expect("a child budget");
    world
        .restore_image(0, target, &admitted)
        .expect("the image restores in another program");
    // The same image restores into the program it names.
    let (arena, namespace) = publish_artifact(&captured).expect("the artifact publishes");
    let mut own = World::new(
        arena,
        namespace,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    let target = own.new_child(0).expect("a child budget");
    own.restore_image(0, target, &admitted)
        .expect("the image restores into its own program");
}

/// Foreign restore relocates byte caches and preserves ABI effect rows.
#[test]
fn a_foreign_restore_relocates_bytes_and_preserves_effect_rows() {
    let captured = program(
        "def hold[effect e](task: () -> Int with e, bytes: Bytes): String\n\
           i = 0\n\
           while i < 20\n\
             i = i + 1\n\
           end\n\
           bytes.hex()\n\
         end\n\
         hold({ ||: Int with Clock.Now 1 }, b\"\\x00\\xff\")\n",
    );
    let images = boundaries(&captured, &[], 100);
    let image = pick(&images, "a byte literal and effect row", |image| {
        image.envs.iter().any(|env| !env.rows.is_empty())
            && image
                .machines
                .iter()
                .any(|machine| machine.literals.iter().any(Option::is_some))
    });
    let mut budget = lm_vm::snapshot::AdmissionBudget::default();
    let admitted = admit_image(&captured, image, &mut budget).expect("the capture admits");
    let running = program(SHARED_SOURCE);
    let (arena, namespace) = publish_artifact(&running).expect("the other artifact publishes");
    let mut world = World::new(
        arena,
        namespace,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    let target = world.new_child(0).expect("the restore target exists");
    let root = world
        .restore_image(0, target, &admitted)
        .expect("the foreign image restores");
    match world.run_machine(root) {
        RootEvent::Done(value) => assert_eq!(world.show_result_of(root, value), "\"00ff\""),
        other => panic!("the foreign image stopped: {other:?}"),
    }
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
        (VM_IMAGE_SOURCE, &["Vm"][..]),
        (TERMINAL_SOURCE, &["Vm"][..]),
    ] {
        let loaded = program(source);
        for image in boundaries(&loaded, allow, 60) {
            assert_eq!(admit(&loaded, &image), None, "a clean capture must admit");
        }
    }
}

/// An editable image reference uses generation zero.
#[test]
fn a_nonzero_image_reference_generation_rejects() {
    let loaded = program(SHARED_SOURCE);
    let images = boundaries(&loaded, &[], 60);
    let mut broken = pick(&images, "an object-valued local", |image| {
        image.machines.iter().any(|machine| {
            machine
                .locals
                .iter()
                .any(|value| matches!(value, Value::Obj(_)))
        })
    });
    let reference = broken
        .machines
        .iter_mut()
        .flat_map(|machine| machine.locals.iter_mut())
        .find_map(|value| match value {
            Value::Obj(reference) => Some(reference),
            _ => None,
        })
        .expect("the capture holds an object-valued local");
    reference.generation = 1;

    let mut budget = lm_vm::snapshot::AdmissionBudget::default();
    let error =
        admit_image(&loaded, broken, &mut budget).expect_err("the nonzero generation rejects");
    assert_eq!(error.reason, ImageReason::Reference);
}
// ---------------------------------------------------------------
// Every capture of every shipped program admits.
// ---------------------------------------------------------------

/// The grants given to the root of every program.
///
/// A grant widens what one program reaches, so one list serves the
/// whole corpus.
const GATE_GRANTS: [&str; 11] = [
    "Vm", "Io", "Fs", "Proc", "Clock", "Rand", "Compiler", "Reflect", "Env", "Args", "Entropy",
];

/// The bytecode boundaries the gate drives for one program.
const GATE_BOUNDARIES: usize = 400;

/// A closure built inside a generic body. Its capture and parameter
/// types name the type variable of the enclosing generic.
const CLOSURE_IN_GENERIC: &str = "\
def hold[T](v: T): T
  g = { ||: T v }
  g()
end

hold(41)
";

/// A machine whose entry function is generic: the frame of the held
/// machine carries a substitution no call site of the image states.
const GENERIC_ENTRY: &str = "\
def hold[T](v: T): Run[T] with Vm
  sys.vm.Vm().activate_or_fault({ |x: T|: T x }, args: (v,))
end

def go(): Int with Vm
  vm = hold(41)
  case vm.run()
  in Ok(v)  then v
  in Err(_) then 0
  end
end

go()
";

/// A direct generic parameter uses the entry frame environment.
#[test]
fn a_generic_entry_parameter_crosses_the_vm_boundary() {
    let loaded = program(GENERIC_ENTRY);
    let (arena, namespace) = publish_artifact(&loaded).expect("the artifact publishes");
    let mut world = World::new(
        arena,
        namespace,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    world.allow("Vm").expect("the grant names a group");
    assert_eq!(world.run_root(), Outcome::Done(Value::Int(41)));
}

/// A proc handle that outlives the constructor of its proc.
const PROC_HANDLE: &str = "\
class Worker < Proc[Int]
  def on_spawn(self): Int with Proc
    case self.receive()
    in Msg(n)  then n
    in Closed  then 0
    end
  end
end

def go(): Int with Proc
  h = Worker.spawn()
  h.send(7)
  case h.done()
  in Ok(v)  then v
  in Err(_) then 0
  end
end

go()
";

/// A handle that `sys.proc.run` produced. Its mailbox type is `Never`,
/// and no proc class stands behind it.
const PROC_RUN_HANDLE: &str = "\
def go(): Int with Proc, Vm
  vm = sys.vm.Vm().activate_or_fault({ ||: Int 41 + 1 }, args: ())
  h = sys.proc.run(vm)
  case h.done()
  in Ok(v)  then v
  in Err(_) then 0
  end
end

go()
";

/// A closure inside a closure inside a generic body. The inner capture
/// type names the variable of the outer generic.
const NESTED_CLOSURE_IN_GENERIC: &str = "\
def hold[T](v: T): T
  outer = do ||: T
    inner = { ||: T v }
    inner()
  end
  outer()
end

hold(41)
";

/// A generic class with a generic field, reached at two arguments.
const GENERIC_FIELD: &str = "\
class Box[T]
  item: T

  def init(mut self, item: T)
    self.item = item
  end
end

class Pair2[A]
  left: Box[A]

  def init(mut self, left: Box[A])
    self.left = left
  end
end

def go(): Int
  p = Pair2(Box(41))
  q = Box(true)
  if q.item
    p.left.item + 1
  else
    0
  end
end

go()
";

/// Polymorphic recursion, captured while the recursion is live.
const POLYMORPHIC_RECURSION: &str = "\
def depth[T](v: T, n: Int): Int
  if n <= 0
    0
  else
    1 + depth((v, v), n - 1)
  end
end

depth(1, 3)
";

/// Every program the gate drives: the label, the source, and whether
/// the source came from a file.
fn gate_corpus() -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    // The shipped examples. `examples/05-modules` and
    // `examples/16-text-editor` hold package projects. Those projects
    // need the package compiler driver.
    let mut files: Vec<std::path::PathBuf> = Vec::new();
    collect_lm(&lm_testkit::repo_root().join("examples"), &mut files);
    files.sort();
    for path in files {
        let name = path
            .strip_prefix(lm_testkit::repo_root())
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        if name.starts_with("examples/05-modules/") || name.starts_with("examples/16-text-editor/")
        {
            continue;
        }
        let source = std::fs::read_to_string(&path).expect("the example reads");
        out.push((name, source));
    }
    // The sources of this suite, plus the two shapes round A2 owns.
    for (label, source) in [
        ("generic-source", GENERIC_SOURCE),
        ("init-source", INIT_SOURCE),
        ("shared-source", SHARED_SOURCE),
        ("box-source", BOX_SOURCE),
        ("two-machines-source", TWO_MACHINES_SOURCE),
        ("vm-image-source", VM_IMAGE_SOURCE),
        ("terminal-source", TERMINAL_SOURCE),
        ("nested-source", NESTED_SOURCE),
        ("proc-source", PROC_SOURCE),
        ("call-token-source", CALL_TOKEN_SOURCE),
        ("abstract-source", ABSTRACT_SOURCE),
        ("override-source", OVERRIDE_SOURCE),
        ("table-source", TABLE_SOURCE),
        ("alias-source", ALIAS_SOURCE),
        ("bag-source", BAG_SOURCE),
        ("subclass-alias-source", SUBCLASS_ALIAS_SOURCE),
        ("inherited-parent-source", INHERITED_PARENT_SOURCE),
        ("pick-source", PICK_SOURCE),
        ("faulted-source", FAULTED_SOURCE),
        ("asked-source", ASKED_SOURCE),
        ("proc-handle", PROC_HANDLE),
        ("closure-in-a-generic-body", CLOSURE_IN_GENERIC),
        ("a-generic-entry-function", GENERIC_ENTRY),
        ("proc-run-handle", PROC_RUN_HANDLE),
        (
            "nested-closure-in-a-generic-body",
            NESTED_CLOSURE_IN_GENERIC,
        ),
        ("a-generic-field", GENERIC_FIELD),
        ("polymorphic-recursion", POLYMORPHIC_RECURSION),
        ("read-list-source", READ_LIST_SOURCE),
        ("two-classes-source", TWO_CLASSES_SOURCE),
    ] {
        out.push((label.to_string(), source.to_string()));
    }
    out
}

/// Collect every `.lm` file below one directory.
fn collect_lm(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("the directory reads") {
        let path = entry.expect("a directory entry").path();
        if path.is_dir() {
            collect_lm(&path, out);
        } else if path.extension().map(|e| e == "lm").unwrap_or(false) {
            out.push(path);
        }
    }
}

/// Drive the whole world of one program and admit every capture.
///
/// The call answers one line per capture that did not admit.
fn gate_sweep(label: &str, source: &str) -> Vec<String> {
    let artifact =
        compile_text("gate.lm", source).unwrap_or_else(|e| panic!("{label} does not compile: {e}"));
    let (arena, namespace) =
        publish_artifact(&artifact).unwrap_or_else(|e| panic!("{label} does not load: {e}"));
    let uses_compiler_host = source.contains("sys.compiler.") || source.contains("sys.reflect.");
    let host: Box<dyn lm_vm::Host> = if uses_compiler_host {
        Box::new(CliHost::new(1))
    } else {
        Box::new(RecordingHost::new(1))
    };
    let mut world = World::new(arena, namespace, VmConfig::default(), host);
    for grant in GATE_GRANTS {
        world.allow(grant).expect("the grant names a group");
    }
    let mut out: Vec<String> = Vec::new();
    let mut captures = 0usize;
    for boundary in 0..GATE_BOUNDARIES {
        let gate = world.next_gate();
        if let Ok(image) = world.capture_snapshot(gate, 0, false) {
            captures += 1;
            let bytes = image.bytes().expect("the image encodes");
            if let Err(e) = world.load_snapshot_bytes(bytes) {
                out.push(format!("{label} boundary {boundary}: {e}"));
            }
        }
        match world.state_of(0) {
            lm_vm::MachineState::Ready | lm_vm::MachineState::Waiting => match world.step_root() {
                RootEvent::Ran | RootEvent::Blocked | RootEvent::Asked(_) => {}
                _ => break,
            },
            lm_vm::MachineState::Blocked => {
                if world.poll_blocked() == 0 {
                    match world.runnable_procs().first().copied() {
                        Some(proc) => {
                            world.drive_proc(proc);
                        }
                        None => {
                            if world.wait_host_completion(|_| true).is_none() {
                                break;
                            }
                        }
                    }
                }
            }
            _ => break,
        }
    }
    assert!(captures > 0, "{label}: no capture succeeded at all");
    out
}

/// The positive control of the whole admission rule.
///
/// Trusted capture writes the state of a legal world. Those bytes
/// must pass the external loader, which repeats every admission rule
/// with no trust at all. A rule that refuses a legal world is as
/// serious as a rule that admits a forgery.
///
/// The sweep covers the whole corpus with no exclusion. The three
/// shapes the type environment witness unblocked stay in the corpus:
/// a proc handle past the constructor, a closure a generic body built,
/// and a machine whose entry function is generic.
#[test]
fn every_capture_of_every_shipped_program_admits() {
    const WORKERS: usize = 4;
    let corpus = gate_corpus();
    let worker_count = WORKERS.min(corpus.len());
    let fails = std::thread::scope(|scope| {
        let mut workers = Vec::with_capacity(worker_count);
        for offset in 0..worker_count {
            let corpus = &corpus;
            workers.push(scope.spawn(move || {
                let mut failures = Vec::new();
                for (label, source) in corpus.iter().skip(offset).step_by(worker_count) {
                    failures.extend(gate_sweep(label, source));
                }
                failures
            }));
        }
        let mut failures = Vec::new();
        for worker in workers {
            failures.extend(worker.join().expect("an admission worker does not panic"));
        }
        failures
    });
    assert!(
        fails.is_empty(),
        "{} capture(s) of a legal world did not admit:\n{}",
        fails.len(),
        fails.join("\n")
    );
}

// ---------------------------------------------------------------
// Restore applies the effective ceiling to live state.
// ---------------------------------------------------------------

/// A program that loops until its own budget runs out.
const SPIN_SOURCE: &str = "\
def spin(n: Int): Int
  total = 0
  i = 0
  while i < n
    total = total + i
    i = i + 1
  end
  total
end

spin(1000000)
";

/// A container states the fuel its machine holds, so restore must
/// clamp that fuel by the ceiling of the target world.
///
/// `clamp` bounds the configured budget. The interpreter reads
/// `vm.fuel` instead, so an unclamped copy let an image state any
/// budget. A restored machine then ran a loop of the victim program
/// without end, and the host never returned.
#[test]
fn a_restored_machine_takes_the_fuel_ceiling_of_its_target() {
    let loaded = program(SPIN_SOURCE);
    let images = boundaries(&loaded, &[], 40);
    let mut broken = pick(&images, "a running loop", |image| {
        !image.machines[0].frames.is_empty()
    });
    // The image claims every budget it can state.
    broken.machines[0].fuel = u64::MAX;
    broken.machines[0].limits.fuel = u64::MAX;
    assert_eq!(
        admit(&loaded, &broken),
        None,
        "admission proves structure, so a large budget admits"
    );

    let bytes = codec::encode(&broken, usize::MAX).expect("the image encodes");
    let admitted =
        load_snapshot_for_artifact(&loaded, &bytes, lm_vm::snapshot::LoadLimits::default())
            .expect("the container admits");
    let ceiling = VmConfig {
        fuel: 2000,
        ..VmConfig::default()
    };
    let (arena, namespace) = publish_artifact(&loaded).expect("the artifact publishes");
    let mut world = World::new(arena, namespace, ceiling, Box::new(RecordingHost::new(1)));
    let target = world.new_child(0).expect("a child budget");
    let root = world
        .restore_image(0, target, &admitted)
        .expect("the image restores");
    // The loop needs far more than the ceiling, so the restored
    // machine stops instead of running without end.
    assert!(
        matches!(world.run_machine(root), RootEvent::Fault(rec) if rec.code == FaultCode::OutOfFuel),
        "the restored machine must run out of fuel"
    );
}

// ---------------------------------------------------------------
// The boundary check follows the restore of a world.
// ---------------------------------------------------------------

/// A world that restored nothing runs no boundary check, and a restore
/// turns the check on for the whole world.
///
/// Every value of a world that restored nothing came out of verified
/// code, so the check would state a rule the verifier already proved.
/// A restore states values the verifier never saw, so the check must
/// cover every later crossing of that world.
///
/// The four type-confusion cases of this file prove the second half by
/// construction: each one restores a forged image and then faults. This
/// case states the flag itself, so a later change cannot leave the
/// check off after a restore and still pass them by accident.
#[test]
fn a_restore_turns_the_boundary_check_on_for_its_world() {
    let loaded = program(READ_LIST_SOURCE);
    let images = boundaries(&loaded, &[], 60);
    let image = pick(&images, "any captured world", |_| true);

    let (arena, namespace) = publish_artifact(&loaded).expect("the artifact publishes");
    let mut world = World::new(
        arena,
        namespace,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    assert!(
        !world.restored_any(),
        "a fresh world restored nothing, so it checks no boundary"
    );

    let bytes = codec::encode(&image, usize::MAX).expect("the image encodes");
    let admitted =
        load_snapshot_for_artifact(&loaded, &bytes, lm_vm::snapshot::LoadLimits::default())
            .expect("the container admits");
    let target = world.new_child(0).expect("a child budget");
    world
        .restore_image(0, target, &admitted)
        .expect("the image restores");
    assert!(
        world.restored_any(),
        "a committed restore turns the check on"
    );
}
