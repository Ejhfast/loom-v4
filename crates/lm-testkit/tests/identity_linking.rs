//! Identity and linking: the four identities, the merge table, the
//! order-invariant member labeling, and the symmetric members.
//!
//! The suite follows `docs/specs/identity-and-linking.md` sections 4
//! to 7 and specification 3.6 and 3.7.

use lm_bytecode::identity::module_identity;
use lm_bytecode::{BcType, DecodeError, Func, Instr, Module};
use lm_compiler::{compile_module, link, CompileEnv, LinkEnv, LinkUnit};
use lm_source::SourceFile;
use lm_testkit::compile_text;

fn identity_of(source: &str) -> (Module, lm_bytecode::identity::ModuleIdentity) {
    let module = compile_text("t.lm", source).expect("compiles");
    let identity = module_identity(&module).expect("hashes");
    (module, identity)
}

fn func_hash(
    module: &Module,
    identity: &lm_bytecode::identity::ModuleIdentity,
    name: &str,
) -> [u8; 32] {
    let idx = module
        .funcs
        .iter()
        .position(|f| f.name == name)
        .unwrap_or_else(|| panic!("no function `{name}`"));
    identity.func_hashes[idx]
}

fn class_hash(
    module: &Module,
    identity: &lm_bytecode::identity::ModuleIdentity,
    name: &str,
) -> [u8; 32] {
    let idx = module
        .classes
        .iter()
        .position(|c| c.name == name)
        .unwrap_or_else(|| panic!("no class `{name}`"));
    identity.class_hashes[idx]
}

// ---------------------------------------------------------------
// Section 4: the referenced nominal identity.
// ---------------------------------------------------------------

/// Two structurally equal classes receive one structural hash. Two
/// functions that name one of them each must not. A type digest names
/// a class by qualified key, so the signatures stay apart.
#[test]
fn a_referenced_qualified_key_separates_two_equal_signatures() {
    let source = "class Vec2\n  x: Int = 0\n  y: Int = 0\nend\n\
                  class Point\n  x: Int = 0\n  y: Int = 0\nend\n\
                  def f(v: Vec2): Int\n  v.x\nend\n\
                  def g(p: Point): Int\n  p.x\nend\n\
                  f(Vec2()) + g(Point())\n";
    let (module, identity) = identity_of(source);
    assert_eq!(
        class_hash(&module, &identity, "Vec2"),
        class_hash(&module, &identity, "Point"),
        "the two classes are structurally equal, so the hashes must agree"
    );
    assert_ne!(
        func_hash(&module, &identity, "f"),
        func_hash(&module, &identity, "g"),
        "two signatures that name two nominal classes share a hash"
    );
}

/// The qualified key of a class follows its module path. The core
/// image keeps the reserved path `core` in every module.
#[test]
fn a_qualified_key_carries_the_module_path() {
    let module = compile_one("app.shapes", "class Dot\n  x: Int = 0\nend\n", &[], false).module;
    let key_of = |name: &str| {
        module
            .classes
            .iter()
            .find(|c| c.name == name)
            .map(|c| c.key.clone())
            .unwrap_or_else(|| panic!("no class `{name}`"))
    };
    assert_eq!(key_of("Dot"), "app.shapes.Dot");
    assert_eq!(key_of("Option"), "core.Option");
    assert_eq!(key_of("Option.Some"), "core.Option.Some");
}

// ---------------------------------------------------------------
// Section 5: the linker merge table.
// ---------------------------------------------------------------

/// Compile one module against the interfaces already seen.
fn compile_one(
    path: &str,
    text: &str,
    seen: &[lm_bytecode::interface::Interface],
    is_main: bool,
) -> lm_compiler::CompiledModule {
    let mut env = CompileEnv::new();
    for interface in seen {
        env.bind_interface(interface.clone()).expect("binds");
    }
    env.bind_root("shapes", "app.shapes").expect("binds");
    env.bind_root("main", "app.main").expect("binds");
    let source = SourceFile::new("t.lm", text.to_string());
    compile_module(path, &source, &env.freeze(), is_main).expect("the module compiles")
}

/// A two-module program: `app.shapes` defines `Dot`, and `app.main`
/// defines a structurally equal `Spot` and uses both.
fn two_module_program() -> Vec<lm_compiler::CompiledModule> {
    let shapes = compile_one(
        "app.shapes",
        "class Dot\n  x: Int = 0\nend\ndef make(): Dot\n  Dot()\nend\n",
        &[],
        false,
    );
    let main = compile_one(
        "app.main",
        "use shapes\nclass Spot\n  x: Int = 0\nend\n\
         d = shapes.make()\ns = Spot()\nd.x + s.x\n",
        std::slice::from_ref(&shapes.interface),
        true,
    );
    vec![shapes, main]
}

fn link_units(units: &[lm_compiler::CompiledModule]) -> Result<lm_compiler::LinkedProgram, String> {
    let mut env = LinkEnv::new();
    for unit in units {
        env.bind(LinkUnit {
            path: unit.path.clone(),
            module: unit.module.clone(),
            interface: unit.interface.clone(),
        })
        .expect("binds");
    }
    link("app.main", &env.freeze()).map_err(|e| e.0)
}

/// Row 1: one qualified key with one structural hash merges. Every
/// module embeds the core, and the merged program holds one copy of
/// each core class.
#[test]
fn every_embedded_core_copy_merges() {
    let program = link_units(&two_module_program()).expect("links");
    let mut keys: Vec<&str> = program
        .module
        .classes
        .iter()
        .map(|c| c.key.as_str())
        .collect();
    let total = keys.len();
    keys.sort_unstable();
    keys.dedup();
    assert_eq!(total, keys.len(), "the merged program holds a key twice");
    assert!(
        keys.contains(&"core.Option"),
        "the merged program lost the core"
    );
}

/// Row 3: two classes with equal structures and different qualified
/// keys stay distinct definitions in the merged program.
#[test]
fn two_equal_shapes_with_two_keys_stay_distinct() {
    let units = two_module_program();
    // The two classes really are structurally equal.
    let dot = {
        let id = module_identity(&units[0].module).expect("hashes");
        let idx = units[0]
            .module
            .classes
            .iter()
            .position(|c| c.key == "app.shapes.Dot")
            .expect("the class exists");
        id.class_hashes[idx]
    };
    let spot = {
        let id = module_identity(&units[1].module).expect("hashes");
        let idx = units[1]
            .module
            .classes
            .iter()
            .position(|c| c.key == "app.main.Spot")
            .expect("the class exists");
        id.class_hashes[idx]
    };
    assert_eq!(dot, spot, "the probe needs two structurally equal classes");
    let program = link_units(&units).expect("links");
    let count = |key: &str| {
        program
            .module
            .classes
            .iter()
            .filter(|c| c.key == key)
            .count()
    };
    assert_eq!(count("app.shapes.Dot"), 1);
    assert_eq!(count("app.main.Spot"), 1);
}

/// Row 2: one qualified key with two structural hashes rejects. The
/// message names both providers and the rebuild.
///
/// This is the split-core defect. One module carries an edited copy of
/// a core class, so `core.Option.Some` arrives with two shapes.
#[test]
fn two_versions_of_one_qualified_key_reject() {
    let mut units = two_module_program();
    let some = units[1]
        .module
        .classes
        .iter()
        .position(|c| c.key == "core.Option.Some")
        .expect("the core arm is embedded");
    units[1].module.classes[some].fields[0].0 = "w".to_string();
    let error = link_units(&units).expect_err("two versions of one key must reject");
    assert!(error.contains("core.Option.Some"), "{error}");
    assert!(error.contains("two implementations"), "{error}");
    assert!(error.contains("app.shapes"), "{error}");
    assert!(error.contains("app.main"), "{error}");
    assert!(error.contains("rebuild"), "{error}");
}

/// A method body edit keeps the qualified key and moves the structural
/// hash of the class that answers the selector.
#[test]
fn a_method_body_edit_keeps_the_key_and_moves_the_hash() {
    let before = "class Counter\n  value: Int = 0\n  def get(self): Int\n    self.value\n  end\n\
                  end\nCounter().get()\n";
    let after = "class Counter\n  value: Int = 0\n  def get(self): Int\n    self.value + 0\n  \
                 end\nend\nCounter().get()\n";
    let (ma, ia) = identity_of(before);
    let (mb, ib) = identity_of(after);
    let key = |m: &Module| {
        m.classes
            .iter()
            .find(|c| c.name == "Counter")
            .map(|c| c.key.clone())
            .expect("the class exists")
    };
    assert_eq!(key(&ma), key(&mb), "the key must not move");
    assert_ne!(
        class_hash(&ma, &ia, "Counter"),
        class_hash(&mb, &ib, "Counter"),
        "the structural hash must move"
    );
}

// ---------------------------------------------------------------
// Sections 6 and 7: order-invariant labeling and symmetric members.
// ---------------------------------------------------------------

const CYCLE: &str = "def even(n: Int): Bool\n\
                     \x20 if n == 0\n    true\n  else\n    odd(n - 1)\n  end\nend\n\
                     def odd(n: Int): Bool\n\
                     \x20 if n == 0\n    false\n  else\n    even(n - 1)\n  end\nend\n\
                     even(4)\n";

/// A rename inside a cyclic component moves no structural hash, in
/// any direction of the old name order. The week-5 rule that sorted
/// members by name is gone.
#[test]
fn a_rename_inside_a_cycle_moves_no_hash() {
    let module = compile_text("t.lm", CYCLE).expect("compiles");
    let identity = module_identity(&module).expect("hashes");
    for to in ["evenx", "aaa", "zzz"] {
        let mut twin = module.clone();
        let idx = twin
            .funcs
            .iter()
            .position(|f| f.name == "even")
            .expect("the function exists");
        twin.funcs[idx].name = to.to_string();
        let twin_identity = module_identity(&twin).expect("hashes");
        assert_eq!(
            identity.func_hashes, twin_identity.func_hashes,
            "the rename `even` to `{to}` moved a structural hash"
        );
        assert_eq!(
            identity.class_hashes, twin_identity.class_hashes,
            "the rename `even` to `{to}` moved a class hash"
        );
    }
}

/// The two members of an asymmetric cycle still receive distinct
/// hashes: refinement separates them by the bodies they reach.
#[test]
fn an_asymmetric_cycle_separates_its_members() {
    let (module, identity) = identity_of(CYCLE);
    assert_ne!(
        func_hash(&module, &identity, "even"),
        func_hash(&module, &identity, "odd")
    );
    assert_eq!(
        identity.max_refine_rounds, 0,
        "the first labels already separate two different bodies"
    );
}

/// Section 7: two mutually recursive definitions with equal bodies are
/// symmetric through every round. No order-invariant rule separates
/// them, so they share one structural hash.
#[test]
fn symmetric_members_share_one_structural_hash() {
    let source = "def ping(n: Int): Int\n\
                  \x20 if n == 0\n    0\n  else\n    pong(n - 1)\n  end\nend\n\
                  def pong(n: Int): Int\n\
                  \x20 if n == 0\n    0\n  else\n    ping(n - 1)\n  end\nend\n\
                  ping(4)\n";
    let (module, identity) = identity_of(source);
    assert_eq!(
        func_hash(&module, &identity, "ping"),
        func_hash(&module, &identity, "pong"),
        "two symmetric members must share one structural hash"
    );
    // The program still runs: the linker merges the two into one
    // definition, which is what indistinguishable means.
    let bytes = lm_testkit::compile_to_bytes("t.lm", source).expect("compiles");
    let loaded = lm_vm::load_bytes(&bytes).expect("loads");
    let mut vm = lm_vm::Vm::new(&loaded, lm_vm::VmConfig::default());
    let outcome = vm.run();
    assert_eq!(vm.show_outcome(&outcome), "Done(0)");
}

/// A closure body inside a component takes part in the refinement and
/// enters the component hash. Two functions that differ only inside a
/// nested closure must therefore receive different structural hashes.
#[test]
fn a_nested_closure_body_reaches_the_structural_hash() {
    let one =
        "def f(n: Int): Int\n  g = do |x: Int|: Int\n    f(x - 1)\n  end\n  g(n)\nend\nf(3)\n";
    let two =
        "def f(n: Int): Int\n  g = do |x: Int|: Int\n    f(x - 2)\n  end\n  g(n)\nend\nf(3)\n";
    let (ma, ia) = identity_of(one);
    let (mb, ib) = identity_of(two);
    assert_ne!(
        func_hash(&ma, &ia, "f"),
        func_hash(&mb, &ib, "f"),
        "an edit inside a nested closure moved no structural hash"
    );
}

/// A hand-built cycle whose members differ only far away: member zero
/// carries one extra instruction, and every other member is equal.
/// Refinement must reach every member, one round per step.
fn chain_cycle(n: usize) -> Module {
    let mut funcs = Vec::with_capacity(n);
    for i in 0..n {
        let next = ((i + 1) % n) as u32;
        let mut block = vec![Instr::Call(next)];
        if i == 0 {
            block.push(Instr::Pop);
            block.push(Instr::ConstInt(1));
        }
        block.push(Instr::Return);
        funcs.push(Func {
            name: format!("f{i}"),
            type_params: 0,
            effect_params: 0,
            params: vec![],
            param_muts: vec![],
            ret: 2,
            row: vec![],
            captures: vec![],
            local_types: vec![],
            blocks: vec![block],
        });
    }
    Module {
        strings: vec![],
        types: vec![BcType::Unit, BcType::Bool, BcType::Int, BcType::Str],
        selectors: vec![],
        apps: vec![],
        imports: vec![],
        core_roles: [lm_bytecode::NO_ROLE; lm_bytecode::CORE_ROLE_COUNT],
        classes: vec![],
        funcs,
        entry: 0,
        exports: vec![],
    }
}

/// A wide hostile component: refinement separates every member, the
/// round count follows the chain length, and the result is
/// deterministic. This runs before the verifier, on untrusted bytes.
#[test]
fn a_wide_hostile_component_refines_and_terminates() {
    const N: usize = 256;
    let module = chain_cycle(N);
    let identity = module_identity(&module).expect("hashes");
    let mut hashes = identity.func_hashes.clone();
    hashes.sort_unstable();
    hashes.dedup();
    assert_eq!(hashes.len(), N, "refinement left two members unseparated");
    // The first labels separate member zero, and each round then
    // separates the member before the last one separated.
    assert_eq!(
        identity.max_refine_rounds as usize,
        N - 2,
        "the chain needs one round per member"
    );
    let again = module_identity(&module).expect("hashes");
    assert_eq!(identity.func_hashes, again.func_hashes);
}

/// A wide symmetric component costs two rounds, not one per member.
/// The loop stops as soon as the partition stops refining.
#[test]
fn a_wide_symmetric_component_stops_at_once() {
    const N: usize = 256;
    let mut module = chain_cycle(N);
    // Make member zero equal to the others, so every member is
    // symmetric with every other member.
    module.funcs[0].blocks = vec![vec![Instr::Call(1), Instr::Return]];
    let identity = module_identity(&module).expect("hashes");
    let mut hashes = identity.func_hashes.clone();
    hashes.sort_unstable();
    hashes.dedup();
    assert_eq!(hashes.len(), 1, "every member must share one hash");
    assert_eq!(
        identity.max_refine_rounds, 1,
        "a symmetric component must settle in one round"
    );
}

/// Identity runs on untrusted bytes before the verifier, so the
/// refinement work is bounded. A component that needs more rounds
/// than the budget allows rejects with a clear diagnostic. No source
/// program reaches the bound: it needs a cycle of about three
/// thousand members that refines one member per round.
#[test]
fn a_component_past_the_refinement_budget_rejects() {
    const N: usize = 256;
    const REPEATS: usize = 512;
    let mut module = chain_cycle(N);
    // Every member repeats its one call, so the round cost grows and
    // the round budget falls below the rounds the chain needs.
    for (i, func) in module.funcs.iter_mut().enumerate() {
        let next = ((i + 1) % N) as u32;
        let mut block = vec![Instr::Call(next), Instr::Pop];
        for _ in 1..REPEATS {
            block.push(Instr::Call(next));
            block.push(Instr::Pop);
        }
        if i == 0 {
            block.push(Instr::ConstInt(1));
        } else {
            block.push(Instr::ConstInt(0));
        }
        block.push(Instr::Return);
        func.blocks = vec![block];
    }
    let error = module_identity(&module).expect_err("the budget must reject");
    assert!(error.0.contains("budget"), "{error}");
}

/// Measure the refinement cost on a wide hostile component. Run it
/// with:
///
/// ```text
/// cargo test --release -p lm-testkit --test identity_linking \
///   measure_refinement -- --ignored --nocapture
/// ```
#[test]
#[ignore]
fn measure_refinement_on_a_wide_component() {
    for n in [64usize, 256, 1024, 2048] {
        let module = chain_cycle(n);
        let bytes = lm_bytecode::encode(&module).len();
        let start = std::time::Instant::now();
        let identity = module_identity(&module).expect("hashes");
        println!(
            "chain-{n}: {bytes} bytes, rounds {}, identity {:?}",
            identity.max_refine_rounds,
            start.elapsed()
        );
    }
    for n in [256usize, 2048, 8192] {
        let mut module = chain_cycle(n);
        module.funcs[0].blocks = vec![vec![Instr::Call(1), Instr::Return]];
        let bytes = lm_bytecode::encode(&module).len();
        let start = std::time::Instant::now();
        let identity = module_identity(&module).expect("hashes");
        println!(
            "symmetric-{n}: {bytes} bytes, rounds {}, identity {:?}",
            identity.max_refine_rounds,
            start.elapsed()
        );
    }
}

// ---------------------------------------------------------------
// The self-describing `mut` marker encoding.
// ---------------------------------------------------------------

/// A function whose marker vector does not match its parameter table
/// rejects at the decoder, not at a later pass. The encoding carries
/// its own count, so no reader takes the count from another table.
#[test]
fn a_misaligned_function_marker_vector_rejects_at_the_decoder() {
    let bytes = lm_testkit::compile_to_bytes("t.lm", "def f(n: Int): Int\n  n\nend\nf(1)\n")
        .expect("compiles");
    let mut module = lm_bytecode::decode(&bytes).expect("decodes");
    let idx = module
        .funcs
        .iter()
        .position(|f| f.name == "f")
        .expect("the function exists");
    module.funcs[idx].param_muts.push(false);
    let bad = lm_bytecode::encode(&module);
    assert_eq!(
        lm_bytecode::decode(&bad),
        Err(DecodeError::MutMarkerCount),
        "a misaligned marker vector must reject at the decoder"
    );
}

/// The same rule holds for the marker vector of a function type.
#[test]
fn a_misaligned_type_marker_vector_rejects_at_the_decoder() {
    let bytes = lm_testkit::compile_to_bytes("t.lm", "f = do |x: Int|: Int x + 1 end\nf(1)\n")
        .expect("compiles");
    let mut module = lm_bytecode::decode(&bytes).expect("decodes");
    let idx = module
        .types
        .iter()
        .position(|t| matches!(t, BcType::Fn(..)))
        .expect("the module holds a function type");
    if let BcType::Fn(_, muts, _, _) = &mut module.types[idx] {
        muts.push(true);
    }
    let bad = lm_bytecode::encode(&module);
    assert_eq!(
        lm_bytecode::decode(&bad),
        Err(DecodeError::MutMarkerCount),
        "a misaligned type marker vector must reject at the decoder"
    );
}

/// Two marker shapes never write one semantic region. The count is
/// inside the bytes, so the region separates them without help.
#[test]
fn two_marker_shapes_write_two_semantic_regions() {
    let bytes = lm_testkit::compile_to_bytes("t.lm", "def f(n: Int): Int\n  n\nend\nf(1)\n")
        .expect("compiles");
    let module = lm_bytecode::decode(&bytes).expect("decodes");
    let mut twin = module.clone();
    let idx = twin
        .funcs
        .iter()
        .position(|f| f.name == "f")
        .expect("the function exists");
    twin.funcs[idx].param_muts.push(false);
    assert_ne!(
        lm_bytecode::semantic_section(&module),
        lm_bytecode::semantic_section(&twin),
        "two marker shapes share one semantic region"
    );
}

// ---------------------------------------------------------------
// Section 8: slot resolution.
// ---------------------------------------------------------------

/// Every compiled module declares its core role slots, and the table
/// names the embedded core classes.
#[test]
fn a_compiled_module_declares_every_core_role() {
    let module = compile_text("t.lm", "x = 1\nx\n").expect("compiles");
    for (role, label) in lm_bytecode::corepin::PINNED_LABELS.iter().enumerate() {
        let slot = module.core_roles[role];
        assert_ne!(
            slot,
            lm_bytecode::NO_ROLE,
            "the module declares no slot for `{label}`"
        );
        assert_eq!(
            module.classes[slot as usize].key,
            lm_bytecode::corepin::pinned_key(label),
            "the slot of `{label}` names another class"
        );
    }
}

/// The verifier proves the shape of every declared slot. A table that
/// points a role at a class with another shape rejects.
#[test]
fn a_crafted_core_role_shape_rejects() {
    let module = compile_text("t.lm", "x = 1\nx\n").expect("compiles");
    let some = lm_bytecode::corepin::role_index("Option.Some").expect("the role exists");
    let ok = lm_bytecode::corepin::role_index("Result.Ok").expect("the role exists");
    // `Result.Ok` has the right field but two type parameters and
    // another parent, so it cannot fill the `Option.Some` role.
    let mut twin = module.clone();
    twin.core_roles[some] = module.core_roles[ok];
    let error = lm_verify::verify_module(&twin).expect_err("the crafted role must reject");
    assert!(error.message.contains("core role"), "{error:?}");
    // A role that names a class outside the table rejects at the
    // decoder as well as at the verifier.
    let mut wild = module.clone();
    wild.core_roles[some] = 9999;
    assert!(lm_verify::verify_module(&wild).is_err());
    let bytes = lm_bytecode::encode(&wild);
    assert_eq!(
        lm_bytecode::decode(&bytes),
        Err(DecodeError::BadCoreRole),
        "the decoder admitted a role outside the class table"
    );
}

/// The verifier reads no source name. A rename of every class and
/// every function leaves the verification hash unchanged.
#[test]
fn a_rename_of_every_definition_keeps_the_verification_hash() {
    let source = "class Counter\n  value: Int = 0\n  def add(mut self, n: Int): Int\n    \
                  self.value = self.value + n\n    self.value\n  end\nend\n\
                  c = Counter()\nc.add(1)\n";
    let module = compile_text("t.lm", source).expect("compiles");
    let mut twin = module.clone();
    for (idx, class) in twin.classes.iter_mut().enumerate() {
        class.name = format!("c{idx}");
        class.key = format!("k{idx}");
    }
    for (idx, func) in twin.funcs.iter_mut().enumerate() {
        func.name = format!("f{idx}");
    }
    assert_eq!(
        lm_bytecode::identity::verification_hash(&module),
        lm_bytecode::identity::verification_hash(&twin),
        "a rename moved the verification hash"
    );
    // Both still load, and both agree with the uncached load.
    let mut cache = lm_vm::VerifiedCache::new();
    lm_vm::load_cached(module, &mut cache).expect("loads");
    lm_vm::load_cached(twin, &mut cache).expect("loads");
    assert_eq!(cache.verifications, 1, "the rename cost a verifier run");
}

/// The reserved module path `core` rejects with a clear diagnostic. A
/// module with that path would give a user class a core qualified key.
#[test]
fn the_reserved_core_module_path_rejects() {
    let ast = lm_source::parse::parse("x = 1\nx\n").expect("parses");
    let error = lm_hir::check_module_with(
        &ast,
        lm_hir::CheckOptions {
            module_path: "core".to_string(),
            ..lm_hir::CheckOptions::default()
        },
    )
    .err()
    .expect("the reserved path must reject");
    assert_eq!(error.code, "E0290");
    assert!(error.message.contains("core image"), "{error:?}");
}

/// Measure the load path: the identity cost, the verifier cost, and
/// the module size. Run it with:
///
/// ```text
/// cargo test --release -p lm-testkit --test identity_linking \
///   measure_load_path -- --ignored --nocapture
/// ```
#[test]
#[ignore]
fn measure_load_path() {
    let source = "class Counter\n  value: Int = 0\n  def add(mut self, n: Int): Int\n    \
                  self.value = self.value + n\n    self.value\n  end\n  \
                  def get(self): Int\n    self.value\n  end\nend\n\
                  def run(n: Int): Int\n  c = Counter()\n  i = 0\n  \
                  while i < n\n    c.add(i)\n    i = i + 1\n  end\n  c.get()\nend\n\
                  run(10)\n";
    let bytes = lm_testkit::compile_to_bytes("t.lm", source).expect("compiles");
    let module = lm_bytecode::decode(&bytes).expect("decodes");
    const ROUNDS: u32 = 200;
    let start = std::time::Instant::now();
    for _ in 0..ROUNDS {
        module_identity(&module).expect("hashes");
    }
    let identity = start.elapsed() / ROUNDS;
    let start = std::time::Instant::now();
    for _ in 0..ROUNDS {
        lm_verify::verify_module(&module).expect("verifies");
    }
    let verify = start.elapsed() / ROUNDS;
    let start = std::time::Instant::now();
    for _ in 0..ROUNDS {
        lm_vm::load_bytes(&bytes).expect("loads");
    }
    let load = start.elapsed() / ROUNDS;
    println!(
        "module {} bytes: identity {identity:?} verify {verify:?} load {load:?}",
        bytes.len()
    );
}
