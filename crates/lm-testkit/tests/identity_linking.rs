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
// Section 5: function bindings and function code.
// ---------------------------------------------------------------

/// The binding key of one function value in a compiled module.
fn binding_of(module: &Module, key: &str) -> Option<u32> {
    module
        .bindings
        .iter()
        .find(|b| b.key == key)
        .map(|b| b.func)
}

/// Give one class of a compiled module the qualified key of another
/// provider, as a stale build-cache entry does. The rewrite keeps the
/// module self-consistent: the declaration name and every binding of
/// that class move with the key, because a key is
/// `<module path>.<name>`.
fn rekey_class(unit: &mut lm_compiler::CompiledModule, from: &str, to: &str, name: &str) {
    for class in &mut unit.module.classes {
        if class.key == from {
            class.key = to.to_string();
            class.name = name.to_string();
        }
    }
    let prefix = format!("{from}.");
    for binding in &mut unit.module.bindings {
        if let Some(rest) = binding.key.strip_prefix(&prefix) {
            binding.key = format!("{to}.{rest}");
        }
    }
}

/// Two providers of one class key. The second module declares a class
/// with the same qualified key and a different constructor.
fn two_providers_of_one_class_key(first: &str, second: &str) -> Vec<lm_compiler::CompiledModule> {
    let shapes = compile_one("app.shapes", first, &[], false);
    let mut main = compile_one(
        "app.main",
        second,
        std::slice::from_ref(&shapes.interface),
        true,
    );
    rekey_class(&mut main, "app.main.Spot", "app.shapes.Dot", "Dot");
    vec![shapes, main]
}

/// The measured gap: a class structural hash covers no constructor.
///
/// A field default is inlined into the generated `<new>` function, and
/// an `init` body is reached through it. Neither enters `class_bytes`,
/// so two classes that differ only there share one class structural
/// hash. The constructor binding is what separates them.
#[test]
fn a_class_structural_hash_covers_no_constructor() {
    let defaults = "class Zero\n  x: Int = 0\nend\nclass Seven\n  x: Int = 7\nend\n\
                    Zero().x + Seven().x\n";
    let (module, identity) = identity_of(defaults);
    assert_eq!(
        class_hash(&module, &identity, "Zero"),
        class_hash(&module, &identity, "Seven"),
        "a field default must stay outside the class structural hash"
    );
    assert_ne!(
        func_hash(&module, &identity, "<new Zero>"),
        func_hash(&module, &identity, "<new Seven>"),
        "a field default must reach the constructor structural hash"
    );
    let inits = "class A\n  x: Int = 0\n\n  def init(mut self, n: Int)\n    self.x = n\n  end\n\
                 end\nclass B\n  x: Int = 0\n\n  def init(mut self, n: Int)\n    self.x = n + 1\n \
                 end\nend\nA(1).x + B(1).x\n";
    let (module, identity) = identity_of(inits);
    assert_eq!(
        class_hash(&module, &identity, "A"),
        class_hash(&module, &identity, "B"),
        "an `init` body must stay outside the class structural hash"
    );
    assert_ne!(
        func_hash(&module, &identity, "<new A>"),
        func_hash(&module, &identity, "<new B>"),
        "an `init` body must reach the constructor structural hash"
    );
}

/// Row 2 of the function binding table: two providers of one class key
/// whose field defaults differ reject.
///
/// The two classes share one structural hash, so the class table
/// merges them. Their constructors carry one binding key and two
/// structural hashes, and the binding table rejects.
#[test]
fn two_providers_of_one_key_with_two_field_defaults_reject() {
    let units = two_providers_of_one_class_key(
        "class Dot\n  x: Int = 0\nend\ndef make(): Dot\n  Dot()\nend\n",
        "use shapes\nclass Spot\n  x: Int = 7\nend\n\
         d = shapes.make()\ns = Spot()\nd.x + s.x\n",
    );
    // The class table alone cannot separate the two providers.
    let hash_of = |unit: &lm_compiler::CompiledModule| {
        let id = module_identity(&unit.module).expect("hashes");
        let idx = unit
            .module
            .classes
            .iter()
            .position(|c| c.key == "app.shapes.Dot")
            .expect("the class exists");
        id.class_hashes[idx]
    };
    assert_eq!(
        hash_of(&units[0]),
        hash_of(&units[1]),
        "the probe needs one class key with one structural hash"
    );
    let error = link_units(&units).expect_err("two constructors of one key must reject");
    assert!(error.contains("app.shapes.Dot.<new>"), "{error}");
    assert!(error.contains("two implementations"), "{error}");
    assert!(error.contains("app.shapes"), "{error}");
    assert!(error.contains("app.main"), "{error}");
    assert!(error.contains("rebuild"), "{error}");
}

/// Row 2 again, with two `init` bodies. An `init` is not a method, so
/// it enters no class structural hash either.
#[test]
fn two_providers_of_one_key_with_two_init_bodies_reject() {
    let units = two_providers_of_one_class_key(
        "class Dot\n  x: Int = 0\n\n  def init(mut self, n: Int)\n    self.x = n\n  end\nend\n\
         def make(): Dot\n  Dot(1)\nend\n",
        "use shapes\nclass Spot\n  x: Int = 0\n\n  def init(mut self, n: Int)\n    \
         self.x = n + 1\n  end\nend\nd = shapes.make()\ns = Spot(1)\nd.x + s.x\n",
    );
    let hash_of = |unit: &lm_compiler::CompiledModule| {
        let id = module_identity(&unit.module).expect("hashes");
        let idx = unit
            .module
            .classes
            .iter()
            .position(|c| c.key == "app.shapes.Dot")
            .expect("the class exists");
        id.class_hashes[idx]
    };
    assert_eq!(
        hash_of(&units[0]),
        hash_of(&units[1]),
        "the probe needs one class key with one structural hash"
    );
    let error = link_units(&units).expect_err("two initializers of one key must reject");
    assert!(error.contains("app.shapes.Dot."), "{error}");
    assert!(error.contains("two implementations"), "{error}");
    assert!(error.contains("rebuild"), "{error}");
}

/// A constructor binding is derived from the class key, so the linker
/// checks the derivation. A module that renames a class and keeps the
/// old constructor binding is not self-consistent and rejects.
#[test]
fn a_class_without_its_constructor_binding_rejects() {
    let mut units = two_module_program();
    let spot = units[1]
        .module
        .classes
        .iter()
        .position(|c| c.key == "app.main.Spot")
        .expect("the class exists");
    units[1].module.classes[spot].key = "app.shapes.Dot".to_string();
    let error = link_units(&units).expect_err("an unbound constructor must reject");
    assert!(error.contains("app.shapes.Dot.<new>"), "{error}");
    assert!(error.contains("rebuild"), "{error}");
}

/// Row 3 of the function binding table: two names with one structural
/// hash keep both bindings and share one code object.
///
/// This is the provenance rule. Content merging alone would drop the
/// second name, and the listing would report the first name where the
/// source wrote the second.
#[test]
fn two_equal_bodies_share_one_code_object_and_keep_two_bindings() {
    let shapes = compile_one(
        "app.shapes",
        "def bump(n: Int): Int\n  n + 1\nend\n",
        &[],
        false,
    );
    let main = compile_one(
        "app.main",
        "use shapes\ndef increment(n: Int): Int\n  n + 1\nend\n\
         shapes.bump(1) + increment(2)\n",
        std::slice::from_ref(&shapes.interface),
        true,
    );
    let program = link_units(&[shapes, main]).expect("links");
    let first = binding_of(&program.module, "app.shapes.bump").expect("the first name survives");
    let second =
        binding_of(&program.module, "app.main.increment").expect("the second name survives");
    assert_eq!(
        first, second,
        "two equal bodies must share one function value"
    );
    let listing = lm_hir::dump_cfg(&program.module);
    assert!(listing.contains("binding app.shapes.bump"), "{listing}");
    assert!(listing.contains("binding app.main.increment"), "{listing}");
}

/// Row 1 of the function binding table: one binding key with one
/// structural hash is one binding. Every module embeds the core, so
/// every core binding arrives once per module and survives once.
#[test]
fn every_binding_key_appears_once_in_the_merged_program() {
    let program = link_units(&two_module_program()).expect("links");
    let mut keys: Vec<&str> = program
        .module
        .bindings
        .iter()
        .map(|b| b.key.as_str())
        .collect();
    let total = keys.len();
    keys.sort_unstable();
    keys.dedup();
    assert_eq!(total, keys.len(), "the merged program holds one key twice");
    assert!(
        keys.contains(&"core.Option.<new>"),
        "the core lost a binding"
    );
    assert!(
        keys.contains(&"core.Option.is_some"),
        "the core lost a method"
    );
    assert!(
        keys.contains(&"app.shapes.make"),
        "a user binding is missing"
    );
}

/// Every function a module names carries one binding. The set with no
/// binding is exactly the entry and the closure bodies, and no source
/// name reaches either. A generated definition that escapes both the
/// class structural hash and the binding table would appear here.
#[test]
fn every_named_function_carries_one_binding() {
    let source = "class Counter\n  value: Int = 1\n\n  def init(mut self, n: Int)\n    \
                  self.value = n\n  end\n\n  def add(mut self, n: Int): Int\n    \
                  self.value = self.value + n\n    self.value\n  end\nend\n\
                  enum Shape\n  Dot\n  Line(n: Int)\nend\n\
                  def twice(n: Int): Int\n  n * 2\nend\n\
                  add_one = do |x: Int|: Int x + 1 end\n\
                  c = Counter(2)\ntwice(c.add(1)) + add_one(1)\n";
    let module = compile_text("t.lm", source).expect("compiles");
    let bound: Vec<u32> = module.bindings.iter().map(|b| b.func).collect();
    for (idx, func) in module.funcs.iter().enumerate() {
        if bound.contains(&(idx as u32)) {
            continue;
        }
        assert!(
            func.name == "<entry>" || func.name.starts_with("<closure "),
            "the function `{}` carries no binding",
            func.name
        );
    }
    let keys: Vec<&str> = module.bindings.iter().map(|b| b.key.as_str()).collect();
    for key in [
        "twice",
        "Counter.add",
        "Counter.init",
        "Counter.<new>",
        "Shape.<new>",
        "Shape.Dot.<new>",
        "Shape.Line.<new>",
    ] {
        assert!(keys.contains(&key), "the binding `{key}` is missing");
    }
}

/// A binding key is a name, so it enters no structural hash. It enters
/// the module semantic hash, because a module that binds other names
/// is another module.
#[test]
fn a_binding_key_never_enters_a_structural_hash() {
    let (mut module, identity) = identity_of("def f(n: Int): Int\n  n + 1\nend\nf(1)\n");
    for binding in &mut module.bindings {
        binding.key = format!("z{}", binding.key);
    }
    let renamed = module_identity(&module).expect("hashes");
    assert_eq!(
        identity.func_hashes, renamed.func_hashes,
        "a binding key moved a function structural hash"
    );
    assert_eq!(
        identity.class_hashes, renamed.class_hashes,
        "a binding key moved a class structural hash"
    );
    assert_ne!(
        identity.semantic_hash, renamed.semantic_hash,
        "a module that binds other names must hash differently"
    );
    assert_eq!(
        lm_vm::verified_key(&module),
        lm_vm::verified_key(&identity_of("def f(n: Int): Int\n  n + 1\nend\nf(1)\n").0),
        "a binding key must not reach the verifier"
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
        bindings: vec![],
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

/// Several independent components, each one cycle of `per` members.
/// Every member repeats its one call `repeats` times, so one round
/// costs more and the module budget binds at a readable size.
fn many_chains(count: usize, per: usize, repeats: usize) -> Module {
    let mut funcs = Vec::with_capacity(count * per);
    for c in 0..count {
        let base = c * per;
        for i in 0..per {
            let next = (base + (i + 1) % per) as u32;
            let mut block = Vec::with_capacity(2 * repeats + 2);
            for _ in 0..repeats {
                block.push(Instr::Call(next));
                block.push(Instr::Pop);
            }
            // One member of each cycle differs, so refinement
            // separates one more member in every round.
            block.push(Instr::ConstInt(if i == 0 { c as i64 + 1 } else { 0 }));
            block.push(Instr::Return);
            funcs.push(Func {
                name: format!("f{c}_{i}"),
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
        bindings: vec![],
    }
}

/// The component budget bounds one component. A module holds many
/// components, and their cost adds up, so the module carries its own
/// budget over the sum. Without it a crafted module reaches any cost
/// through many components that each stay inside the component budget.
///
/// One component of this shape stays inside the component budget, and
/// three of them pass the module budget.
#[test]
fn a_module_past_the_module_refinement_budget_rejects() {
    const PER: usize = 512;
    const REPEATS: usize = 62;
    module_identity(&many_chains(1, PER, REPEATS)).expect("one component stays inside the budget");
    let error =
        module_identity(&many_chains(3, PER, REPEATS)).expect_err("the module budget must reject");
    assert!(error.0.contains("module"), "{error}");
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
    // The module budget over several worst-case components. The
    // component budget bounds each one; the module budget bounds the
    // sum, so the cost of one module stays bounded.
    for count in [1usize, 2, 4, 5, 8, 64] {
        let module = many_chains(count, 2048, 1);
        let bytes = lm_bytecode::encode(&module).len();
        let start = std::time::Instant::now();
        let result = module_identity(&module);
        let took = start.elapsed();
        let verdict = match result {
            Ok(id) => format!("accepted, rounds {}", id.max_refine_rounds),
            Err(e) => format!("rejected: {}", e.0),
        };
        println!(
            "{count} x chain-2048: {bytes} bytes ({:.1} MiB), {took:?}, {verdict}",
            bytes as f64 / (1024.0 * 1024.0)
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

// ---------------------------------------------------------------
// Review regressions: the constructor binding must name the
// construction function of its own class, and nothing else.
//
// A key check on its own proved nothing about the target. A binding
// then named an import slot, a decoy with a matching hash, or an
// ordinary function, and two providers of one class key merged into
// one class with two live constructors.
// ---------------------------------------------------------------

/// The two providers of `app.shapes.Dot`, with one field default each.
/// Unmutated, this pair must reject.
fn dot_providers() -> Vec<lm_compiler::CompiledModule> {
    two_providers_of_one_class_key(
        "class Dot\n  x: Int = 0\nend\n",
        "use shapes\n\nclass Spot\n  x: Int = 7\nend\n\
         d = shapes.Dot()\ns = Spot()\nd.x + s.x\n",
    )
}

/// The control: the honest conflict still rejects.
#[test]
fn two_providers_of_one_key_still_reject() {
    let units = dot_providers();
    assert!(
        link_units(&units).is_err(),
        "two providers of one class key linked"
    );
}

/// A constructor binding that names an imported declaration rejects.
/// `merge_bindings` skips an imported target, so a binding parked
/// there switched the conflict rule off for the exact case it catches.
#[test]
fn a_constructor_binding_on_an_import_slot_rejects() {
    let mut units = dot_providers();
    let extern_funcs = units[1].module.extern_funcs();
    let Some(slot) = extern_funcs.iter().position(|e| *e) else {
        panic!("the module imports nothing");
    };
    let binding = units[1]
        .module
        .bindings
        .iter_mut()
        .find(|b| b.key == "app.shapes.Dot.<new>")
        .expect("the constructor binding exists");
    binding.func = slot as u32;
    let error = link_units(&units).expect_err("the crafted module linked");
    assert!(
        error.contains("imported declaration"),
        "unexpected message: {error}"
    );
}

/// A constructor binding that names any other function rejects, even
/// when that function's structural hash matches the honest one.
#[test]
fn a_constructor_binding_on_another_function_rejects() {
    let mut units = dot_providers();
    let extern_funcs = units[1].module.extern_funcs();
    let bound = units[1]
        .module
        .bindings
        .iter()
        .find(|b| b.key == "app.shapes.Dot.<new>")
        .map(|b| b.func)
        .expect("the constructor binding exists");
    let target = (0..units[1].module.funcs.len() as u32)
        .find(|f| *f != bound && !extern_funcs[*f as usize])
        .expect("the module holds another local function");
    let binding = units[1]
        .module
        .bindings
        .iter_mut()
        .find(|b| b.key == "app.shapes.Dot.<new>")
        .expect("the constructor binding exists");
    binding.func = target;
    let error = link_units(&units).expect_err("the crafted module linked");
    assert!(
        error.contains("construction function") || error.contains("needs the key"),
        "unexpected message: {error}"
    );
}

/// Two constructor bindings for one class reject.
#[test]
fn two_constructor_bindings_for_one_class_reject() {
    let mut units = dot_providers();
    let copy = units[1]
        .module
        .bindings
        .iter()
        .find(|b| b.key == "app.shapes.Dot.<new>")
        .cloned()
        .expect("the constructor binding exists");
    units[1].module.bindings.push(copy);
    let error = link_units(&units).expect_err("the crafted module linked");
    assert!(
        error.contains("two constructor bindings"),
        "unexpected message: {error}"
    );
}

/// A constructor binding on an imported class rejects. A module binds
/// the constructor of a class it defines, never one it imports.
#[test]
fn a_constructor_binding_on_an_imported_class_rejects() {
    let mut units = dot_providers();
    let extern_classes = units[1].module.extern_classes();
    let Some(imported) = extern_classes.iter().position(|e| *e) else {
        panic!("the module imports no class");
    };
    let key = units[1].module.classes[imported].key.clone();
    let func = units[1]
        .module
        .bindings
        .iter()
        .find(|b| b.class != lm_bytecode::NO_CLASS)
        .map(|b| b.func)
        .expect("a constructor binding exists");
    units[1].module.bindings.push(lm_bytecode::FuncBinding {
        key: lm_bytecode::ctor_binding_key(&key),
        func,
        class: imported as u32,
    });
    let error = link_units(&units).expect_err("the crafted module linked");
    assert!(
        error.contains("imported class"),
        "unexpected message: {error}"
    );
}

/// An export whose construction function differs from the binding
/// rejects. A caller reaches the export, so the two must agree.
#[test]
fn an_export_and_a_binding_that_disagree_reject() {
    let mut units = dot_providers();
    let count = units[0].module.funcs.len() as u32;
    assert!(count > 1, "the module has more than one function");
    let export = units[0]
        .module
        .exports
        .iter_mut()
        .find(|e| e.kind.is_class() && e.ctor != lm_bytecode::NO_CTOR)
        .expect("the module exports a class with a constructor");
    // Any other function index proves the disagreement.
    let other = (export.ctor + 1) % count;
    assert_ne!(export.ctor, other, "pick a different function");
    export.ctor = other;
    let error = link_units(&units).expect_err("the crafted module linked");
    assert!(
        error.contains("construction function"),
        "unexpected message: {error}"
    );
}

/// A hand-built module reaches identity and the linker without a
/// decoder. A class index out of range must reject, never panic.
#[test]
fn a_binding_class_index_out_of_range_rejects() {
    let mut module = compile_text("t.lm", "class A\n  x: Int = 0\nend\na = A()\na.x\n").unwrap();
    module.bindings.push(lm_bytecode::FuncBinding {
        key: "t.Z.<new>".to_string(),
        func: 0,
        class: 9_000_000,
    });
    let result = std::panic::catch_unwind(|| module_identity(&module));
    assert!(result.is_ok(), "identity panicked on a bad class index");
    let error = result.unwrap().expect_err("a bad class index was accepted");
    assert!(
        error.to_string().contains("class index out of range"),
        "unexpected message: {error}"
    );
}

/// The binding table declares its own count. One entry needs twelve
/// bytes, and the general length rule bounds a count at one byte per
/// entry, so the decoder checks the real cost before it reserves.
#[test]
fn an_impossible_binding_count_rejects_before_the_reserve() {
    let module = compile_text("t.lm", "def f(n: Int): Int\n  n + 1\nend\nf(1)\n").unwrap();
    let good = lm_bytecode::encode(&module);
    assert!(lm_bytecode::decode(&good).is_ok(), "the sample must decode");
    // Every four-byte window: a forged count must never reserve past
    // the input. A rejection or an unrelated success both hold; a
    // panic or an allocation failure does not.
    for at in 0..good.len().saturating_sub(4) {
        let mut bad = good.clone();
        bad[at..at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        let result = std::panic::catch_unwind(|| lm_bytecode::decode(&bad));
        assert!(result.is_ok(), "the decoder panicked at offset {at}");
    }
}

/// A selector name lives in the semantic region, so a selector rename
/// moves the verification hash. A class rename and a function rename
/// do not.
#[test]
fn a_selector_rename_moves_the_verification_hash() {
    use lm_bytecode::identity::verification_hash;
    let foo = compile_text(
        "t.lm",
        "class C\n  def foo(self): Int\n    1\n  end\nend\nC().foo()\n",
    )
    .unwrap();
    let bar = compile_text(
        "t.lm",
        "class C\n  def bar(self): Int\n    1\n  end\nend\nC().bar()\n",
    )
    .unwrap();
    assert_ne!(
        verification_hash(&foo),
        verification_hash(&bar),
        "a selector rename must move the verification hash"
    );
    let renamed = compile_text(
        "t.lm",
        "class D\n  def foo(self): Int\n    1\n  end\nend\nD().foo()\n",
    )
    .unwrap();
    assert_eq!(
        verification_hash(&foo),
        verification_hash(&renamed),
        "a class rename must hold the verification hash"
    );
}

/// A snapshot classification change moves the verification hash of
/// every module.
///
/// `OpDef.snapshot` decides whether a pending instance of one
/// operation holds live host state, so it changes snapshot and
/// resource behavior. The operation identity now covers it, the
/// manifest digest covers the identities, and the verification hash
/// covers the manifest digest. A verified-code cache and an admitted
/// snapshot therefore cannot survive that change.
#[test]
fn a_snapshot_classification_change_moves_the_verification_hash() {
    use lm_abi::{
        identity_of, manifest_digest, manifest_digest_of, op, op_identity, op_name, SnapshotClass,
        OP_CLOCK_NOW, OP_COUNT,
    };
    use lm_bytecode::identity::{verification_hash, verification_hash_with};
    let module = compile_text("t.lm", "x = 1\nx\n").unwrap();
    let mut flipped = *op(OP_CLOCK_NOW);
    flipped.snapshot = SnapshotClass::HostAttachment;
    let name = op_name(OP_CLOCK_NOW);
    let mutated: Vec<[u8; 32]> = (0..OP_COUNT)
        .map(|slot| {
            if slot == OP_CLOCK_NOW {
                identity_of(&name, &flipped)
            } else {
                op_identity(slot)
            }
        })
        .collect();
    let manifest = manifest_digest_of(&mutated);
    assert_ne!(manifest, manifest_digest());
    assert_ne!(
        verification_hash_with(manifest, &module),
        verification_hash(&module),
        "a classification change must move the verification hash"
    );
}

/// Every field of one operation definition reaches its identity.
///
/// The identity encoder once read `params` and `reply` for a `Fixed`
/// entry and `schema` for a `VmControl` entry alone. `Vm.SnapshotSelf`
/// is `VmControl` with a reply the verifier reads, so that reply could
/// change and move no digest at all.
#[test]
fn every_field_of_one_operation_definition_moves_its_identity() {
    use lm_abi::{
        identity_of, op, op_identity, op_name, AbiType, OpKind, SnapshotClass, OP_CLOCK_NOW,
        OP_VM_SNAPSHOT_SELF,
    };
    // The reply of one `VmControl` entry.
    let name = op_name(OP_VM_SNAPSHOT_SELF);
    let mut edited = *op(OP_VM_SNAPSHOT_SELF);
    assert_eq!(edited.kind, OpKind::VmControl);
    assert_ne!(edited.reply, AbiType::Unit);
    edited.reply = AbiType::Unit;
    assert_ne!(
        identity_of(&name, &edited),
        op_identity(OP_VM_SNAPSHOT_SELF),
        "a VmControl reply change must move the operation identity"
    );
    // The parameters of one `VmControl` entry.
    let mut edited = *op(OP_VM_SNAPSHOT_SELF);
    edited.params = &[AbiType::Int];
    assert_ne!(
        identity_of(&name, &edited),
        op_identity(OP_VM_SNAPSHOT_SELF),
        "a VmControl parameter change must move the operation identity"
    );
    // The schema of one `Fixed` entry, and every other field of it.
    let name = op_name(OP_CLOCK_NOW);
    let base = *op(OP_CLOCK_NOW);
    let mut edits = Vec::new();
    let mut with_schema = base;
    with_schema.schema = "() -> Int";
    edits.push(("schema", with_schema));
    let mut with_kind = base;
    with_kind.kind = OpKind::VmControl;
    edits.push(("kind", with_kind));
    let mut with_group = base;
    with_group.group = "Rand";
    edits.push(("group", with_group));
    let mut with_member = base;
    with_member.member = "Later";
    edits.push(("member", with_member));
    let mut with_params = base;
    with_params.params = &[AbiType::Str];
    edits.push(("params", with_params));
    let mut with_reply = base;
    with_reply.reply = AbiType::Str;
    edits.push(("reply", with_reply));
    let mut with_snapshot = base;
    with_snapshot.snapshot = SnapshotClass::HostAttachment;
    edits.push(("snapshot", with_snapshot));
    for (field, edited) in edits {
        assert_ne!(
            identity_of(&name, &edited),
            op_identity(OP_CLOCK_NOW),
            "a change of `{field}` must move the operation identity"
        );
    }
}
