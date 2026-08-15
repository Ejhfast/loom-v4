//! Week-5 identity suites: the sectioned container, definition and
//! module hashes, hash-linked core references, the verified-code
//! cache, and the interface artifact.

use lm_bytecode::identity::{module_identity, ModuleIdentity};
use lm_bytecode::{Func, Instr, Module};
use lm_testkit::{compile_text, compile_to_bytes};

fn identity_of(source: &str) -> (Module, ModuleIdentity) {
    let module = compile_text("t.lm", source).expect("compiles");
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
fn a_comment_edit_changes_no_hash_and_no_byte() {
    let plain = "def f(n: Int): Int\n  n + 1\nend\nf(41)\n";
    let commented = "# a leading comment\ndef f(n: Int): Int\n  # inside\n  n + 1\nend\n\nf(41)\n";
    let a = compile_to_bytes("t.lm", plain).unwrap();
    let b = compile_to_bytes("t.lm", commented).unwrap();
    // With an empty debug section the container bytes are identical,
    // so the container hash may only change when debug content
    // changes.
    assert_eq!(a, b);
    let (ma, ia) = identity_of(plain);
    let (mb, ib) = identity_of(commented);
    assert_eq!(ia.semantic_hash, ib.semantic_hash);
    assert_eq!(func_hash(&ma, &ia, "f"), func_hash(&mb, &ib, "f"));
}

#[test]
fn a_body_edit_changes_only_that_definition() {
    let one = "def f(n: Int): Int\n  n + 1\nend\ndef g(n: Int): Int\n  n * 3\nend\nf(g(1))\n";
    let two = "def f(n: Int): Int\n  n + 2\nend\ndef g(n: Int): Int\n  n * 3\nend\nf(g(1))\n";
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
         \"{b.value} {i.value} first\"\nend\n",
        "def beta(): String\n  i = Box(9)\n  b = Box(\"shared literal\")\n  \
         \"{b.value} {i.value} second\"\nend\n",
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
    let before = "def helper(n: Int): Int\n  n * 2\nend\n\
                  def caller(n: Int): Int\n  helper(n) + 1\nend\ncaller(3)\n";
    let after = "def assist(n: Int): Int\n  n * 2\nend\n\
                 def caller(n: Int): Int\n  assist(n) + 1\nend\ncaller(3)\n";
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

/// Structurally identical enum families and arms stay distinct: the
/// family forms one component, and the name-ordered member ordinals
/// separate them.
#[test]
fn identical_shapes_in_different_families_stay_distinct() {
    let source = "x: Option[Int] = None\nx.is_none()\n";
    let (module, identity) = identity_of(source);
    assert_ne!(
        class_hash(&module, &identity, "StepEvent"),
        class_hash(&module, &identity, "DriveEvent")
    );
    assert_ne!(
        class_hash(&module, &identity, "StepEvent.Ran"),
        class_hash(&module, &identity, "StepEvent.Waiting")
    );
    assert_ne!(
        class_hash(&module, &identity, "StepEvent.Done"),
        class_hash(&module, &identity, "DriveEvent.Done")
    );
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
        types: vec![
            lm_bytecode::BcType::Unit,
            lm_bytecode::BcType::Bool,
            lm_bytecode::BcType::Int,
            lm_bytecode::BcType::Str,
        ],
        selectors: vec![],
        apps: vec![],
        classes: vec![],
        funcs,
        imports: vec![],
        entry: 0,
        exports: vec![],
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
    let loaded = lm_vm::load_bytes(&bytes).expect("loads");
    let core = loaded.core_layout();
    assert!(core.option_some.is_some());
    assert!(core.option_none.is_some());
    assert!(core.run_done.is_some());
    assert!(core.drive_asked.is_some());
}

/// A corrupted embedded core definition changes its hash, so the
/// layout slot resolves to nothing and the verifier rejects the
/// instruction that needs the family.
#[test]
fn a_corrupted_core_definition_no_longer_resolves() {
    let bytes = compile_to_bytes("t.lm", "xs = [1, 2]\nxs.get(0).is_some()\n").unwrap();
    let mut module = lm_bytecode::decode(&bytes).unwrap();
    // Flip the arm order of the embedded Option family record: swap
    // the field type of Some to String.
    let some = module
        .classes
        .iter()
        .position(|c| c.name == "Option.Some")
        .expect("the embedded core Option.Some exists");
    module.classes[some].fields[0].1 = 3;
    let identity = module_identity(&module).expect("still hashes");
    let layout = lm_bytecode::corepin::core_layout(&module, &identity);
    assert!(layout.option_some.is_none(), "a corrupted arm resolved");
    assert!(
        lm_vm::load(module).is_err(),
        "the corrupted module was admitted"
    );
}

// ---------------------------------------------------------------
// The verified-code cache.
// ---------------------------------------------------------------

#[test]
fn a_second_load_of_the_same_semantic_module_skips_verification() {
    let bytes = compile_to_bytes("t.lm", "def f(n: Int): Int\n  n + 1\nend\nf(41)\n").unwrap();
    let mut cache = lm_vm::VerifiedCache::new();
    let loaded = lm_vm::load_bytes_cached(&bytes, &mut cache).expect("loads");
    assert_eq!(cache.verifications, 1);
    let again = lm_vm::load_bytes_cached(&bytes, &mut cache).expect("loads");
    assert_eq!(cache.verifications, 1, "the second load re-verified");
    // Both admissions execute.
    for loaded in [&loaded, &again] {
        let mut vm = lm_vm::Vm::new(loaded, lm_vm::VmConfig::default());
        let outcome = vm.run();
        assert_eq!(vm.show_outcome(&outcome), "Done(42)");
    }
    // A different module misses and verifies.
    let other = compile_to_bytes("t.lm", "def f(n: Int): Int\n  n + 2\nend\nf(40)\n").unwrap();
    lm_vm::load_bytes_cached(&other, &mut cache).expect("loads");
    assert_eq!(cache.verifications, 2);
}

/// Poisoning: the loader recomputes the semantic hash from the
/// decoded content, so tampered bytes can never ride a cached entry.
/// The tampered module misses the cache and the verifier rejects it.
#[test]
fn tampered_bytes_never_ride_a_cached_admission() {
    let source = "def f(n: Int): Int\n  n + 1\nend\nf(41)\n";
    let bytes = compile_to_bytes("t.lm", source).unwrap();
    let mut cache = lm_vm::VerifiedCache::new();
    lm_vm::load_bytes_cached(&bytes, &mut cache).expect("loads");
    assert_eq!(cache.verifications, 1);
    // Tamper: retarget a jump in the entry to an invalid block. The
    // bytes decode, but the semantics differ, so the recomputed hash
    // misses the cache and the full verifier rejects the module.
    let mut module = lm_bytecode::decode(&bytes).unwrap();
    let entry = module.entry as usize;
    module.funcs[entry].blocks[0].insert(0, Instr::Jump(9999));
    let tampered = lm_bytecode::encode(&module);
    let result = lm_vm::load_bytes_cached(&tampered, &mut cache);
    assert!(result.is_err(), "tampered bytes were admitted");
    assert_eq!(cache.verifications, 1, "a rejection never enters the cache");
}

/// Review regression: a dead duplicate pool entry keeps the semantic
/// hash equal, because the hash covers referenced content only. The
/// structural pass must still reject the stream on every load; the
/// cache may skip only the function dataflow.
#[test]
fn hash_equal_noncanonical_bytes_reject_on_a_cache_hit() {
    let source = "def f(n: Int): Int\n  n + 1\nend\nf(41)\n";
    let bytes = compile_to_bytes("t.lm", source).unwrap();
    let mut cache = lm_vm::VerifiedCache::new();
    lm_vm::load_bytes_cached(&bytes, &mut cache).expect("loads");
    assert_eq!(cache.verifications, 1);
    // Append one dead duplicate type entry. The verifier rejects the
    // table; the semantic hash does not change.
    let mut module = lm_bytecode::decode(&bytes).unwrap();
    module.types.push(lm_bytecode::BcType::Int);
    let bad = lm_bytecode::encode(&module);
    let plain = lm_vm::load_bytes(&bad);
    assert!(plain.is_err(), "the uncached load accepted the table");
    let cached = lm_vm::load_bytes_cached(&bad, &mut cache);
    assert!(cached.is_err(), "the cached load bypassed the verifier");
    assert_eq!(cache.verifications, 1);
    // The valid bytes still hit the cache afterward.
    lm_vm::load_bytes_cached(&bytes, &mut cache).expect("loads");
    assert_eq!(cache.verifications, 1);
}

/// Review regression: a duplicate selector name keeps the semantic
/// hash equal, because the canonical encoding replaces a selector
/// index with its name. The two indices are different dispatch keys,
/// and only the per-function pass resolved the method. The structural
/// pass must reject the duplicate, or a cache hit admits a module that
/// faults the dispatch table.
#[test]
fn a_duplicate_selector_name_rejects_on_a_cache_hit() {
    let source = "class Counter\n  value: Int = 0\n  def add(mut self, n: Int): Int\n    \
                  self.value = self.value + n\n    self.value\n  end\nend\n\
                  c = Counter()\nc.add(1)\n";
    let bytes = compile_to_bytes("t.lm", source).unwrap();
    let mut cache = lm_vm::VerifiedCache::new();
    lm_vm::load_bytes_cached(&bytes, &mut cache).expect("loads");
    assert_eq!(cache.verifications, 1);

    // Append a copy of the selector a call already uses, then point
    // that call at the copy.
    let mut module = lm_bytecode::decode(&bytes).unwrap();
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

    let bad = lm_bytecode::encode(&module);
    assert!(
        lm_vm::load_bytes(&bad).is_err(),
        "the uncached load accepted the table"
    );
    assert!(
        lm_vm::load_bytes_cached(&bad, &mut cache).is_err(),
        "the cached load bypassed the verifier"
    );
    assert_eq!(cache.verifications, 1, "a rejection never enters the cache");
}

/// Review regression: the loader computes the identity of untrusted
/// bytes before the verifier runs. A function that makes a closure of
/// itself is a one-member `MakeClosure` cycle. The cycle marker must
/// cover it; an unfinished body digest must never panic the loader.
#[test]
fn a_self_referential_closure_hashes_without_a_panic() {
    let source = "f = do |x: Int|: Int x + 1 end\nf(1)\n";
    let mut module = compile_text("t.lm", source).unwrap();
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
    // The same bytes reach the loader, which must reject and not panic.
    let bytes = lm_bytecode::encode(&module);
    assert!(
        lm_vm::load_bytes(&bytes).is_err(),
        "the loader admitted a hand-built closure cycle"
    );
}

/// The cache invariant: for one byte stream, a cached load and an
/// uncached load must always agree on admission. This sweeps the four
/// tables whose index the canonical encoding replaces with content.
/// Each case copies the entry an instruction already names, then
/// points that instruction at the copy.
///
/// The `stable` flag records whether the mutation keeps the module
/// semantic hash. Four cases keep it, because the canonical encoding
/// replaces the mutated index with content. The class twin no longer
/// keeps it: compiler ABI version 2 makes class identity nominal, so
/// `A` and `B` are different definitions and `New A` retargeted to
/// `B` moves the module hash. The admission invariant is the durable
/// part and holds in both directions.
#[test]
fn a_cached_load_and_an_uncached_load_always_agree() {
    let class_src = "class Counter\n  value: Int = 0\n  def add(mut self, n: Int): Int\n    \
                     self.value = self.value + n\n    self.value\n  end\n  \
                     def get(self): Int\n    self.value\n  end\nend\n\
                     c = Counter()\nc.add(1)\nc.get()\n";
    let str_src = "def label(): String\n  \"hello\"\nend\nlabel()\n";
    let gen_src = "def id[T](x: T): T\n  x\nend\nid[Int](1)\n";
    let twin_src = "class A\n  x: Int = 0\nend\nclass B\n  x: Int = 0\nend\na = A()\na.x\n";

    // (label, source, mutation, stable). Each mutation returns true
    // when it applied. `stable` states whether the module semantic
    // hash must survive the mutation.
    type Mutation = fn(&mut Module) -> bool;
    let cases: [(&str, &str, Mutation, bool); 7] = [
        (
            "selector",
            class_src,
            |m: &mut Module| {
                let Some(used) = used_selector(m) else {
                    return false;
                };
                let copy = m.selectors.len() as u32;
                m.selectors.push(m.selectors[used as usize].clone());
                point_selector(m, used, copy)
            },
            true,
        ),
        (
            "string",
            str_src,
            |m: &mut Module| {
                let Some(used) = used_string(m) else {
                    return false;
                };
                let copy = m.strings.len() as u32;
                m.strings.push(m.strings[used as usize].clone());
                point_string(m, used, copy)
            },
            true,
        ),
        (
            "application",
            gen_src,
            |m: &mut Module| {
                let Some(used) = used_app(m) else {
                    return false;
                };
                let copy = m.apps.len() as u32;
                m.apps.push(m.apps[used as usize].clone());
                point_app(m, used, copy)
            },
            true,
        ),
        (
            "dead type",
            class_src,
            |m: &mut Module| {
                m.types.push(lm_bytecode::BcType::Int);
                true
            },
            true,
        ),
        // Two classes with identical structure kept one definition
        // hash before compiler ABI version 2. Class identity is now
        // nominal, so retargeting `New` between them moves the module
        // hash. Only the dataflow pass sees the class index, so the
        // admission verdict must still agree.
        (
            "class twin",
            twin_src,
            |m: &mut Module| {
                let a = m.classes.iter().position(|c| c.name == "A").unwrap() as u32;
                let b = m.classes.iter().position(|c| c.name == "B").unwrap() as u32;
                point_new(m, a, b)
            },
            false,
        ),
        // Week 6: an import slot turns a definition into a
        // declaration. The slot is inside the semantic region, so the
        // module hash and the verification hash both move, and the
        // loader rejects the module for its unresolved slot.
        (
            "import slot",
            str_src,
            |m: &mut Module| {
                let target = m.funcs.iter().position(|f| f.name == "label").unwrap() as u32;
                m.imports.push(lm_bytecode::Import {
                    module: "other".to_string(),
                    name: "label".to_string(),
                    kind: lm_bytecode::ImportKind::Func,
                    def: target,
                    hash: [3u8; 32],
                });
                true
            },
            false,
        ),
        // A moved pin is a different import requirement, so the hash
        // moves with it.
        (
            "moved pin",
            str_src,
            |m: &mut Module| {
                let target = m.funcs.iter().position(|f| f.name == "label").unwrap() as u32;
                m.funcs[target as usize].blocks.clear();
                m.funcs[target as usize].local_types = m.funcs[target as usize].params.clone();
                m.imports.push(lm_bytecode::Import {
                    module: "other".to_string(),
                    name: "label".to_string(),
                    kind: lm_bytecode::ImportKind::Func,
                    def: target,
                    hash: [9u8; 32],
                });
                true
            },
            false,
        ),
    ];

    for (label, source, mutate, stable) in cases {
        let bytes = compile_to_bytes("t.lm", source).unwrap();
        let mut cache = lm_vm::VerifiedCache::new();
        lm_vm::load_bytes_cached(&bytes, &mut cache).expect("loads");

        let mut module = lm_bytecode::decode(&bytes).unwrap();
        let before = module_identity(&module).unwrap().semantic_hash;
        assert!(mutate(&mut module), "{label}: the mutation did not apply");
        let after = module_identity(&module).unwrap().semantic_hash;
        assert_eq!(
            after == before,
            stable,
            "{label}: the module hash did not follow the recorded rule"
        );

        let bad = lm_bytecode::encode(&module);
        let plain = lm_vm::load_bytes(&bad).is_ok();
        let cached = lm_vm::load_bytes_cached(&bad, &mut cache).is_ok();
        assert_eq!(plain, cached, "{label}: the cache changed the verdict");
    }
}

fn used_selector(m: &Module) -> Option<u32> {
    m.funcs
        .iter()
        .flat_map(|f| f.blocks.iter())
        .flatten()
        .find_map(|i| match i {
            Instr::CallVirtual { selector, .. } => Some(*selector),
            _ => None,
        })
}

fn used_string(m: &Module) -> Option<u32> {
    m.funcs
        .iter()
        .flat_map(|f| f.blocks.iter())
        .flatten()
        .find_map(|i| match i {
            Instr::ConstStr(idx) => Some(*idx),
            _ => None,
        })
}

fn used_app(m: &Module) -> Option<u32> {
    m.funcs
        .iter()
        .flat_map(|f| f.blocks.iter())
        .flatten()
        .find_map(|i| match i {
            Instr::CallG { app, .. } => Some(*app),
            _ => None,
        })
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

fn point_string(m: &mut Module, from: u32, to: u32) -> bool {
    let mut done = false;
    for block in m.funcs.iter_mut().flat_map(|f| f.blocks.iter_mut()) {
        for instr in block {
            if let Instr::ConstStr(idx) = instr {
                if *idx == from {
                    *idx = to;
                    done = true;
                }
            }
        }
    }
    done
}

fn point_app(m: &mut Module, from: u32, to: u32) -> bool {
    let mut done = false;
    for block in m.funcs.iter_mut().flat_map(|f| f.blocks.iter_mut()) {
        for instr in block {
            if let Instr::CallG { app, .. } = instr {
                if *app == from {
                    *app = to;
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

/// Review regression: two classes with the same name also share a
/// definition hash, because a class hash covers structure only. The
/// cache key must not rest on the semantic hash, or retargeting `New`
/// between them rides a hit. This case survives a nominal class hash,
/// so the cache key is the load-bearing fix, not the hash domain.
#[test]
fn a_duplicate_class_name_cannot_use_a_cache_hit() {
    let source = "class A\n  x: Int = 0\nend\nclass B\n  x: Int = 0\nend\na = A()\na.x\n";
    let bytes = compile_to_bytes("t.lm", source).unwrap();
    let mut module = lm_bytecode::decode(&bytes).unwrap();
    let a = module.classes.iter().position(|c| c.name == "A").unwrap() as u32;
    let b = module.classes.iter().position(|c| c.name == "B").unwrap() as u32;

    // The baseline: two classes both named `A`. This module is valid.
    module.classes[b as usize].name = "A".to_string();
    let baseline = lm_bytecode::encode(&module);
    let mut cache = lm_vm::VerifiedCache::new();
    lm_vm::load_bytes_cached(&baseline, &mut cache).expect("loads");
    let before = module_identity(&module).unwrap().semantic_hash;

    // Retarget `New A` to the second class named `A`.
    assert!(point_new(&mut module, a, b));
    let after = module_identity(&module).unwrap().semantic_hash;
    assert_eq!(after, before, "the retarget must keep the semantic hash");

    let bad = lm_bytecode::encode(&module);
    let plain = lm_vm::load_bytes(&bad).is_ok();
    let cached = lm_vm::load_bytes_cached(&bad, &mut cache).is_ok();
    assert!(!plain, "the uncached load accepted the retarget");
    assert_eq!(plain, cached, "the cache changed the verdict");
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
    let bytes = compile_to_bytes("t.lm", source).unwrap();
    let module = lm_bytecode::decode(&bytes).unwrap();
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

/// Definition names live outside the semantic region, and week 5
/// kept them out of the verification hash. Week 6 puts them in: the
/// verifier reads the identity-resolved core layout, and identity
/// reads the names. A rename therefore costs a cache hit now, which
/// is the price of the admission invariant. See
/// `week6_names.rs` for the attack this closes.
#[test]
fn a_rename_moves_the_verification_hash() {
    use lm_bytecode::identity::verification_hash;
    let before = "def helper(n: Int): Int\n  n * 2\nend\n\
                  def caller(n: Int): Int\n  helper(n) + 1\nend\ncaller(3)\n";
    let after = "def assist(n: Int): Int\n  n * 2\nend\n\
                 def caller(n: Int): Int\n  assist(n) + 1\nend\ncaller(3)\n";
    let ma = compile_text("t.lm", before).unwrap();
    let mb = compile_text("t.lm", after).unwrap();
    assert_ne!(
        module_identity(&ma).unwrap().semantic_hash,
        module_identity(&mb).unwrap().semantic_hash,
        "a rename must move the semantic hash"
    );
    assert_ne!(
        verification_hash(&ma),
        verification_hash(&mb),
        "a rename must move the verification hash"
    );
    // The semantic region itself does not move: the names live in the
    // export section.
    assert_eq!(
        lm_bytecode::semantic_section(&ma),
        lm_bytecode::semantic_section(&mb),
        "a rename must not move the semantic region"
    );
}
