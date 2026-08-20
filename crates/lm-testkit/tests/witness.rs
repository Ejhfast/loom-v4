//! The type environment witness suite of
//! `docs/specs/sidecar/snapshot-image-admission.md` section 5.6.
//!
//! The suite proves three groups:
//!
//! - the runtime retains one environment index for every frame,
//!   closure, instance, and machine, and a copy preserves it;
//! - the table is bounded, because the language permits polymorphic
//!   recursion;
//! - admission uses a witness where no derivation exists, and it
//!   rejects a witness that disagrees with a derivation.
//!
//! A witness is provenance. Every case that changes one proves that
//! the value semantics do not move with it.

use lm_heap::Object;
use lm_testkit::compile_to_bytes;
use lm_value::Value;
use lm_vm::snapshot::{codec, Image, ImageReason};
use lm_vm::{load_bytes, LoadedModule, Outcome, RecordingHost, RootEvent, Vm, VmConfig, World};

fn program(source: &str) -> LoadedModule {
    let bytes = compile_to_bytes("witness.lm", source).expect("the program compiles");
    load_bytes(&bytes).expect("the program loads")
}

/// Capture the machine world at each instruction boundary of the root.
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

// ---------------------------------------------------------------
// The bounded table.
// ---------------------------------------------------------------

/// Polymorphic recursion: each level applies `(T, T)` to the next.
///
/// The checker resolves the caller variables and the verifier checks
/// their scope alone, so the program is legal and every level names a
/// closed type the level below has not.
const POLYMORPHIC_RECURSION: &str = "\
def grow[T](v: T, n: Int): Int
  if n <= 0
    0
  else
    grow((v, v), n - 1)
  end
end

grow(1, 40)
";

/// The type environment table of one world is bounded.
///
/// A program can create environments without bound, so the world caps
/// its nodes and answers one local resource fault at the cap. The same
/// program under the default cap runs to its result, so the cap and no
/// other rule decides the outcome.
#[test]
fn the_type_environment_table_is_bounded() {
    let loaded = program(POLYMORPHIC_RECURSION);
    let mut open = Vm::new(&loaded, VmConfig::default());
    assert_eq!(open.run(), Outcome::Done(Value::Int(0)));
    for (types, envs) in [(8u32, 1024u32), (1024, 4)] {
        let capped = VmConfig {
            max_closed_types: types,
            max_type_envs: envs,
            ..VmConfig::default()
        };
        let mut vm = Vm::new(&loaded, capped);
        assert_eq!(
            vm.run(),
            Outcome::Fault(lm_vm::FaultCode::BoundaryLimit),
            "types {types} envs {envs}"
        );
    }
}

// ---------------------------------------------------------------
// A witness is never semantic data.
// ---------------------------------------------------------------

/// Two closures of one generic body under two type arguments.
const TWO_ACTIVATIONS: &str = "\
def hold[T](v: T): Digest
  g = do ||: T v end
  g.digest()
end

def go(): Bool
  hold(1) == hold(2)
end

go()
";

/// A witness never enters a guest digest.
///
/// The two closures below come from two activations of one generic
/// body, so they carry two witnesses. The captured values differ, so
/// the digests differ; the case that follows proves the equal-value
/// direction.
#[test]
fn two_closures_of_two_activations_digest_by_content() {
    let loaded = program(TWO_ACTIVATIONS);
    let mut vm = Vm::new(&loaded, VmConfig::default());
    assert_eq!(vm.run(), Outcome::Done(Value::Bool(false)));
}

/// One generic body and one monomorphic body build the same value.
///
/// The generic activation carries a witness and the monomorphic one
/// carries the empty environment. The two digests must agree, because
/// a witness is provenance and never enters the canonical encoding.
const WITNESS_FREE_DIGEST: &str = "\
class Box
  v: Int = 0
end

def wrap[T](v: T, n: Int): Digest
  b = Box()
  b.v = n
  b.freeze().digest()
end

def plain(n: Int): Digest
  b = Box()
  b.v = n
  b.freeze().digest()
end

def go(): Bool
  wrap(\"tag\", 7) == plain(7)
end

go()
";

#[test]
fn a_witness_never_enters_a_guest_digest() {
    let loaded = program(WITNESS_FREE_DIGEST);
    let mut vm = Vm::new(&loaded, VmConfig::default());
    assert_eq!(vm.run(), Outcome::Done(Value::Bool(true)));
}

// ---------------------------------------------------------------
// A copy preserves the witness.
// ---------------------------------------------------------------

/// A generic body sends its closure into a second machine.
///
/// The closure crosses a machine boundary as a copy, and the copy must
/// keep the environment of the frame that created it. The entry
/// function of the second machine is that closure, so its frame runs
/// under the copied environment and admission proves the whole world.
const CLOSURE_ACROSS_A_BOUNDARY: &str = "\
def hold[T](v: T): Run[T] with Vm
  sys.vm.Vm().activate(do ||: T v end, args: ())
end

def go(): Int with Vm
  vm = hold(41)
  case vm.run()
  in Done(v)  then v
  in Fault(_) then 0
  end
end

go()
";

#[test]
fn a_closure_keeps_its_creator_environment_across_a_boundary() {
    let loaded = program(CLOSURE_ACROSS_A_BOUNDARY);
    let images = boundaries(&loaded, &["Vm"], 80);
    // The held machine runs the copied closure, so its bottom frame
    // and its machine witness both name a non-empty environment.
    let image = pick(&images, "a loaded child machine", |image| {
        image.machines.len() > 1 && image.machines[1].witness != 0
    });
    let child = &image.machines[1];
    assert_ne!(child.witness, 0, "the machine keeps the environment");
    assert!(child
        .frames
        .iter()
        .all(|frame| frame.env == child.witness || frame.env == 0));
    // Every capture of the world admits, which is the property the
    // copy rule carries.
    for image in &images {
        assert_eq!(admit(&loaded, image), None, "a clean capture must admit");
    }
}

// ---------------------------------------------------------------
// Admission uses and checks the witness.
// ---------------------------------------------------------------

/// Every frame environment ordinal resolves during admission.
#[test]
fn a_nonbottom_frame_environment_must_resolve() {
    let loaded = program(
        "def inner(): Int\n  value = 40\n  value + 2\nend\n\n\
         def outer(): Int\n  inner()\nend\n\nouter()\n",
    );
    let images = boundaries(&loaded, &[], 20);
    let mut broken = pick(&images, "a direct callee frame", |image| {
        image.machines[0]
            .frames
            .iter()
            .skip(1)
            .any(|frame| frame.closure.is_none())
    });
    let frame = broken.machines[0]
        .frames
        .iter_mut()
        .skip(1)
        .find(|frame| frame.closure.is_none())
        .expect("the capture holds a direct callee");
    frame.env = u32::MAX;

    let mut budget = lm_vm::snapshot::AdmissionBudget::default();
    let error = lm_vm::snapshot::admit(broken, &loaded, &mut budget)
        .expect_err("the missing environment rejects");
    assert_eq!(error.reason, ImageReason::Reference);
}

/// A frame witness that disagrees with its call site admits.
///
/// A restored frame states its own environment. That is the design:
/// admission derives no type from container data, so the boundary
/// check of a restored perform compares against a type the container
/// chose. The check has teeth where the performing frame is live,
/// which is the machine that called restore.
///
/// The environment is still structural data, so its ordinal must name
/// an entry of the table and its arity must match.
#[test]
fn a_frame_witness_that_disagrees_with_its_call_site_admits() {
    let loaded = program(CLOSURE_ACROSS_A_BOUNDARY);
    let images = boundaries(&loaded, &["Vm"], 80);
    let image = pick(&images, "a frame under a generic call site", |image| {
        image
            .machines
            .iter()
            .any(|m| m.frames.iter().skip(1).any(|f| f.env != 0))
    });
    let mut broken = image.clone();
    let mut damaged = false;
    for machine in &mut broken.machines {
        for frame in machine.frames.iter_mut().skip(1) {
            if frame.env != 0 {
                frame.env = 0;
                damaged = true;
            }
        }
    }
    assert!(damaged, "the capture holds a generic call site");
    assert_eq!(admit(&loaded, &broken), None);
    // An ordinal the table has not still rejects.
    let mut broken = image.clone();
    for machine in &mut broken.machines {
        for frame in machine.frames.iter_mut() {
            frame.env = u32::MAX;
        }
    }
    assert_eq!(admit(&loaded, &broken), Some(ImageReason::Reference));
}

/// A closure witness that disagrees with the frame that runs it
/// rejects.
///
/// The frame reads its captures under its own environment, so a
/// closure that named another environment would type them under a
/// substitution the closure never held.
#[test]
fn a_closure_witness_that_disagrees_with_its_frame_rejects() {
    let loaded = program(CLOSURE_ACROSS_A_BOUNDARY);
    let images = boundaries(&loaded, &["Vm"], 80);
    let image = pick(&images, "a frame with a generic capture context", |image| {
        image
            .machines
            .iter()
            .any(|m| m.frames.iter().any(|f| f.closure.is_some() && f.env != 0))
    });
    let mut broken = image.clone();
    // One more environment of the same arity, so the arity rule does
    // not fire in place of the agreement rule under test.
    let bool_ty = broken.types.len() as u32;
    broken.types.push(lm_bytecode::closed::ClosedType::Bool);
    let other = broken.envs.len() as u32;
    broken.envs.push(lm_bytecode::closed::TypeEnv {
        types: vec![bool_ty],
        rows: vec![],
    });
    let mut damaged = false;
    for machine in &mut broken.machines {
        let targets: Vec<u32> = machine
            .frames
            .iter()
            .filter(|f| f.env != 0)
            .filter_map(|f| match f.closure {
                Some(Value::Obj(reference)) => Some(reference.slot),
                _ => None,
            })
            .collect();
        for ordinal in targets {
            if let Object::Closure { env, .. } = &mut machine.objects[ordinal as usize].object {
                *env = lm_value::Witness(lm_value::TypeEnvId(other));
                damaged = true;
            }
        }
    }
    assert!(damaged, "the capture holds a generic capture context");
    assert_eq!(admit(&loaded, &broken), Some(ImageReason::Reference));
}

/// A machine witness that names another body function rejects.
///
/// The stored proc body and the bottom frame both state the body, so
/// the witness has a derivation wherever one of them exists.
#[test]
fn a_machine_witness_that_names_another_body_rejects() {
    let loaded = program(CLOSURE_ACROSS_A_BOUNDARY);
    let images = boundaries(&loaded, &["Vm"], 80);
    let image = pick(&images, "a loaded child machine with a frame", |image| {
        image.machines.len() > 1 && !image.machines[1].frames.is_empty()
    });
    let mut broken = image.clone();
    broken.machines[1].body_func = Some(broken.machines[0].body_func.expect("the root has a body"));
    assert_eq!(admit(&loaded, &broken), Some(ImageReason::State));
    // The environment half of the witness carries the same rule.
    let mut broken = image.clone();
    broken.machines[1].witness = 0;
    assert_eq!(admit(&loaded, &broken), Some(ImageReason::State));
}

// ---------------------------------------------------------------
// The three findings of round A2.
// ---------------------------------------------------------------

/// A proc, a handle to it, and one accepted message.
const PROC_SOURCE: &str = "\
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
  in Done(v)  then v
  in Fault(_) then 0
  end
end

go()
";

/// A forged proc flag takes the proc birth grant at restore.
///
/// Restore gives the whole `Proc` group to a machine the image marks
/// as a proc. The witness closes it: a machine that claims the flag
/// must name a proc class, and its mailbox type derives from that
/// class alone.
#[test]
fn a_forged_proc_flag_rejects() {
    let loaded = program(PROC_SOURCE);
    let images = boundaries(&loaded, &["Proc"], 200);
    let image = pick(&images, "a proc world with a plain root", |image| {
        image.machines.len() > 1 && !image.machines[0].is_proc
    });
    // The root runs the entry function, which is no proc class.
    let mut broken = image.clone();
    broken.machines[0].is_proc = true;
    assert_eq!(admit(&loaded, &broken), Some(ImageReason::Mailbox));
    // The real proc keeps the flag, so the clean world still admits.
    assert_eq!(admit(&loaded, &image), None);
    assert!(image.machines.iter().any(|m| m.is_proc));
}

/// A portable VM image handle rejects a live registry generation.
#[test]
fn a_vm_image_handle_with_a_live_generation_rejects() {
    let loaded = program(CLOSURE_ACROSS_A_BOUNDARY);
    let images = boundaries(&loaded, &["Vm"], 80);
    let image = pick(&images, "a machine handle", |image| {
        image.machines[0]
            .objects
            .iter()
            .any(|entry| matches!(entry.object, Object::NativeVm { .. }))
    });
    let mut broken = image.clone();
    for entry in &mut broken.machines[0].objects {
        if let Object::NativeVm { generation, .. } = &mut entry.object {
            *generation = 1;
        }
    }
    assert_eq!(admit(&loaded, &broken), Some(ImageReason::Reference));
}

/// One instance with a field the initializer assigns later.
///
/// The default expression performs an operation, so the machine stops
/// at a boundary while the object under construction is still partly
/// initialized.
const UNDER_CONSTRUCTION: &str = "\
class Slow
  a: Int = 0
  b: Int = 0
end

def make(): Slow with Clock
  s = Slow()
  s.a = 1
  s.b = 2
  s
end

def go(): Int with Clock
  s = make()
  s.a + s.b
end

go()
";

/// An uninitialized instance field outside construction admits.
///
/// `New` allocates every field as the marker, and the construction
/// function of the class holds the object until the initializer fills
/// it. A container that puts the marker back past that point states no
/// structural fact, so admission accepts it. `Instr::LoadField` raises
/// `UninitializedField` when verified code reads the slot, so the
/// marker stops one machine.
#[test]
fn an_uninitialized_field_outside_construction_admits() {
    let loaded = program(UNDER_CONSTRUCTION);
    let images = boundaries(&loaded, &["Clock"], 120);
    // Every clean capture admits, including the ones that stop inside
    // the construction function.
    for image in &images {
        assert_eq!(admit(&loaded, image), None, "a clean capture must admit");
    }
    // A complete instance that a local names carries no marker. The
    // edit puts one back at a boundary the construction function has
    // already left, so no frame of the machine allocates that class.
    let builder = loaded
        .module()
        .funcs
        .iter()
        .position(|f| f.name == "<new Slow>")
        .expect("the program declares the construction function") as u32;
    let image = pick(&images, "a complete instance past construction", |image| {
        image.machines[0].frames.iter().all(|f| f.func != builder)
            && image.machines[0].objects.iter().any(|entry| {
                matches!(&entry.object, Object::Instance { fields, .. }
                if fields.len() == 2 && fields.iter().all(|f| *f != Value::Uninit))
            })
    });
    let mut broken = image.clone();
    let mut damaged = false;
    for entry in &mut broken.machines[0].objects {
        if let Object::Instance { fields, .. } = &mut entry.object {
            if fields.len() == 2 && fields.iter().all(|f| *f != Value::Uninit) {
                fields[1] = Value::Uninit;
                damaged = true;
            }
        }
    }
    assert!(damaged, "the capture holds a complete instance");
    assert_eq!(admit(&loaded, &broken), None);
}

// ---------------------------------------------------------------
// Restore re-interns the records.
// ---------------------------------------------------------------

/// A restored world holds its own environment table.
///
/// A `TypeEnvId` belongs to one world, so restore re-interns every
/// record and remaps every stored index. The restored world must run
/// the generic body to the same result.
const RESTORE_SOURCE: &str = "\
def hold[T](v: T): Run[T] with Vm
  sys.vm.Vm().activate(do ||: T v end, args: ())
end

def restore_run(snap: RunSnapshot[Int]): Int with Vm
  case sys.vm.Vm().restore(snap)
  in Ok(restored)
    case restored.run()
    in Done(value) then value
    in Fault(_)    then 0 - 1
    end
  in Err(_) then 0 - 2
  end
end

def go(): Int with Vm
  vm = hold(41)
  case vm.snapshot()
  in Ok(snap) then restore_run(snap)
  in Err(_)   then 0 - 3
  end
end

go()
";

#[test]
fn a_restored_world_re_interns_its_type_environments() {
    let loaded = program(RESTORE_SOURCE);
    let mut world = World::new(
        &loaded,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    world.allow("Vm").expect("the grant names a group");
    assert_eq!(world.run_root(), Outcome::Done(Value::Int(41)));
}

/// A restore charges the type environment cap of the target world.
///
/// The image carries the records of another world, so restore
/// re-interns every one of them. A target that refuses the cap builds
/// no machine at all, and its machine count and its target stay
/// exactly as they were.
#[test]
fn a_restore_past_the_type_cap_leaves_the_target_unchanged() {
    let loaded = program(CLOSURE_ACROSS_A_BOUNDARY);
    // One admitted image of a world whose child machine runs a
    // generic entry function.
    let source = {
        let mut world = World::new(
            &loaded,
            VmConfig::default(),
            Box::new(RecordingHost::new(1)),
        );
        world.allow("Vm").expect("the grant names a group");
        let mut held: Option<lm_vm::snapshot::SnapshotImage> = None;
        for _ in 0..80 {
            let gate = world.next_gate();
            if let Ok(image) = world.capture_snapshot(gate, 0, false) {
                if image.world().machines.len() > 1 && image.world().machines[1].witness != 0 {
                    held = Some(image);
                    break;
                }
            }
            match world.step_root() {
                RootEvent::Ran => {}
                _ => break,
            }
        }
        held.expect("the world reached a generic child machine")
    };
    assert!(!source.world().types.is_empty(), "the image carries types");
    let capped = VmConfig {
        max_closed_types: 1,
        max_type_envs: 1,
        ..VmConfig::default()
    };
    let mut world = World::new(&loaded, capped, Box::new(RecordingHost::new(1)));
    let target = world.new_child(0).expect("the budget holds one child");
    let before = world.machine_count();
    assert_eq!(
        world.restore_image(0, target, &source),
        Err(lm_vm::snapshot::RestoreFail::LimitExceeded)
    );
    assert_eq!(world.machine_count(), before);
    assert_eq!(world.state_of(target), lm_vm::MachineState::Empty);
    // The same image restores into a world with the default caps.
    let mut open = World::new(
        &loaded,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    let target = open.new_child(0).expect("the budget holds one child");
    open.restore_image(0, target, &source)
        .expect("the restore builds a world");
}
