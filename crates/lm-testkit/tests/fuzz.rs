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

/// Process independent cases on four bounded worker stacks.
fn run_parallel_cases<T: Send>(cases: Vec<T>, exercise: impl Fn(T) + Sync) {
    const WORKERS: usize = 4;
    let cases = std::sync::Mutex::new(cases);
    std::thread::scope(|scope| {
        let mut workers = Vec::with_capacity(WORKERS);
        for _ in 0..WORKERS {
            let cases = &cases;
            let exercise = &exercise;
            workers.push(
                std::thread::Builder::new()
                    .stack_size(8 << 20)
                    .spawn_scoped(scope, move || loop {
                        let next = cases.lock().expect("the case queue is live").pop();
                        match next {
                            Some(case) => exercise(case),
                            None => break,
                        }
                    })
                    .expect("a case worker starts"),
            );
        }
        for worker in workers {
            worker.join().expect("a case worker does not panic");
        }
    });
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

/// Mutations for the larger installed-code snapshot seed.
const RICH_IMAGE_ROUNDS: usize = 24;

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
            let mut cases = Vec::with_capacity(ROUNDS);
            for round in 0..ROUNDS {
                let mut bytes = base.clone();
                // Apply one to three mutations.
                for _ in 0..=prng.below(3) {
                    mutate(&mut bytes, &mut prng);
                }
                // The generation order keeps each case reproducible.
                let _ = round;
                cases.push(bytes);
            }
            run_parallel_cases(cases, |bytes| exercise_module(&bytes));
        }
    });
}

/// A conformance can resolve a later conformance. The verifier must
/// validate the later conformance before this resolution starts.
#[test]
fn an_invalid_type_in_a_later_conformance_rejects_without_a_panic() {
    let source = r#"
final class NumbersIterator implements Iterator
  type Item = Int

  def next(mut self): Option[Int]
    None
  end
end

final class Numbers implements Iterable
  type Item = Int
  type Iter = NumbersIterator

  def iterator(self): NumbersIterator
    NumbersIterator()
  end
end

Numbers()
"#;
    let mut module = lm_testkit::compile_text("seed.lm", source).expect("the seed compiles");
    let iterable = module
        .interfaces
        .iter()
        .position(|item| item.key == "core.Iterable")
        .expect("Iterable exists") as u32;
    let iterator = module
        .interfaces
        .iter()
        .position(|item| item.key == "core.Iterator")
        .expect("Iterator exists") as u32;
    let numbers = module
        .classes
        .iter()
        .position(|item| item.name == "Numbers")
        .expect("Numbers exists") as u32;
    let numbers_iterator = module
        .classes
        .iter()
        .position(|item| item.name == "NumbersIterator")
        .expect("NumbersIterator exists") as u32;
    let iterable_at = module
        .conformances
        .iter()
        .position(|item| item.class == numbers && item.application.interface == iterable)
        .expect("the Iterable conformance exists");
    let iterator_at = module
        .conformances
        .iter()
        .position(|item| item.class == numbers_iterator && item.application.interface == iterator)
        .expect("the Iterator conformance exists");
    if iterable_at > iterator_at {
        module.conformances.swap(iterable_at, iterator_at);
    }
    let iterator_at = module
        .conformances
        .iter()
        .position(|item| item.class == numbers_iterator && item.application.interface == iterator)
        .expect("the Iterator conformance still exists");
    module.conformances[iterator_at].associated[0] = u32::MAX;

    let error = lm_verify::verify_module(&module).expect_err("the invalid type verifies");
    assert!(error.message.contains("associated binding is out of range"));
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
            .expect("the image encodes")
            .to_vec()
    };
    (loaded, container)
}

/// A snapshot seed with a terminal fault and retained code locations.
fn fault_snapshot_seed() -> (lm_vm::LoadedModule, Vec<u8>) {
    use lm_vm::{RecordingHost, RootEvent, World};
    let source = "def fail(value: Int): Int\n  10 / value\nend\nfail(0)\n";
    let bytes = compile_to_bytes("fault-seed.lm", source).expect("the seed compiles");
    let loaded = lm_vm::load_bytes(&bytes).expect("the seed loads");
    let container = {
        let mut world = World::new(
            &loaded,
            VmConfig::default(),
            Box::new(RecordingHost::new(1)),
        );
        assert!(matches!(world.run_machine(0), RootEvent::Fault(_)));
        assert!(!world
            .root_fault()
            .expect("the fault exists")
            .trace
            .is_empty());
        let gate = world.next_gate();
        world
            .capture_snapshot(gate, 0, false)
            .expect("the terminal machine captures")
            .bytes()
            .expect("the image encodes")
            .to_vec()
    };
    (loaded, container)
}

/// Drive one restored world to a stop under a bounded slice budget.
///
/// The budget keeps a mutant that would run forever from hanging the
/// harness. The rule is the absence of a panic; a fault, a block with
/// no runnable machine, or the budget all stop the drive cleanly.
fn drive_restored(world: &mut lm_vm::World, root: lm_vm::VmId) {
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
    match prng.below(21) {
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
                if let lm_heap::Object::List { items, .. } = &mut m.objects[at].object {
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
            let image_target = prng.below(image.vm_images.len().max(1)) as u32;
            let m = &mut image.machines[vm];
            if !m.objects.is_empty() {
                let at = prng.below(m.objects.len());
                match &mut m.objects[at].object {
                    lm_heap::Object::NativeVm { image, .. } => *image = image_target,
                    lm_heap::Object::NativeTable { vm } => *vm = target,
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
        11 => {
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
        12 => {
            if prng.next() & 1 == 0 {
                image.distinguished = Some(prng.below(image.machines.len() + 1) as u32);
            } else {
                image.full_vm = Some(prng.below(image.vm_images.len() + 1) as u32);
            }
        }
        13 => {
            if !image.installations.is_empty() {
                let installation = prng.below(image.installations.len());
                if !image.installations[installation].is_empty() {
                    let at = prng.below(image.installations[installation].len());
                    image.installations[installation][at] ^= prng.next() as u8;
                }
            }
        }
        14 => {
            if !image.vm_images.is_empty() {
                let vm_image = prng.below(image.vm_images.len());
                let instances = &mut image.vm_images[vm_image].instances;
                if !instances.is_empty() {
                    let instance = prng.below(instances.len());
                    instances[instance].installation = prng.next() as u32;
                }
            }
        }
        15 => {
            if !image.vm_images.is_empty() {
                let vm_image = prng.below(image.vm_images.len());
                let instances = &mut image.vm_images[vm_image].instances;
                if !instances.is_empty() {
                    let instance = prng.below(instances.len());
                    if prng.next() & 1 == 0 {
                        instances[instance].entry = prng.next() as u32;
                    } else {
                        let at = prng.below(instances[instance].semantic_hash.len());
                        instances[instance].semantic_hash[at] ^= prng.next() as u8;
                    }
                }
            }
        }
        16 => {
            if !image.vm_images.is_empty() {
                let vm_image = prng.below(image.vm_images.len());
                let instances = &mut image.vm_images[vm_image].instances;
                if !instances.is_empty() {
                    let at = prng.below(instances.len());
                    let instance = &mut instances[at];
                    let map = match prng.below(3) {
                        0 => &mut instance.funcs,
                        1 => &mut instance.classes,
                        _ => &mut instance.slots,
                    };
                    if !map.is_empty() {
                        let at = prng.below(map.len());
                        map[at] = prng.next() as u32;
                    }
                }
            }
        }
        17 => {
            if !image.vm_images.is_empty() {
                let vm_image = prng.below(image.vm_images.len());
                let slots = &mut image.vm_images[vm_image].slots;
                if !slots.is_empty() {
                    let at = prng.below(slots.len());
                    slots[at] = match prng.below(4) {
                        0 => lm_vm::snapshot::ImageSlotTarget::Function(prng.next() as u32),
                        1 => lm_vm::snapshot::ImageSlotTarget::Class {
                            class: prng.next() as u32,
                            constructor: prng.next() as u32,
                        },
                        2 => {
                            lm_vm::snapshot::ImageSlotTarget::Value(Value::Int(prng.next() as i64))
                        }
                        _ => lm_vm::snapshot::ImageSlotTarget::Empty,
                    };
                }
            }
        }
        18 => {
            if !image.vm_images.is_empty() {
                let vm_image = prng.below(image.vm_images.len());
                let slots = &mut image.vm_images[vm_image].slots;
                if !slots.is_empty() {
                    let at = prng.below(slots.len());
                    slots[at] = lm_vm::snapshot::ImageSlotTarget::Process {
                        proc: prng.below(machines as usize + 1) as u32,
                        generation: prng.next() as u32,
                    };
                }
            }
        }
        19 => {
            let m = &mut image.machines[vm];
            if !m.objects.is_empty() {
                let at = prng.below(m.objects.len());
                if let lm_heap::Object::NativeCodeHandle {
                    image,
                    generation,
                    instance,
                    kind,
                    index,
                } = &mut m.objects[at].object
                {
                    match prng.below(5) {
                        0 => *image = prng.next() as u32,
                        1 => *generation = prng.next() as u32,
                        2 => *instance = prng.next() as u32,
                        3 => *index = prng.next() as u32,
                        _ => {
                            *kind = if prng.next() & 1 == 0 {
                                lm_heap::CodeHandleKind::FunctionBinding
                            } else {
                                lm_heap::CodeHandleKind::ClassBinding
                            }
                        }
                    }
                }
            }
        }
        _ => {
            let mut changed = false;
            if let Some(lm_vm::snapshot::ImageTerminal::Fault(record)) =
                &mut image.machines[vm].terminal
            {
                let trace_len = record.trace.len();
                if let Some(site) = record.trace.get_mut(prng.below(trace_len)) {
                    match prng.below(3) {
                        0 => site.function = prng.next() as u32,
                        1 => site.block = prng.next() as u32,
                        _ => site.instruction = prng.next() as u32,
                    }
                    changed = true;
                }
            }
            if !changed && !image.vm_images.is_empty() {
                let vm_image = prng.below(image.vm_images.len());
                let instances = &mut image.vm_images[vm_image].instances;
                if !instances.is_empty() {
                    let at = prng.below(instances.len());
                    let instance = &mut instances[at];
                    if let Some(interface) = &mut instance.interface {
                        if !interface.is_empty() {
                            let at = prng.below(interface.len());
                            interface[at] ^= prng.next() as u8;
                        }
                    }
                }
            }
        }
    }
    for machine in &mut image.machines {
        recanonicalize(machine);
    }
}

/// Build one artifact with every slot target kind.
fn installed_slot_artifact() -> Vec<u8> {
    use lm_compiler::{compile_module_with_options, CompileEnv, CompileOptions};
    let source = lm_source::SourceFile::new(
        "fuzz-slots.lm",
        "final class Box\nend\ndef step(value: Int): Int\n  value + 1\nend\n0\n",
    );
    let compiled = compile_module_with_options(
        "fuzz-slots",
        &source,
        &CompileEnv::new().freeze(),
        true,
        &CompileOptions::new()
            .late_function("step")
            .late_class("Box"),
    )
    .expect("the installed fuzz artifact compiles");
    let mut module = compiled.module;
    let step = module
        .exports
        .iter()
        .find(|export| export.name == "step" && export.kind == lm_bytecode::ExportKind::Function)
        .expect("the function is exported")
        .def;
    let class = module
        .exports
        .iter()
        .find(|export| export.name == "Box" && export.kind == lm_bytecode::ExportKind::Class)
        .expect("the class is exported");
    assert!(module
        .slots
        .iter()
        .any(|slot| slot.initial == Some(lm_bytecode::SlotTarget::Function(step))));
    assert!(module.slots.iter().any(|slot| slot.initial
        == Some(lm_bytecode::SlotTarget::Class {
            class: class.def,
            constructor: class.ctor,
        })));
    let int = module
        .types
        .iter()
        .position(|ty| *ty == lm_bytecode::BcType::Int)
        .expect("the Int type exists") as u32;
    module.slots.push(lm_bytecode::SlotSpec {
        key: lm_bytecode::ad_hoc_slot_key("fuzz-slots.value"),
        contract_hash: [0; 32],
        contract: lm_bytecode::SlotContract::Value { ty: int },
        initial: None,
    });
    module.slots.push(lm_bytecode::SlotSpec {
        key: lm_bytecode::ad_hoc_slot_key("fuzz-slots.process"),
        contract_hash: [0; 32],
        contract: lm_bytecode::SlotContract::Process {
            message: int,
            result: int,
        },
        initial: None,
    });
    lm_verify::verify_module(&module).expect("the installed fuzz artifact verifies");
    lm_bytecode::encode(&module)
}

/// A second seed with installed code and every live slot target.
///
/// The root also keeps ordinary collections and live locals. Admitted
/// mutants can therefore reach both restored code state and execution.
fn ready_snapshot_seed() -> (lm_vm::LoadedModule, Vec<u8>) {
    use lm_vm::{RecordingHost, RootEvent, World};
    let artifact = installed_slot_artifact();
    let source = r#"
class Worker < Proc[Int]
  def on_spawn(self): Int with Proc
    7
  end
end

def artifact_bytes(): Bytes with Fs.Open, Fs.Read, Fs.Close
  case sys.fs.open("fuzz-slots.lmbc", ReadOnly)
  in Ok(file)
    value = case file.read(1048576)
    in Ok(bytes) then bytes
    in Err(_) then Bytes()
    end
    file.close()
    value
  in Err(_) then Bytes()
  end
end

def go(): Int with Fs.Open, Fs.Read, Fs.Close, Vm, Proc, Compiler.Verify
  image = sys.vm.Vm()
  module = case sys.vm.artifact(artifact_bytes()).verify()
  in Ok(value) then value
  in Err(_)
    return -1
  end
  instance = case image.install(module)
  in Ok(value) then value
  in Err(_)
    return -2
  end
  function_binding = case instance.function_binding[(Int,), Int]("step")
  in Ok(value) then value
  in Err(_)
    return -3
  end
  class_binding = case instance.class_binding("Box")
  in Ok(value) then value
  in Err(_)
    return -4
  end
  function = case instance.function[(Int,), Int]("step")
  in Ok(value) then value
  in Err(_)
    return -5
  end
  class_def = case instance.class_def("Box")
  in Ok(value) then value
  in Err(_)
    return -6
  end
  function_spec = case instance.slot_spec("step")
  in Ok(value) then value
  in Err(_)
    return -5
  end
  function_slot = case instance.slot_for(function_spec)
  in Ok(value) then value
  in Err(_)
    return -5
  end
  class_spec = case instance.slot_spec("Box")
  in Ok(value) then value
  in Err(_)
    return -6
  end
  class_slot = case instance.slot_for(class_spec)
  in Ok(value) then value
  in Err(_)
    return -6
  end
  value_spec = case instance.slot_spec("fuzz-slots.value")
  in Ok(value) then value
  in Err(_)
    return -7
  end
  value_slot = case instance.slot_for(value_spec)
  in Ok(value) then value
  in Err(_)
    return -7
  end
  process_spec = case instance.slot_spec("fuzz-slots.process")
  in Ok(value) then value
  in Err(_)
    return -8
  end
  process_slot = case instance.slot_for(process_spec)
  in Ok(value) then value
  in Err(_)
    return -8
  end
  image.replace_function(function_slot, function)
  image.replace_class(class_slot, class_def)
  image.replace_value(value_slot, 41)
  image.replace_process(process_slot, Worker.spawn())
  pending = case image.change(function_binding, function_binding)
  in Ok(value) then value
  in Err(_)
    return -9
  end
  changes = List[SlotChange]()
  changes.push(pending)
  xs = [1, 2]
  ys = ["a", "b"]
  total = 0
  for _ in Range(0, 100)
    total = total + xs.at(0) + ys.len()
  end
  function_binding.target()
  class_binding.target()
  total + changes.len()
end

go()
"#;
    let bytes = compile_to_bytes("ready.lm", source).expect("the seed compiles");
    let loaded = lm_vm::load_bytes(&bytes).expect("the seed loads");
    let container = {
        let host = std::rc::Rc::new(std::cell::RefCell::new(RecordingHost::new(1)));
        host.borrow_mut()
            .set_file("fuzz-slots.lmbc", artifact.clone());
        let mut world = World::new(&loaded, VmConfig::default(), Box::new(host));
        for grant in ["Fs", "Vm", "Proc", "Compiler.Verify"] {
            world.allow(grant).expect("the grant names a target");
        }
        // Step to one boundary after every slot target is installed.
        let mut best: Option<Vec<u8>> = None;
        for _ in 0..2_000 {
            let gate = world.next_gate();
            if let Ok(image) = world.capture_snapshot(gate, 0, false) {
                let rich = image.world().vm_images.iter().any(|vm| {
                    !vm.instances.is_empty()
                        && vm.slots.iter().any(|slot| {
                            matches!(
                                slot,
                                lm_vm::snapshot::ImageSlotTarget::Value(lm_value::Value::Int(41))
                            )
                        })
                        && vm.slots.iter().any(|slot| {
                            matches!(slot, lm_vm::snapshot::ImageSlotTarget::Process { .. })
                        })
                });
                let bindings = image.world().machines[0]
                    .objects
                    .iter()
                    .filter_map(|entry| match &entry.object {
                        lm_heap::Object::NativeCodeHandle { kind, .. } => Some(*kind),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                let has_change = image.world().machines[0]
                    .objects
                    .iter()
                    .any(|entry| matches!(&entry.object, lm_heap::Object::NativeSlotChange { .. }));
                let has_version = image
                    .world()
                    .vm_images
                    .iter()
                    .any(|vm| vm.slot_versions.iter().any(|version| *version > 0));
                if rich
                    && bindings.contains(&lm_heap::CodeHandleKind::FunctionBinding)
                    && bindings.contains(&lm_heap::CodeHandleKind::ClassBinding)
                    && has_change
                    && has_version
                {
                    best = Some(image.bytes().expect("the image encodes").to_vec());
                    break;
                }
            }
            match world.step_root() {
                RootEvent::Ran => {}
                RootEvent::Waiting | RootEvent::Blocked => {
                    world.poll_blocked();
                }
                _ => break,
            }
        }
        best.expect("one boundary holds installed code and every slot target")
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
        for (loaded, base, grants, rounds) in [
            {
                let (loaded, base) = snapshot_seed();
                (loaded, base, &["Proc", "Vm", "Clock"][..], ROUNDS * 4)
            },
            {
                let (loaded, base) = ready_snapshot_seed();
                (loaded, base, &["Fs", "Vm", "Proc"][..], RICH_IMAGE_ROUNDS)
            },
            {
                let (loaded, base) = fault_snapshot_seed();
                (loaded, base, &[][..], RICH_IMAGE_ROUNDS)
            },
        ] {
            let seed_image = lm_vm::snapshot::codec::load_external(&base, &loaded, limits)
                .expect("the seed admits")
                .into_image();
            for _round in 0..rounds {
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
            ran_count > ROUNDS / 40,
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
            let mut cases = Vec::with_capacity(ROUNDS);
            for _round in 0..ROUNDS {
                let mut bytes = base.clone();
                for _ in 0..=prng.below(3) {
                    mutate(&mut bytes, &mut prng);
                }
                assert!(bytes.len() <= MAX_CASE_BYTES, "a mutation grew the input");
                let source = String::from_utf8_lossy(&bytes).into_owned();
                cases.push((name.clone(), source));
            }
            run_parallel_cases(cases, |(name, source)| {
                // Compile errors are fine; a panic is a failure.
                let _ = lm_testkit::compile_text(&name, &source);
            });
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
            interfaces: vec![],
            conformances: vec![],
            class_bounds: vec![vec![]],
            func_bounds: vec![vec![]],
            classes: vec![BcClass {
                name: "C".to_string(),
                parent_args: Vec::new(),
                key: "C".to_string(),
                is_final: false,
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
            slots: vec![],
            core_roles: [lm_bytecode::NO_ROLE; lm_bytecode::CORE_ROLE_COUNT],
            entry: 0,
            exports: vec![],
            bindings: vec![],
            debug: Vec::new(),
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
            interfaces: vec![],
            conformances: vec![],
            class_bounds: vec![vec![]],
            func_bounds: vec![vec![]],
            classes: vec![BcClass {
                name: "Box".to_string(),
                parent_args: Vec::new(),
                key: "Box".to_string(),
                is_final: false,
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
            slots: vec![],
            core_roles: [lm_bytecode::NO_ROLE; lm_bytecode::CORE_ROLE_COUNT],
            entry: 0,
            exports: vec![],
            bindings: vec![],
            debug: Vec::new(),
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
            interfaces: vec![],
            conformances: vec![],
            class_bounds: vec![],
            func_bounds: vec![vec![]],
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
            slots: vec![],
            core_roles: [lm_bytecode::NO_ROLE; lm_bytecode::CORE_ROLE_COUNT],
            entry: 0,
            exports: vec![],
            bindings: vec![],
            debug: Vec::new(),
        };
        let mut bytes = lm_bytecode::encode(&module);
        // The semantic region starts after the 62-byte header. Its
        // layout for this module: the string count (4), the type
        // count plus four primitive tags (8), the selector count (4),
        // the application count (4), four empty declaration tables
        // (16), one empty function-bound row (4), the import and slot
        // counts (8), the core role table, the class and function
        // counts (8), then seven function fields (28). The local-type
        // table count follows.
        let sem_at = u32::from_le_bytes(bytes[38..42].try_into().unwrap()) as usize;
        let roles = 4 * lm_bytecode::CORE_ROLE_COUNT;
        let count_at = sem_at + 4 + 8 + 4 + 4 + 16 + 4 + 8 + roles + 4 + 4 + 28;
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
        let closure = broken.machines[2].frames[0].closure;
        let frame = match closure {
            Some(lm_value::Value::Obj(reference)) => Some(reference.slot),
            _ => panic!("the frame has an object capture context"),
        };
        let mut roots = (frame, broken.machines[2].start_body);
        std::mem::swap(&mut roots.0, &mut roots.1);
        broken.machines[2].frames[0].closure = roots.0.map(|slot| {
            lm_value::Value::Obj(lm_value::ObjRef {
                slot,
                generation: 0,
            })
        });
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
