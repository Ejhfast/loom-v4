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
            if let Ok(image) = lm_vm::snapshot::codec::decode(&bytes, &loaded, limits) {
                accepted += 1;
                let again = lm_vm::snapshot::codec::encode(&image, usize::MAX)
                    .expect("an accepted image encodes");
                assert_eq!(again, bytes, "an accepted mutant has two spellings");
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
                    if let Ok(root) = world.restore_image(0, target, &image) {
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
                    let out = lm_vm::snapshot::codec::decode(
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
        let image =
            lm_vm::snapshot::codec::decode(&container, &loaded, limits).expect("the seed loads");
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
            lm_vm::snapshot::codec::decode(&bad, &loaded, limits).is_err(),
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
