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
    ] {
        for path in lm_files(&repo_root().join(dir)) {
            let text = std::fs::read_to_string(&path).expect("example reads");
            out.push((path.display().to_string(), text));
        }
    }
    assert!(out.len() >= 9, "the example corpus shrank");
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

#[test]
fn the_regression_corpus_replays() {
    on_supported_stack(|| {
        let dir = repo_root().join("tests/fuzz-regressions");
        let mut modules = 0;
        let mut sources = 0;
        for entry in std::fs::read_dir(&dir).expect("the corpus directory exists") {
            let path = entry.expect("directory entry").path();
            match path.extension().and_then(|e| e.to_str()) {
                Some("lmbc") => {
                    let bytes = std::fs::read(&path).expect("corpus case reads");
                    // Every checked-in module case is a rejection case:
                    // it must fail decode or verify, without a panic.
                    let accepted = lm_bytecode::decode(&bytes)
                        .ok()
                        .and_then(|m| lm_vm::load(m).ok())
                        .is_some();
                    assert!(!accepted, "{} was accepted", path.display());
                    modules += 1;
                }
                Some("lm") => {
                    let text =
                        String::from_utf8_lossy(&std::fs::read(&path).expect("reads")).into_owned();
                    let _ = lm_testkit::compile_text(&path.display().to_string(), &text);
                    sources += 1;
                }
                _ => {}
            }
        }
        assert!(modules >= 5, "the module corpus shrank: {modules}");
        assert!(sources >= 2, "the source corpus shrank: {sources}");
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
            entry: 0,
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
            entry: 0,
        },
    );
    // Week-4 finding class: a perform outside the claimed row, and a
    // first-class operation type with a forged signature.
    let source = "def greet(name: String) with Io.Print\n  sys.io.Print(name)\nend\ngreet(\"x\")\n";
    let mut module = lm_testkit::compile_text("seed.lm", source).expect("seed compiles");
    let greet = module
        .funcs
        .iter()
        .position(|f| f.name == "greet")
        .expect("greet exists");
    module.funcs[greet].row.clear();
    write("perform-outside-claimed-row.lmbc", &module);
    let source = "def f() with Io.Print\n  p = sys.io.Print\n  p(\"x\")\nend\nf()\n";
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
            entry: 0,
        };
        let mut bytes = lm_bytecode::encode(&module);
        let pos = bytes
            .windows(4)
            .position(|w| w == b"main")
            .expect("the function name is in the encoding");
        // After the name: type_params, effect_params, the parameter
        // count, the result type, the row count, and the capture
        // count. The local-type table count follows.
        let count_at = pos + 4 + 4 * 6;
        bytes[count_at..count_at + 4].copy_from_slice(&0x7fff_ffffu32.to_le_bytes());
        assert!(
            lm_bytecode::decode(&bytes).is_err(),
            "the forged local count must be rejected"
        );
        std::fs::write(dir.join("local-count-bomb.lmbc"), bytes).expect("corpus writes");
    }
    // Source seeds: shapes that stressed the scanner and parser.
    std::fs::write(
        dir.join("deep-parens.lm"),
        format!("x = {}1{}\n", "(".repeat(400), ")".repeat(400)),
    )
    .expect("writes");
    std::fs::write(
        dir.join("unterminated-block.lm"),
        "f = do || with Io.Print\n  sys.io.Print(\"x\n",
    )
    .expect("writes");
}
