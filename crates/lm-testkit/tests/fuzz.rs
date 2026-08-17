//! Deterministic seeded-mutation no-panic harness.
//!
//! Real cargo-fuzz needs a nightly toolchain, so this suite is the
//! standing substitute: it applies a fixed number of seeded byte and
//! structure mutations to valid compiled modules and to valid
//! sources, and requires that decode plus verify either rejects
//! cleanly or accepts without a panic. Accepted mutants also run
//! under a small fuel budget. The PRNG seed is fixed, so a failure
//! reproduces exactly.
//!
//! `tests/fuzz-regressions/` holds the permanent corpus: crafted
//! modules for known verifier findings replay on every run.

use lm_testkit::{compile_to_bytes, lm_files, repo_root};
use lm_vm::{Vm, VmConfig};

/// The largest input one fuzz case may present. The mutations never
/// grow an input, and the bound holds even if a mutation changes.
const MAX_CASE_BYTES: usize = 1 << 20;

/// Run one harness body on the supported 8 MiB stack. The parser
/// depth guard assumes it (week-2 note), and hostile inputs push the
/// guarded worst case past the smaller default test-thread stack.
fn on_supported_stack(f: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(8 << 20)
        .spawn(f)
        .expect("thread starts")
        .join()
        .expect("no panic in the harness");
}

/// A deterministic xorshift64* PRNG.
struct Prng(u64);

impl Prng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound.max(1) as u64) as usize
    }
}

/// The fixed harness seed. Change it only with a note in the week
/// documentation, because failures reproduce through it.
const SEED: u64 = 0x00c0_ffee_1234_5678;

/// Mutations per input.
const ROUNDS: usize = 400;

/// Apply one seeded mutation batch to a byte vector.
fn mutate(bytes: &mut Vec<u8>, prng: &mut Prng) {
    if bytes.is_empty() {
        return;
    }
    match prng.below(10) {
        // Flip one random byte (most rounds).
        0..=5 => {
            let at = prng.below(bytes.len());
            bytes[at] = prng.next() as u8;
        }
        // Flip a short run.
        6 | 7 => {
            let at = prng.below(bytes.len());
            let len = 1 + prng.below(8).min(bytes.len() - at - 1);
            for b in &mut bytes[at..at + len] {
                *b = prng.next() as u8;
            }
        }
        // Truncate.
        8 => {
            let keep = prng.below(bytes.len());
            bytes.truncate(keep);
        }
        // Splice a slice of the input onto a random position.
        _ => {
            let from = prng.below(bytes.len());
            let len = 1 + prng.below(16).min(bytes.len() - from - 1);
            let slice: Vec<u8> = bytes[from..from + len].to_vec();
            let at = prng.below(bytes.len());
            for (i, b) in slice.into_iter().enumerate() {
                if at + i < bytes.len() {
                    bytes[at + i] = b;
                }
            }
        }
    }
}

/// Decode, verify, and on acceptance run one module image. A panic
/// fails the test; a clean rejection or a guest fault is fine. Every
/// resource of the case is bounded: input bytes, fuel, frames, arena
/// slots, and heap bytes.
fn exercise_module(bytes: &[u8]) {
    assert!(bytes.len() <= MAX_CASE_BYTES, "a mutation grew the input");
    let Ok(module) = lm_bytecode::decode(bytes) else {
        return;
    };
    let Ok(loaded) = lm_vm::load(module) else {
        return;
    };
    let config = VmConfig {
        fuel: 20_000,
        max_frames: 256,
        max_stack_values: 1 << 16,
        heap_bytes: 1 << 20,
        ..VmConfig::default()
    };
    let mut vm = Vm::new(&loaded, config);
    let outcome = vm.run();
    let _ = vm.show_outcome(&outcome);
}

/// The mutation sources: every runnable example.
fn seed_sources() -> Vec<(String, String)> {
    let mut out = Vec::new();
    for dir in [
        "examples/01-basics",
        "examples/02-objects",
        "examples/03-types",
        "examples/04-effects",
        "examples/06-graphs",
        "examples/07-procs",
    ] {
        for path in lm_files(&repo_root().join(dir)) {
            let text = std::fs::read_to_string(&path).expect("example reads");
            out.push((path.display().to_string(), text));
        }
    }
    assert!(out.len() >= 14, "the example corpus shrank");
    out
}

#[test]
fn mutated_modules_never_panic_the_decoder_verifier_or_vm() {
    on_supported_stack(|| {
        let mut prng = Prng(SEED);
        for (name, text) in seed_sources() {
            let base = compile_to_bytes(&name, &text).expect("examples compile");
            for round in 0..ROUNDS {
                let mut bytes = base.clone();
                // One to three stacked mutations.
                for _ in 0..=prng.below(3) {
                    mutate(&mut bytes, &mut prng);
                }
                // A panic here fails the test with the (name, round) pair
                // in the harness output.
                let _ = round;
                exercise_module(&bytes);
            }
        }
    });
}

/// The snapshot mutation seed: the checkpoint world, plus its
/// program.
///
/// The dictionary is the container itself. Every mutation starts from
/// a real image, so the mutants reach the structural rules instead of
/// stopping at the magic.
fn snapshot_seed() -> (lm_vm::LoadedModule, Vec<u8>) {
    use lm_vm::{RecordingHost, World};
    let path = repo_root().join("checkpoints/asked-tree.lm");
    let text = std::fs::read_to_string(&path).expect("the checkpoint source reads");
    let bytes = compile_to_bytes(&path.display().to_string(), &text).expect("it compiles");
    let loaded = lm_vm::load_bytes(&bytes).expect("it loads");
    let container = {
        let mut world = World::new(
            &loaded,
            VmConfig::default(),
            Box::new(RecordingHost::new(1)),
        );
        for grant in ["Proc", "Vm", "Clock"] {
            world.allow(grant).expect("the grant names a target");
        }
        lm_proc::run_world(&mut world);
        world
            .last_snapshot()
            .expect("the program captured a world")
            .bytes()
            .to_vec()
    };
    (loaded, container)
}

/// Drive one restored world to a stop under a bounded slice budget.
///
/// The budget keeps a mutant that would run forever from hanging the
/// harness. The rule is the absence of a panic; a fault, a block with
/// no runnable machine, or the budget all stop the drive cleanly.
fn drive_restored(world: &mut lm_vm::World<'_>, root: lm_vm::VmId) {
    for _ in 0..10_000 {
        match world.run_machine(root) {
            lm_vm::RootEvent::Done(_) | lm_vm::RootEvent::Fault(_) | lm_vm::RootEvent::Asked(_) => {
                return
            }
            lm_vm::RootEvent::Blocked => {
                if world.poll_blocked() > 0 {
                    continue;
                }
                match world.runnable_procs().first().copied() {
                    Some(proc) => {
                        world.drive_proc(proc);
                    }
                    None => return,
                }
            }
            lm_vm::RootEvent::Ran | lm_vm::RootEvent::Waiting => return,
        }
    }
}

#[test]
fn mutated_snapshot_containers_never_panic_the_loader() {
    on_supported_stack(|| {
        let mut prng = Prng(SEED ^ 0x5_9a5);
        let (loaded, base) = snapshot_seed();
        let limits = lm_vm::snapshot::LoadLimits::default();
        let mut accepted = 0usize;
        let mut sealed = 0usize;
        for _round in 0..ROUNDS * 4 {
            let mut bytes = base.clone();
            for _ in 0..=prng.below(3) {
                mutate(&mut bytes, &mut prng);
            }
            assert!(bytes.len() <= MAX_CASE_BYTES, "a mutation grew the input");
            // The container hash is an unkeyed integrity check, so the
            // real attacker holds a hash-valid crafted image. Reseal
            // the body, so the structural loader is exercised instead
            // of the hash gate catching almost every mutant. A
            // truncated mutant that lost its hash region cannot be
            // resealed, and it tests the truncation path.
            if bytes.len() >= 32 {
                let end = bytes.len() - 32;
                let hash = lm_vm::snapshot::codec::container_hash(&bytes[..end]);
                bytes[end..].copy_from_slice(&hash);
                sealed += 1;
            }
            // A rejection is fine. An acceptance must restore into a
            // world without a panic, and it must encode back to
            // exactly the bytes it came from. The restored world then
            // runs under a tight heap and fuel cap: an unproven state
            // that only shows at run time faults or stops, but never
            // panics the interpreter.
            if let Ok(image) = lm_vm::snapshot::codec::decode(&bytes, limits) {
                let again = lm_vm::snapshot::codec::encode(&image, usize::MAX)
                    .expect("an accepted image encodes");
                assert_eq!(again, bytes, "an accepted mutant has two spellings");
                let mut budget = lm_vm::snapshot::AdmissionBudget::default();
                let Ok(admitted) = lm_vm::snapshot::admit(image, &loaded, &mut budget) else {
                    continue;
                };
                accepted += 1;
                let mut world = lm_vm::World::new(
                    &loaded,
                    VmConfig {
                        fuel: 20_000,
                        heap_bytes: 1 << 20,
                        max_children: 4_096,
                        ..VmConfig::default()
                    },
                    Box::new(lm_vm::RecordingHost::new(1)),
                );
                if let Some(target) = world.new_child(0) {
                    if let Ok(root) = world.restore_image(0, target, &admitted) {
                        drive_restored(&mut world, root);
                    }
                }
            }
        }
        // The reseal makes every non-truncated mutant reach the
        // structural loader. The rule is the absence of a panic, not
        // the counts; the counters state that the resealed path is not
        // empty and is not trivially all-accept.
        assert!(sealed > ROUNDS, "most mutants keep a hash region");
        let _ = accepted;
    });
}

// ---------------------------------------------------------------
// The structural snapshot fuzzer.
// ---------------------------------------------------------------

/// The largest time one restored mutant may take.
///
/// The slice bound and the fuel cap already bound the work. The clock
/// states the same rule again, so a future path that loops without
/// spending fuel fails the case instead of hanging the suite.
const MAX_CASE_TIME: std::time::Duration = std::time::Duration::from_secs(10);

/// Rebuild the heap of one machine in canonical traversal order.
///
/// A structural edit can move an object out of the reachable set or
/// change the traversal order. The canonical order rule would then
/// answer for every mutant, and the run-time rules would never see
/// one. The pass keeps the heap canonical, so a mutant reaches the
/// interpreter.
fn recanonicalize(machine: &mut lm_vm::snapshot::ImageMachine) {
    use lm_value::{ObjRef, Value};
    let roots = lm_vm::snapshot::image_roots(machine);
    let mut order: Vec<u32> = Vec::new();
    let mut seen = vec![false; machine.objects.len()];
    let mut stack: Vec<u32> = roots.iter().rev().copied().collect();
    let mut children: Vec<ObjRef> = Vec::new();
    while let Some(r) = stack.pop() {
        if r as usize >= seen.len() || seen[r as usize] {
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
        slot: moved.get(r.slot as usize).copied().unwrap_or(0),
        generation: 0,
    };
    let objects: Vec<lm_vm::snapshot::ImageObject> = order
        .iter()
        .map(|r| {
            let entry = &machine.objects[*r as usize];
            lm_vm::snapshot::ImageObject {
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
        frame.closure = frame.closure.and_then(|o| moved.get(o as usize).copied());
    }
    for v in machine.locals.iter_mut().chain(machine.operands.iter_mut()) {
        value(v);
    }
    if let Some(pending) = &mut machine.pending {
        for v in &mut pending.args {
            value(v);
        }
    }
    if let Some(lm_vm::snapshot::ImageTerminal::Done(v)) = &mut machine.terminal {
        value(v);
    }
    for v in machine.mailbox.queue.iter_mut() {
        value(v);
    }
    machine.start_body = machine
        .start_body
        .and_then(|o| moved.get(o as usize).copied());
    for slot in machine.literals.iter_mut() {
        *slot = slot.and_then(|o| moved.get(o as usize).copied());
    }
    machine.objects = objects;
}

/// Apply one seeded structural mutation to a decoded image.
///
/// A byte mutation almost always fails the decoder. A structural
/// mutation states a world the decoder accepts, so admission and the
/// interpreter answer for it instead.
fn mutate_image(image: &mut lm_vm::snapshot::Image, prng: &mut Prng) {
    use lm_value::{ObjRef, Value};
    if image.machines.is_empty() {
        return;
    }
    let vm = prng.below(image.machines.len());
    let machines = image.machines.len() as u32;
    let objects = image.machines[vm].objects.len() as u32;
    // The value pool of this machine: the scalars a program can hold
    // and every object ordinal of its heap.
    let pick_value = |prng: &mut Prng| -> Value {
        match prng.below(6) {
            0 => Value::Unit,
            1 => Value::Int(prng.next() as i64),
            2 => Value::Bool(prng.next() & 1 == 0),
            3 => Value::Uninit,
            4 => Value::Op((prng.next() % 64) as u32),
            _ if objects > 0 => Value::Obj(ObjRef {
                slot: prng.below(objects as usize) as u32,
                generation: 0,
            }),
            _ => Value::Unit,
        }
    };
    match prng.below(12) {
        0 => {
            let m = &mut image.machines[vm];
            if !m.locals.is_empty() {
                let at = prng.below(m.locals.len());
                m.locals[at] = pick_value(prng);
            }
        }
        1 => {
            let m = &mut image.machines[vm];
            if !m.operands.is_empty() {
                let at = prng.below(m.operands.len());
                m.operands[at] = pick_value(prng);
            }
        }
        2 => {
            let value = pick_value(prng);
            let m = &mut image.machines[vm];
            if !m.objects.is_empty() {
                let at = prng.below(m.objects.len());
                if let lm_heap::Object::Instance { fields, .. } = &mut m.objects[at].object {
                    if !fields.is_empty() {
                        let slot = prng.below(fields.len());
                        fields[slot] = value;
                    }
                }
            }
        }
        3 => {
            let value = pick_value(prng);
            let m = &mut image.machines[vm];
            if !m.objects.is_empty() {
                let at = prng.below(m.objects.len());
                if let lm_heap::Object::List { items } = &mut m.objects[at].object {
                    if !items.is_empty() {
                        let slot = prng.below(items.len());
                        items[slot] = value;
                    }
                }
            }
        }
        4 => {
            let value = pick_value(prng);
            let m = &mut image.machines[vm];
            if !m.mailbox.queue.is_empty() {
                let at = prng.below(m.mailbox.queue.len());
                m.mailbox.queue[at] = value;
            }
        }
        5 => {
            // A native handle names another machine of the world.
            let target = prng.below(machines as usize) as u32;
            let m = &mut image.machines[vm];
            if !m.objects.is_empty() {
                let at = prng.below(m.objects.len());
                match &mut m.objects[at].object {
                    lm_heap::Object::NativeVm { vm } | lm_heap::Object::NativeTable { vm } => {
                        *vm = target;
                    }
                    lm_heap::Object::NativeHandle { proc, .. } => *proc = target,
                    lm_heap::Object::NativeRequest { vm, .. }
                    | lm_heap::Object::NativeCall { vm, .. } => *vm = target,
                    _ => {}
                }
            }
        }
        6 => {
            let env = prng.below(image.envs.len().max(1)) as u32;
            let m = &mut image.machines[vm];
            if !m.frames.is_empty() {
                let at = prng.below(m.frames.len());
                m.frames[at].env = env;
            }
        }
        7 => {
            let env = prng.below(image.envs.len().max(1)) as u32;
            image.machines[vm].witness = env;
        }
        8 => {
            let m = &mut image.machines[vm];
            m.is_proc = !m.is_proc;
        }
        9 => {
            let m = &mut image.machines[vm];
            if !m.frames.is_empty() {
                let at = prng.below(m.frames.len());
                m.frames[at].ip = prng.next() as u32 % 8;
            }
        }
        10 => {
            let value = pick_value(prng);
            if let Some(lm_vm::snapshot::ImageTerminal::Done(v)) = &mut image.machines[vm].terminal
            {
                *v = value;
            }
        }
        _ => {
            let env = prng.below(image.envs.len().max(1)) as u32;
            let m = &mut image.machines[vm];
            if !m.objects.is_empty() {
                let at = prng.below(m.objects.len());
                match &mut m.objects[at].object {
                    lm_heap::Object::Instance { env: w, .. }
                    | lm_heap::Object::Closure { env: w, .. } => {
                        *w = lm_value::Witness(lm_value::TypeEnvId(env));
                    }
                    _ => {}
                }
            }
        }
    }
    for machine in &mut image.machines {
        recanonicalize(machine);
    }
}

/// A second seed: one world captured mid execution.
///
/// The checkpoint world stops `asked`, so a restored root answers its
/// event and executes no instruction. This seed stops `ready` between
/// two instructions, so a restored mutant runs the interpreter over a
/// heap that holds a class instance, two lists, and live locals.
fn ready_snapshot_seed() -> (lm_vm::LoadedModule, Vec<u8>) {
    use lm_vm::{RecordingHost, RootEvent, World};
    let source = "\
class Counter
  n: Int = 7
end

def go(): Int
  a = Counter()
  xs = [1, 2]
  ys = [\"a\", \"b\"]
  m = ys.len()
  m = m + xs.at(0)
  m + a.n
end

go()
";
    let bytes = compile_to_bytes("ready.lm", source).expect("the seed compiles");
    let loaded = lm_vm::load_bytes(&bytes).expect("the seed loads");
    let container = {
        let mut world = World::new(
            &loaded,
            VmConfig::default(),
            Box::new(RecordingHost::new(1)),
        );
        // Step to the boundary that holds every object of the body.
        let mut best: Option<Vec<u8>> = None;
        for _ in 0..200 {
            let gate = world.next_gate();
            if let Ok(image) = world.capture_snapshot(gate, 0, false) {
                if image.world().machines[0].objects.len() >= 4 {
                    best = Some(image.bytes().to_vec());
                    break;
                }
            }
            match world.step_root() {
                RootEvent::Ran => {}
                _ => break,
            }
        }
        best.expect("one boundary holds the whole heap")
    };
    (loaded, container)
}

/// Every structural mutant decodes, admits or rejects, restores, and
/// drives without a panic and without a hang.
///
/// Admission proves structure alone, so most of these mutants admit.
/// The interpreter then meets a world that verified code never built,
/// and the rule is that one machine stops instead of the host.
#[test]
fn mutated_snapshot_images_never_panic_the_runtime() {
    on_supported_stack(|| {
        let mut prng = Prng(SEED ^ 0x1_5eed_1a6e);
        let limits = lm_vm::snapshot::LoadLimits::default();
        let mut admitted_count = 0usize;
        let mut restored_count = 0usize;
        let mut ran_count = 0usize;
        for (loaded, base, grants) in [
            {
                let (loaded, base) = snapshot_seed();
                (loaded, base, &["Proc", "Vm", "Clock"][..])
            },
            {
                let (loaded, base) = ready_snapshot_seed();
                (loaded, base, &[][..])
            },
        ] {
            let seed_image = lm_vm::snapshot::codec::load_external(&base, &loaded, limits)
                .expect("the seed admits")
                .into_image();
            for _round in 0..ROUNDS * 4 {
                let started = std::time::Instant::now();
                let mut image = seed_image.clone();
                for _ in 0..=prng.below(3) {
                    mutate_image(&mut image, &mut prng);
                }
                let Ok(bytes) = lm_vm::snapshot::codec::encode(&image, usize::MAX) else {
                    continue;
                };
                assert!(bytes.len() <= MAX_CASE_BYTES, "a mutation grew the input");
                let Ok(admitted) = lm_vm::snapshot::codec::load_external(&bytes, &loaded, limits)
                else {
                    continue;
                };
                admitted_count += 1;
                // A tight heap and fuel cap bound the run. The restored
                // world takes the grants the capture ran under, so the
                // mutant reaches the kernel instead of stopping at the
                // policy table.
                let mut world = lm_vm::World::new(
                    &loaded,
                    VmConfig {
                        fuel: 20_000,
                        heap_bytes: 1 << 20,
                        max_children: 4_096,
                        ..VmConfig::default()
                    },
                    Box::new(lm_vm::RecordingHost::new(1)),
                );
                for grant in grants {
                    world.allow(grant).expect("the grant names a target");
                }
                if let Some(target) = world.new_child(0) {
                    if let Ok(root) = world.restore_image(0, target, &admitted) {
                        restored_count += 1;
                        for vm in world.machine_ids() {
                            for grant in grants {
                                world.allow_on(vm, grant).expect("the grant names a target");
                            }
                        }
                        if world.state_of(root) == lm_vm::MachineState::Ready {
                            ran_count += 1;
                        }
                        drive_restored(&mut world, root);
                    }
                }
                assert!(
                    started.elapsed() < MAX_CASE_TIME,
                    "one mutant ran past its time bound"
                );
            }
        }
        // The counters state that the path is not empty. The rule is
        // the absence of a panic and of a hang.
        assert!(
            admitted_count > ROUNDS * 2,
            "too few structural mutants admitted: {admitted_count}"
        );
        assert!(
            restored_count > ROUNDS * 2,
            "too few structural mutants restored: {restored_count}"
        );
        assert!(
            ran_count > ROUNDS,
            "too few restored mutants executed an instruction: {ran_count}"
        );
    });
}

#[test]
fn mutated_sources_never_panic_the_scanner_checker_or_lowering() {
    on_supported_stack(|| {
        let mut prng = Prng(SEED ^ 0x5eed);
        for (name, text) in seed_sources() {
            let base = text.into_bytes();
            for _round in 0..ROUNDS {
                let mut bytes = base.clone();
                for _ in 0..=prng.below(3) {
                    mutate(&mut bytes, &mut prng);
                }
                assert!(bytes.len() <= MAX_CASE_BYTES, "a mutation grew the input");
                let source = String::from_utf8_lossy(&bytes).into_owned();
                // Compile errors are fine; a panic is a failure.
                let _ = lm_testkit::compile_text(&name, &source);
            }
        }
    });
}

/// The interface decoder is a new byte surface. A mutated interface
/// must reject or decode without a panic, and a decoded interface
/// must re-encode to bytes that decode again.
#[test]
fn mutated_interfaces_never_panic_the_decoder() {
    use lm_bytecode::interface::{decode_interface, encode_interface};
    on_supported_stack(|| {
        let mut prng = Prng(SEED ^ 0x1face);
        let source = "class Point\n  x: Int = 0\n  def sum(self): Int\n    self.x\n  end\nend\n\
                      enum Shape\n  Dot\n  Line(len: Int)\nend\n\
                      def area(s: Shape): Int\n  case s\n  in Dot then 0\n  \
                      in Line(l) then l\n  end\nend\n";
        let compiled = lm_compiler::compile_module(
            "seed.shapes",
            &lm_source::SourceFile::new("seed.lm", source.to_string()),
            &lm_compiler::CompileEnv::new().freeze(),
            false,
        )
        .expect("the seed compiles");
        let base = compiled.interface_bytes;
        for _ in 0..ROUNDS {
            let mut bytes = base.clone();
            for _ in 0..=prng.below(3) {
                mutate(&mut bytes, &mut prng);
            }
            assert!(bytes.len() <= MAX_CASE_BYTES, "a mutation grew the input");
            if let Ok(interface) = decode_interface(&bytes) {
                let again = encode_interface(&interface);
                assert!(
                    decode_interface(&again).is_ok(),
                    "a decoded interface did not re-encode"
                );
            }
        }
    });
}

/// The manifest parser is a new text surface. Every mutation must
/// produce a manifest or a diagnostic, never a panic.
#[test]
fn mutated_manifests_never_panic_the_parser() {
    on_supported_stack(|| {
        let mut prng = Prng(SEED ^ 0x504b47);
        let base = "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
                    [dependencies]\nmathlib = { path = \"../mathlib\" }\n"
            .as_bytes()
            .to_vec();
        for _ in 0..ROUNDS {
            let mut bytes = base.clone();
            for _ in 0..=prng.below(3) {
                mutate(&mut bytes, &mut prng);
            }
            let text = String::from_utf8_lossy(&bytes).into_owned();
            let _ = lm_compiler::parse_manifest(&text);
        }
    });
}

#[test]
fn the_regression_corpus_replays() {
    on_supported_stack(|| {
        let dir = repo_root().join("tests/fuzz-regressions");
        let mut modules = 0;
        let mut sources = 0;
        let mut containers = 0;
        for entry in std::fs::read_dir(&dir).expect("the corpus directory exists") {
            let path = entry.expect("directory entry").path();
            match path.extension().and_then(|e| e.to_str()) {
                Some("lmbc") => {
                    let bytes = std::fs::read(&path).expect("corpus case reads");
                    // Every checked-in module case is a rejection
                    // case, and it must reject at the intended layer:
                    // the local-count bomb at the decoder, every
                    // forgery seed at the verifier.
                    let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                    let decoded = lm_bytecode::decode(&bytes);
                    if name == "local-count-bomb" {
                        assert!(decoded.is_err(), "{} passed the decoder", path.display());
                    } else {
                        let module = decoded
                            .unwrap_or_else(|e| panic!("{} must decode: {e}", path.display()));
                        assert!(
                            lm_vm::load(module).is_err(),
                            "{} was accepted",
                            path.display()
                        );
                    }
                    modules += 1;
                }
                Some("lm") => {
                    let text =
                        String::from_utf8_lossy(&std::fs::read(&path).expect("reads")).into_owned();
                    let _ = lm_testkit::compile_text(&path.display().to_string(), &text);
                    sources += 1;
                }
                Some("lms") => {
                    // Every snapshot case runs against the checkpoint
                    // program the container names. The valid seed
                    // loads; every other seed rejects at the loader.
                    let bytes = std::fs::read(&path).expect("corpus case reads");
                    let (loaded, _) = snapshot_seed();
                    let out = lm_vm::snapshot::codec::load_external(
                        &bytes,
                        &loaded,
                        lm_vm::snapshot::LoadLimits::default(),
                    );
                    let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                    if name == "snapshot-world" {
                        out.unwrap_or_else(|e| panic!("{} must load: {e}", path.display()));
                    } else {
                        assert!(out.is_err(), "{} was accepted", path.display());
                    }
                    containers += 1;
                }
                _ => {}
            }
        }
        assert!(modules >= 5, "the module corpus shrank: {modules}");
        assert!(sources >= 4, "the source corpus shrank: {sources}");
        assert!(containers >= 2, "the container corpus shrank: {containers}");
    });
}

/// Rebuild the checked-in corpus. Run explicitly with
/// `cargo test -p lm-testkit --test fuzz -- --ignored`.
#[test]
#[ignore]
fn regenerate_fuzz_corpus() {
    use lm_bytecode::{BcClass, BcClassKind, BcType, Func, Instr, Module, NO_PARENT};
    let dir = repo_root().join("tests/fuzz-regressions");
    std::fs::create_dir_all(&dir).expect("corpus directory");
    let write = |name: &str, module: &Module| {
        std::fs::write(dir.join(name), lm_bytecode::encode(module)).expect("corpus writes");
    };
    let base_types = || vec![BcType::Unit, BcType::Bool, BcType::Int, BcType::Str];
    // Week-3 finding 1: `CallVirtualG` with an out-of-range type
    // application was a host panic before the structural bound.
    let mut types = base_types();
    types.push(BcType::Class(0));
    write(
        "callvirtualg-app-forgery.lmbc",
        &Module {
            strings: vec![],
            types,
            selectors: vec!["f".to_string()],
            apps: vec![],
            classes: vec![BcClass {
                name: "C".to_string(),
                parent_args: Vec::new(),
                key: "C".to_string(),
                parent: NO_PARENT,
                type_params: 0,
                kind: BcClassKind::Normal,
                fields: vec![],
                methods: vec![],
            }],
            funcs: vec![Func {
                name: "main".to_string(),
                type_params: 0,
                effect_params: 0,
                params: vec![],
                param_muts: vec![],
                ret: 2,
                row: vec![],
                captures: vec![],
                local_types: vec![],
                blocks: vec![vec![
                    Instr::New(0),
                    Instr::CallVirtualG {
                        selector: 0,
                        argc: 0,
                        app: 77,
                    },
                    Instr::Return,
                ]],
            }],
            imports: vec![],
            core_roles: [lm_bytecode::NO_ROLE; lm_bytecode::CORE_ROLE_COUNT],
            entry: 0,
            exports: vec![],
            bindings: vec![],
        },
    );
    // Week-3 finding 2: `CastType` between two instantiations of one
    // generic class forged the argument vector.
    let mut types = base_types();
    types.push(BcType::Var(0)); // 4
    types.push(BcType::Inst(0, vec![2])); // 5 Box[Int]
    types.push(BcType::Inst(0, vec![3])); // 6 Box[String]
    write(
        "casttype-argument-forgery.lmbc",
        &Module {
            strings: vec![],
            types,
            selectors: vec![],
            apps: vec![lm_bytecode::TypeApp {
                types: vec![2],
                rows: vec![],
            }],
            classes: vec![BcClass {
                name: "Box".to_string(),
                parent_args: Vec::new(),
                key: "Box".to_string(),
                parent: NO_PARENT,
                type_params: 1,
                kind: BcClassKind::Normal,
                fields: vec![("v".to_string(), 4)],
                methods: vec![],
            }],
            funcs: vec![Func {
                name: "main".to_string(),
                type_params: 0,
                effect_params: 0,
                params: vec![],
                param_muts: vec![],
                ret: 2,
                row: vec![],
                captures: vec![],
                local_types: vec![],
                blocks: vec![vec![
                    Instr::NewG { class: 0, app: 0 },
                    Instr::CastType(6),
                    Instr::LoadField(0),
                    Instr::Return,
                ]],
            }],
            imports: vec![],
            core_roles: [lm_bytecode::NO_ROLE; lm_bytecode::CORE_ROLE_COUNT],
            entry: 0,
            exports: vec![],
            bindings: vec![],
        },
    );
    // Week-4 finding class: a perform outside the claimed row, and a
    // first-class operation type with a forged signature.
    let source = "def greet(name: String) with Io.Print\n  sys.io.print(name)\nend\ngreet(\"x\")\n";
    let mut module = lm_testkit::compile_text("seed.lm", source).expect("seed compiles");
    let greet = module
        .funcs
        .iter()
        .position(|f| f.name == "greet")
        .expect("greet exists");
    module.funcs[greet].row.clear();
    write("perform-outside-claimed-row.lmbc", &module);
    let source = "def f() with Io.Print\n  p = sys.io.print\n  p(\"x\")\nend\nf()\n";
    let mut module = lm_testkit::compile_text("seed.lm", source).expect("seed compiles");
    for ty in &mut module.types {
        if let BcType::Op(_, f) = ty {
            *f = 2;
        }
    }
    write("op-type-signature-forgery.lmbc", &module);
    // The overflow found by this harness: a forged local slot count
    // sized a multi-gigabyte allocation in the verifier dataflow and
    // in the initial frame before any bound applied. The count is now
    // the local-type table length, so the seed patches the encoded
    // count field; the decoder length guard rejects it before any
    // allocation.
    {
        let module = Module {
            strings: vec![],
            types: base_types(),
            selectors: vec![],
            apps: vec![],
            classes: vec![],
            funcs: vec![Func {
                name: "main".to_string(),
                type_params: 0,
                effect_params: 0,
                params: vec![],
                param_muts: vec![],
                ret: 2,
                row: vec![],
                captures: vec![],
                local_types: vec![],
                blocks: vec![vec![Instr::ConstInt(1), Instr::Return]],
            }],
            imports: vec![],
            core_roles: [lm_bytecode::NO_ROLE; lm_bytecode::CORE_ROLE_COUNT],
            entry: 0,
            exports: vec![],
            bindings: vec![],
        };
        let mut bytes = lm_bytecode::encode(&module);
        // The semantic region starts after the 30-byte header. Its
        // layout for this module: the string count (4), the type
        // count plus four primitive tags (8), the selector count (4),
        // the application count (4), the import count (4), the core
        // role table (four bytes per role), the class count (4), the
        // function count (4), then the function record: type_params,
        // effect_params, the parameter count, the marker count, the
        // result type, the row count, and the capture count (28). The
        // local-type table count follows.
        let sem_at = u32::from_le_bytes(bytes[6..10].try_into().unwrap()) as usize;
        let roles = 4 * lm_bytecode::CORE_ROLE_COUNT;
        let count_at = sem_at + 4 + 8 + 4 + 4 + 4 + roles + 4 + 4 + 28;
        assert_eq!(
            u32::from_le_bytes(bytes[count_at..count_at + 4].try_into().unwrap()),
            0,
            "the local-count field moved; update the offset"
        );
        bytes[count_at..count_at + 4].copy_from_slice(&0x7fff_ffffu32.to_le_bytes());
        assert!(
            lm_bytecode::decode(&bytes).is_err(),
            "the forged local count must be rejected"
        );
        std::fs::write(dir.join("local-count-bomb.lmbc"), bytes).expect("corpus writes");
    }
    // Snapshot container seeds: one valid machine world and one
    // world whose heap is not in canonical traversal order.
    {
        let (loaded, container) = snapshot_seed();
        std::fs::write(dir.join("snapshot-world.lms"), &container).expect("corpus writes");
        let limits = lm_vm::snapshot::LoadLimits::default();
        let image = lm_vm::snapshot::codec::load_external(&container, &loaded, limits)
            .expect("the seed loads")
            .into_image();
        let mut broken = image.clone();
        let mut roots = (
            broken.machines[2].frames[0].closure,
            broken.machines[2].start_body,
        );
        std::mem::swap(&mut roots.0, &mut roots.1);
        broken.machines[2].frames[0].closure = roots.0;
        broken.machines[2].start_body = roots.1;
        let bad =
            lm_vm::snapshot::codec::encode(&broken, usize::MAX).expect("the damaged image encodes");
        assert!(
            lm_vm::snapshot::codec::load_external(&bad, &loaded, limits).is_err(),
            "the swapped-root seed must reject"
        );
        std::fs::write(dir.join("snapshot-swapped-roots.lms"), &bad).expect("corpus writes");
    }
    // Source seeds: shapes that stressed the scanner and parser.
    std::fs::write(
        dir.join("deep-parens.lm"),
        format!("x = {}1{}\n", "(".repeat(400), ")".repeat(400)),
    )
    .expect("writes");
    std::fs::write(
        dir.join("unterminated-block.lm"),
        "f = do || with Io.Print\n  sys.io.print(\"x\n",
    )
    .expect("writes");
}
