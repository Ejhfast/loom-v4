//! Week-5 identity suites: the sectioned container, definition and
//! module hashes, hash-linked core references, the verified-code
//! cache, and the interface artifact.

use lm_bytecode::identity::{module_identity, ModuleIdentity};
use lm_bytecode::{Func, Instr, Module};
use lm_testkit::{compile_module_text, compile_to_bytes};

fn identity_of(source: &str) -> (Module, ModuleIdentity) {
    let module = compile_module_text("t.lm", source).expect("compiles");
    let identity = module_identity(&module).expect("hashes");
    (module, identity)
}

fn func_hash(module: &Module, identity: &ModuleIdentity, name: &str) -> [u8; 32] {
    let idx = module
        .funcs
        .iter()
        .position(|f| f.name == name)
        .unwrap_or_else(|| panic!("no function `{name}`"));
    identity.func_hashes[idx]
}

fn class_hash(module: &Module, identity: &ModuleIdentity, name: &str) -> [u8; 32] {
    let idx = module
        .classes
        .iter()
        .position(|c| c.name == name)
        .unwrap_or_else(|| panic!("no class `{name}`"));
    identity.class_hashes[idx]
}

// ---------------------------------------------------------------
// Deterministic artifacts.
// ---------------------------------------------------------------

#[test]
fn building_twice_is_byte_identical() {
    let source = "def f(n: Int): Int\n  n + 1\nend\nf(41)\n";
    let a = compile_to_bytes("t.lm", source).unwrap();
    let b = compile_to_bytes("t.lm", source).unwrap();
    assert_eq!(a, b);
}

#[test]
fn a_comment_edit_changes_only_source_bytes() {
    let plain =
        "def f(n: Int): Int\n  if n == 0\n    1\n  else\n    f(n - 1) + 1\n  end\nend\nf(2)\n";
    let commented = "# a leading comment\ndef f(n: Int): Int\n  # inside\n  if n == 0\n    1\n  else\n    f(n - 1) + 1\n  end\nend\n\nf(2)\n";
    let a = compile_to_bytes("t.lm", plain).unwrap();
    let b = compile_to_bytes("t.lm", commented).unwrap();
    // The source attachment preserves comments and changes exact bytes.
    assert_ne!(a, b);
    let (ma, ia) = identity_of(plain);
    let (mb, ib) = identity_of(commented);
    assert_eq!(ia.semantic_hash, ib.semantic_hash);
    assert_eq!(func_hash(&ma, &ia, "f"), func_hash(&mb, &ib, "f"));
    assert_eq!(
        lm_bytecode::identity::verification_hash(&ma),
        lm_bytecode::identity::verification_hash(&mb)
    );
}

#[test]
fn a_body_edit_changes_only_that_definition() {
    let one = "def f(n: Int): Int\n  if n < 0\n    f(n)\n  else\n    n + 1\n  end\nend\ndef g(n: Int): Int\n  if n < 0\n    g(n)\n  else\n    n * 3\n  end\nend\nf(g(1))\n";
    let two = "def f(n: Int): Int\n  if n < 0\n    f(n)\n  else\n    n + 2\n  end\nend\ndef g(n: Int): Int\n  if n < 0\n    g(n)\n  else\n    n * 3\n  end\nend\nf(g(1))\n";
    let (ma, ia) = identity_of(one);
    let (mb, ib) = identity_of(two);
    assert_ne!(func_hash(&ma, &ia, "f"), func_hash(&mb, &ib, "f"));
    assert_eq!(func_hash(&ma, &ia, "g"), func_hash(&mb, &ib, "g"));
    assert_ne!(ia.semantic_hash, ib.semantic_hash);
}

// ---------------------------------------------------------------
// Order independence and rename invariance.
// ---------------------------------------------------------------

/// Reordering the top-level definitions renumbers the string pool,
/// the type table, and the application table, because the
/// definitions share literals and generic instantiations. Every
/// definition hash and the module semantic hash stay unchanged.
#[test]
fn definition_hashes_do_not_depend_on_source_order() {
    let shared_defs = [
        // `alpha` and `beta` share the string literals and the
        // Box[Int] / Box[String] instantiations, so a reorder
        // renumbers the pools.
        "class Box[T]\n  value: T\n  def init(mut self, value: T)\n    self.value = value\n  \
         end\nend\n",
        "def alpha(): String\n  b = Box(\"shared literal\")\n  i = Box(7)\n  \
         \"#{b.value} #{i.value} first\"\nend\n",
        "def beta(): String\n  i = Box(9)\n  b = Box(\"shared literal\")\n  \
         \"#{b.value} #{i.value} second\"\nend\n",
        // Mutual recursion: one strongly connected component.
        "def is_even(n: Int): Bool\n  if n == 0\n    true\n  else\n    is_odd(n - 1)\n  end\nend\n",
        "def is_odd(n: Int): Bool\n  if n == 0\n    false\n  else\n    is_even(n - 1)\n  end\nend\n",
    ];
    let entry = "(alpha(), beta(), is_even(4))\n";
    let forward = format!("{}{entry}", shared_defs.join(""));
    let reversed: Vec<&str> = shared_defs.iter().rev().copied().collect();
    let backward = format!("{}{entry}", reversed.join(""));
    let (ma, ia) = identity_of(&forward);
    let (mb, ib) = identity_of(&backward);
    // The pools really renumbered: the first string differs.
    assert_ne!(
        ma.strings, mb.strings,
        "the reorder must renumber the pools"
    );
    for name in ["alpha", "beta", "is_even", "is_odd", "<entry>"] {
        assert_eq!(
            func_hash(&ma, &ia, name),
            func_hash(&mb, &ib, name),
            "the hash of `{name}` depends on definition order"
        );
    }
    assert_eq!(class_hash(&ma, &ia, "Box"), class_hash(&mb, &ib, "Box"));
    assert_eq!(ia.semantic_hash, ib.semantic_hash);
}

/// Renaming one definition leaves its own hash and every caller's
/// hash unchanged, because references are by hash. The module
/// semantic hash changes through the export table.
#[test]
fn a_rename_changes_the_module_hash_and_no_definition_hash() {
    let before = "def helper(n: Int): Int\n  if n < 0\n    helper(n)\n  else\n    n * 2\n  end\nend\n\
                  def caller(n: Int): Int\n  if n < 0\n    caller(n)\n  else\n    helper(n) + 1\n  end\nend\ncaller(3)\n";
    let after = "def assist(n: Int): Int\n  if n < 0\n    assist(n)\n  else\n    n * 2\n  end\nend\n\
                 def caller(n: Int): Int\n  if n < 0\n    caller(n)\n  else\n    assist(n) + 1\n  end\nend\ncaller(3)\n";
    let (ma, ia) = identity_of(before);
    let (mb, ib) = identity_of(after);
    assert_eq!(
        func_hash(&ma, &ia, "helper"),
        func_hash(&mb, &ib, "assist"),
        "the renamed definition hash changed"
    );
    assert_eq!(
        func_hash(&ma, &ia, "caller"),
        func_hash(&mb, &ib, "caller"),
        "the caller hash changed on a callee rename"
    );
    assert_ne!(ia.semantic_hash, ib.semantic_hash);
}

/// A mutually recursive component hashes deterministically, and its
/// two members receive distinct hashes.
#[test]
fn scc_members_have_distinct_deterministic_hashes() {
    let source = "def ping(n: Int): Int\n  if n == 0\n    0\n  else\n    pong(n - 1)\n  end\nend\n\
                  def pong(n: Int): Int\n  if n == 0\n    1\n  else\n    ping(n - 1)\n  end\nend\n\
                  ping(4)\n";
    let (ma, ia) = identity_of(source);
    let (mb, ib) = identity_of(source);
    assert_eq!(func_hash(&ma, &ia, "ping"), func_hash(&mb, &ib, "ping"));
    assert_eq!(func_hash(&ma, &ia, "pong"), func_hash(&mb, &ib, "pong"));
    assert_ne!(func_hash(&ma, &ia, "ping"), func_hash(&ma, &ia, "pong"));
}

/// Two families with different shapes stay apart, and so do two arms
/// with different parents: an arm names its parent by qualified key,
/// and a family names its arms by qualified key.
///
/// Two arms of one family with equal shapes are symmetric members
/// (specification 3.7). They share one structural hash, and their
/// qualified keys keep them apart.
#[test]
fn a_referenced_qualified_key_separates_identical_shapes() {
    let module = lm_hir::core_image();
    let identity = module_identity(&module).expect("the core image hashes");
    assert_ne!(
        class_hash(&module, &identity, "StepEvent"),
        class_hash(&module, &identity, "DriveEvent")
    );
    assert_ne!(
        class_hash(&module, &identity, "StepEvent.Done"),
        class_hash(&module, &identity, "DriveEvent.Done")
    );
    // `Ran` and `Waiting` are empty case classes of one family, so
    // nothing structural separates them.
    assert_eq!(
        class_hash(&module, &identity, "StepEvent.Ran"),
        class_hash(&module, &identity, "StepEvent.Waiting")
    );
    let key_of = |name: &str| {
        module
            .classes
            .iter()
            .find(|c| c.name == name)
            .map(|c| c.key.clone())
            .expect("the class exists")
    };
    assert_eq!(key_of("StepEvent.Ran"), "core.StepEvent.Ran");
    assert_ne!(key_of("StepEvent.Ran"), key_of("StepEvent.Waiting"));
}

/// A definition chain a few thousand deep hashes on a small Rust
/// stack: the Tarjan walk and the digest passes are iterative.
#[test]
fn a_deep_definition_chain_hashes_on_a_bounded_stack() {
    const CHAIN: usize = 3000;
    let mut funcs = Vec::with_capacity(CHAIN + 1);
    for i in 0..CHAIN {
        funcs.push(Func {
            name: format!("f{i}"),
            param_names: vec![],
            type_params: 0,
            effect_params: 0,
            params: vec![],
            param_muts: vec![],
            ret: 2,
            row: vec![],
            captures: vec![],
            local_types: vec![],
            blocks: vec![vec![Instr::Call(i as u32 + 1), Instr::Return]],
        });
    }
    funcs.push(Func {
        name: "last".to_string(),
        param_names: vec![],
        type_params: 0,
        effect_params: 0,
        params: vec![],
        param_muts: vec![],
        ret: 2,
        row: vec![],
        captures: vec![],
        local_types: vec![],
        blocks: vec![vec![Instr::ConstInt(1), Instr::Return]],
    });
    let module = Module {
        strings: vec![],
        bytes: vec![],
        types: vec![
            lm_bytecode::BcType::Unit,
            lm_bytecode::BcType::Bool,
            lm_bytecode::BcType::Int,
            lm_bytecode::BcType::Str,
        ],
        selectors: vec![],
        apps: vec![],
        interfaces: vec![],
        conformances: vec![],
        class_bounds: vec![],
        func_bounds: vec![vec![]; funcs.len()],
        classes: vec![],
        funcs,
        imports: vec![],
        slots: vec![],
        core_roles: [lm_bytecode::NO_ROLE; lm_bytecode::CORE_ROLE_COUNT],
        entry: 0,
        exports: vec![],
        bindings: vec![],
        debug: Vec::new(),
    };
    let identity = std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(move || module_identity(&module).expect("the chain hashes"))
        .expect("thread starts")
        .join()
        .expect("no Rust stack overflow");
    assert_eq!(identity.func_hashes.len(), CHAIN + 1);
}

// ---------------------------------------------------------------
// Hash-linked core references.
// ---------------------------------------------------------------

#[test]
fn the_loaded_module_carries_the_hash_resolved_core_layout() {
    let bytes = compile_to_bytes("t.lm", "xs = [1, 2]\nxs.get(0).is_some()\n").unwrap();
    let (arena, namespace) = lm_testkit::publish_artifact_bytes(&bytes).expect("loads");
    let vm = lm_vm::Vm::new(arena, namespace, lm_vm::VmConfig::default());
    let core = vm.core_layout();
    assert!(core.option_some.is_some());
    assert!(core.option_none.is_some());
    assert!(core.result_ok.is_some());
    assert!(core.drive_asked.is_some());
}

/// A corrupted embedded core definition keeps its declared role slot,
/// and the verifier rejects the shape. The role table is a claim, and
/// the verifier proves it.
#[test]
fn a_corrupted_core_definition_fails_the_role_shape() {
    let mut module = lm_compiler::core_link_unit()
        .expect("the core unit builds")
        .module()
        .clone();
    // Flip the arm order of the embedded Option family record: swap
    // the field type of Some to String.
    let some = module
        .classes
        .iter()
        .position(|c| c.name == "Option.Some")
        .expect("the embedded core Option.Some exists");
    module.classes[some].fields[0].1 = 3;
    let layout = lm_bytecode::corepin::declared_layout(&module);
    assert!(
        layout.option_some.is_some(),
        "the declared role slot must survive the edit"
    );
    let error = lm_verify::verify_module(&module).expect_err("the corrupted module was admitted");
    assert!(
        error.message.contains("wrong type"),
        "the rejection must name the shape: {error:?}"
    );
}

// ---------------------------------------------------------------
// Artifact publication.
// ---------------------------------------------------------------

#[test]
fn publishing_the_same_artifact_reuses_one_namespace() {
    let artifact =
        lm_testkit::compile_text("t.lm", "def f(n: Int): Int\n  n + 1\nend\nf(41)\n").unwrap();
    let core = lm_compiler::core_link_unit().expect("the core unit builds");
    let mut arena = lm_link::CodeArena::new();
    let first = arena
        .publish(artifact.clone(), Some(core.clone()))
        .expect("the artifact publishes");
    let second = arena
        .publish(artifact, Some(core))
        .expect("the artifact publishes again");
    assert_eq!(first, second);
    assert_eq!(arena.namespace_count(), 1);
    let mut vm = lm_vm::Vm::new(arena, first, lm_vm::VmConfig::default());
    let outcome = vm.run();
    assert_eq!(vm.show_outcome(&outcome), "Done(42)");
}

#[test]
fn a_tampered_artifact_unit_never_publishes() {
    let source = "def f(n: Int): Int\n  n + 1\nend\nf(41)\n";
    let artifact = lm_testkit::compile_text("t.lm", source).unwrap();
    let mut module = artifact.root().module().clone();
    let entry = module.entry as usize;
    module.funcs[entry].blocks[0].insert(0, Instr::Jump(9999));
    let tampered =
        lm_testkit::replace_artifact_root(&artifact, module).expect("the damaged artifact builds");
    assert!(lm_testkit::publish_artifact(&tampered).is_err());
}

/// A duplicate pool entry can keep semantic identity.
/// The independent verifier still rejects the unit.
#[test]
fn hash_equal_noncanonical_code_rejects() {
    let source = "def f(n: Int): Int\n  n + 1\nend\nf(41)\n";
    let mut module = compile_module_text("t.lm", source).unwrap();
    let before = module_identity(&module).unwrap().semantic_hash;
    module.types.push(lm_bytecode::BcType::Int);
    assert_eq!(module_identity(&module).unwrap().semantic_hash, before);
    assert!(lm_verify::verify_module(&module).is_err());
}

/// Review regression: a duplicate selector name keeps the semantic
/// hash equal, because the canonical encoding replaces a selector
/// index with its name. The two indices are different dispatch keys,
/// and only the per-function pass resolved the method. The structural
/// pass must reject the duplicate, or a cache hit admits a module that
/// faults the dispatch table.
#[test]
fn a_duplicate_selector_name_rejects() {
    let source = "class Counter\n  value: Int = 0\n  def add(mut self, n: Int): Int\n    \
                  self.value = self.value + n\n    self.value\n  end\nend\n\
                  c = Counter()\nc.add(1)\n";
    let mut module = compile_module_text("t.lm", source).unwrap();
    let before = module_identity(&module).unwrap().semantic_hash;
    let used = module
        .funcs
        .iter()
        .flat_map(|f| f.blocks.iter())
        .flatten()
        .find_map(|i| match i {
            Instr::CallVirtual { selector, .. } => Some(*selector),
            _ => None,
        })
        .expect("the program calls a method");
    let duplicate = module.selectors.len() as u32;
    module
        .selectors
        .push(module.selectors[used as usize].clone());
    for func in &mut module.funcs {
        for block in &mut func.blocks {
            for instr in block {
                if let Instr::CallVirtual { selector, .. } = instr {
                    if *selector == used {
                        *selector = duplicate;
                    }
                }
            }
        }
    }
    let after = module_identity(&module).unwrap().semantic_hash;
    assert_eq!(after, before, "the duplicate name must keep the hash");

    assert!(lm_verify::verify_module(&module).is_err());
}

/// Review regression: the loader computes the identity of untrusted
/// bytes before the verifier runs. A function that makes a closure of
/// itself is a one-member `MakeClosure` cycle. The cycle marker must
/// cover it; an unfinished body digest must never panic the loader.
#[test]
fn a_self_referential_closure_hashes_without_a_panic() {
    let source = "f = { |x: Int|: Int x + 1 }\nf(1)\n";
    let mut module = compile_module_text("t.lm", source).unwrap();
    let target = module
        .funcs
        .iter()
        .flat_map(|f| f.blocks.iter())
        .flatten()
        .find_map(|i| match i {
            Instr::MakeClosure { func, .. } => Some(*func),
            _ => None,
        })
        .expect("the program makes a closure");
    module.funcs[target as usize].blocks = vec![vec![
        Instr::MakeClosure {
            func: target,
            captures: 0,
        },
        Instr::Return,
    ]];
    // The hash is defined and deterministic.
    let first = module_identity(&module).expect("hashes").semantic_hash;
    let second = module_identity(&module).expect("hashes").semantic_hash;
    assert_eq!(first, second, "the cycle marker must be deterministic");
    assert!(lm_verify::verify_module(&module).is_err());
}

fn point_selector(m: &mut Module, from: u32, to: u32) -> bool {
    let mut done = false;
    for block in m.funcs.iter_mut().flat_map(|f| f.blocks.iter_mut()) {
        for instr in block {
            if let Instr::CallVirtual { selector, .. } = instr {
                if *selector == from {
                    *selector = to;
                    done = true;
                }
            }
        }
    }
    done
}

fn point_new(m: &mut Module, from: u32, to: u32) -> bool {
    let mut done = false;
    for block in m.funcs.iter_mut().flat_map(|f| f.blocks.iter_mut()) {
        for instr in block {
            if let Instr::New(c) = instr {
                if *c == from {
                    *c = to;
                    done = true;
                }
            }
        }
    }
    done
}

/// Two classes with one key can keep one semantic hash after retargeting.
/// The verifier still rejects the invalid call shape.
#[test]
fn a_duplicate_class_key_cannot_bypass_verification() {
    let source =
        "class A\n  x: Int = 0\nend\nclass B\n  x: Int = 0\nend\na = A()\nb = B()\na.x + b.x\n";
    let mut module = compile_module_text("t.lm", source).unwrap();
    let a = module.classes.iter().position(|c| c.name == "A").unwrap() as u32;
    let b = module.classes.iter().position(|c| c.name == "B").unwrap() as u32;

    // Give two classes one name and one key. The verifier rejects
    // the duplicate binding before any execution.
    module.classes[b as usize].name = "A".to_string();
    module.classes[b as usize].key = module.classes[a as usize].key.clone();
    for binding in &mut module.bindings {
        if binding.class == b {
            binding.key = lm_bytecode::ctor_binding_key(&module.classes[b as usize].key);
        }
    }
    lm_verify::verify_module(&module).expect_err("the duplicate binding rejects");
    let before = module_identity(&module).unwrap().semantic_hash;

    // Retarget `New A` to the second class named `A`.
    assert!(point_new(&mut module, a, b));
    let after = module_identity(&module).unwrap().semantic_hash;
    assert_eq!(after, before, "the retarget must keep the semantic hash");

    assert!(lm_verify::verify_module(&module).is_err());
}

/// The verification hash answers a different question from the
/// semantic hash: "did the verifier approve this exact
/// representation?" It keeps every module-global index, so any change
/// the verifier can see moves it.
#[test]
fn the_verification_hash_keeps_every_index() {
    use lm_bytecode::identity::verification_hash;
    let source = "class Counter\n  value: Int = 0\n  def add(mut self, n: Int): Int\n    \
                  self.value = self.value + n\n    self.value\n  end\nend\n\
                  c = Counter()\nc.add(1)\n";
    let module = compile_module_text("t.lm", source).unwrap();
    let base = verification_hash(&module);
    assert_eq!(base, verification_hash(&module), "not deterministic");

    // A duplicate selector name keeps the semantic hash and must move
    // the verification hash.
    let mut dup = module.clone();
    let used = dup
        .funcs
        .iter()
        .flat_map(|f| f.blocks.iter())
        .flatten()
        .find_map(|i| match i {
            Instr::CallVirtual { selector, .. } => Some(*selector),
            _ => None,
        })
        .unwrap();
    let copy = dup.selectors.len() as u32;
    dup.selectors.push(dup.selectors[used as usize].clone());
    assert!(point_selector(&mut dup, used, copy));
    assert_eq!(
        module_identity(&dup).unwrap().semantic_hash,
        module_identity(&module).unwrap().semantic_hash,
        "the case must keep the semantic hash"
    );
    assert_ne!(base, verification_hash(&dup), "the verifier sees this");

    // A dead pool entry must move it too.
    let mut dead = module.clone();
    dead.types.push(lm_bytecode::BcType::Int);
    assert_ne!(base, verification_hash(&dead));
}

/// Definition names live outside the semantic region. Published slot
/// keys live inside it. A source binding rename changes its slot key
/// and verification hash.
#[test]
fn a_published_rename_moves_the_verification_hash() {
    use lm_bytecode::identity::verification_hash;
    let before = "def helper(n: Int): Int\n  if n < 0\n    helper(n)\n  else\n    n * 2\n  end\nend\n\
                  def caller(n: Int): Int\n  if n < 0\n    caller(n)\n  else\n    helper(n) + 1\n  end\nend\ncaller(3)\n";
    let after = "def assist(n: Int): Int\n  if n < 0\n    assist(n)\n  else\n    n * 2\n  end\nend\n\
                 def caller(n: Int): Int\n  if n < 0\n    caller(n)\n  else\n    assist(n) + 1\n  end\nend\ncaller(3)\n";
    let ma = compile_module_text("t.lm", before).unwrap();
    let mb = compile_module_text("t.lm", after).unwrap();
    assert_ne!(
        module_identity(&ma).unwrap().semantic_hash,
        module_identity(&mb).unwrap().semantic_hash,
        "a rename must move the semantic hash through the export table"
    );
    assert_ne!(
        verification_hash(&ma),
        verification_hash(&mb),
        "a published rename must move the verification hash"
    );
    assert_ne!(
        lm_bytecode::semantic_section(&ma),
        lm_bytecode::semantic_section(&mb),
        "a published rename must move the semantic region"
    );
}
