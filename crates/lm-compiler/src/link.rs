//! The pure linker.
//!
//! Linking merges the modules of one program into a single artifact
//! with an empty import table. It installs no global name, performs
//! no host operation, and reads no file: it takes decoded modules and
//! returns a decoded module.
//!
//! Three rules do the work.
//!
//! - **Slot resolution.** An import slot names a providing module and
//!   an export. The linker finds that export and rejects a provider
//!   whose interface hash differs from the pinned one.
//! - **Class merging.** The linker compares two classes on their
//!   qualified key and their structural hash, and it implements all
//!   four rows of the class table in specification 3.6. This is how
//!   the embedded core copies of every module become one core: a core
//!   value that crosses a module boundary keeps its class.
//! - **Function code sharing and binding resolution.** Two functions
//!   with one structural hash are one function value. A named binding
//!   maps a qualified name to a function value, and the linker keeps
//!   every binding. One binding key with two structural hashes is a
//!   rejection: two providers of one name never coexist in silence.
//! - **Relocation.** Every module-global index is renumbered into the
//!   merged tables. Strings, types, selectors, and applications
//!   intern by content, so the merged tables stay canonical.
//!
//! The merged module passes the whole verifier before it runs. A
//! wrong pin or a wrong resolution therefore produces no executable
//! code that the verifier did not admit.

use crate::env::FrozenLinkEnv;
use lm_bytecode::identity::{module_identity, ModuleIdentity};
use lm_bytecode::interface::Interface;
use lm_bytecode::{
    BcClass, BcRow, BcType, Func, Import, ImportKind, Instr, Module, TypeApp, NO_PARENT,
};
use std::collections::HashMap;

/// A link failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkError(pub String);

impl std::fmt::Display for LinkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "link error: {}", self.0)
    }
}

fn fail(message: impl Into<String>) -> LinkError {
    LinkError(message.into())
}

/// One linked program: the merged module and its container bytes.
#[derive(Debug, Clone)]
pub struct LinkedProgram {
    pub module: Module,
    pub artifact: Vec<u8>,
    pub semantic_hash: [u8; 32],
    pub container_hash: [u8; 32],
}

impl LinkedProgram {
    /// The typed entry of the program: the parameter types, the
    /// result type, and the effect row of the entry function.
    ///
    /// `LinkedEntry` is the typed handle specification 3.6 requests.
    /// A caller states the expected shape and receives an error when
    /// the program does not match.
    pub fn entry(&self) -> LinkedEntry<'_> {
        let func = &self.module.funcs[self.module.entry as usize];
        LinkedEntry {
            module: &self.module,
            func,
        }
    }
}

/// A typed handle on the program entry.
pub struct LinkedEntry<'a> {
    module: &'a Module,
    func: &'a Func,
}

impl LinkedEntry<'_> {
    /// The result type of the entry, as an index into the module type
    /// table.
    pub fn result_type(&self) -> u32 {
        self.func.ret
    }

    /// The effect row the entry charges.
    pub fn row(&self) -> &[BcRow] {
        &self.func.row
    }

    /// Check the entry against an expected result type and row. The
    /// row names operations by manifest name.
    pub fn expect(&self, result: &BcType, row: &[&str]) -> Result<(), LinkError> {
        let found = &self.module.types[self.func.ret as usize];
        if found != result {
            return Err(fail(format!(
                "the entry returns {found:?}, and the caller expects {result:?}"
            )));
        }
        let mut names: Vec<&str> = self
            .func
            .row
            .iter()
            .map(|elem| match elem {
                BcRow::Op(idx) => self.module.strings[*idx as usize].as_str(),
                BcRow::Var(_) => "?",
            })
            .collect();
        names.sort_unstable();
        let mut want: Vec<&str> = row.to_vec();
        want.sort_unstable();
        if names != want {
            return Err(fail(format!(
                "the entry charges [{}], and the caller expects [{}]",
                names.join(", "),
                want.join(", ")
            )));
        }
        Ok(())
    }
}

/// The merged tables of one link step.
struct Merged {
    strings: Vec<String>,
    string_index: HashMap<String, u32>,
    types: Vec<BcType>,
    type_index: HashMap<BcType, u32>,
    selectors: Vec<String>,
    selector_index: HashMap<String, u32>,
    apps: Vec<TypeApp>,
    app_index: HashMap<TypeApp, u32>,
    classes: Vec<BcClass>,
    funcs: Vec<Func>,
    /// The merged core role table. Every module carries the same core,
    /// so every module fills the same roles with the same merged
    /// classes.
    core_roles: [u32; lm_bytecode::CORE_ROLE_COUNT],
    /// (qualified key, structural hash) to merged class index. The
    /// pair is the whole merge rule for a class.
    class_by_def: HashMap<(String, [u8; 32]), u32>,
    /// The one structural hash every qualified key carries, and the
    /// module that first provided it. A second version of one key is
    /// a rejection, and the message names both providers.
    class_version: HashMap<String, ([u8; 32], String)>,
    /// Structural hash to merged function index. A function value is
    /// identified by its structural hash, so content alone decides
    /// which code objects merge.
    func_by_hash: HashMap<[u8; 32], u32>,
    /// The named function bindings of the merged program, in link
    /// order. Several bindings may name one function value.
    bindings: Vec<lm_bytecode::FuncBinding>,
    /// The one structural hash every binding key carries, and the
    /// module that first provided it. A second version of one key is a
    /// rejection, and the message names both providers.
    binding_version: HashMap<String, ([u8; 32], String)>,
    /// (module path, export name) to the merged class index.
    class_exports: HashMap<(String, String), u32>,
    /// (module path, export name) to the merged function index.
    func_exports: HashMap<(String, String), u32>,
    /// (module path, class export name) to the merged constructor.
    ctor_exports: HashMap<(String, String), u32>,
    /// The interface hash of every export, for the pin check.
    export_hash: HashMap<(String, String), [u8; 32]>,
}

impl Default for Merged {
    fn default() -> Merged {
        Merged {
            strings: Vec::new(),
            string_index: HashMap::new(),
            types: Vec::new(),
            type_index: HashMap::new(),
            selectors: Vec::new(),
            selector_index: HashMap::new(),
            apps: Vec::new(),
            app_index: HashMap::new(),
            classes: Vec::new(),
            funcs: Vec::new(),
            core_roles: [lm_bytecode::NO_ROLE; lm_bytecode::CORE_ROLE_COUNT],
            class_by_def: HashMap::new(),
            class_version: HashMap::new(),
            func_by_hash: HashMap::new(),
            bindings: Vec::new(),
            binding_version: HashMap::new(),
            class_exports: HashMap::new(),
            func_exports: HashMap::new(),
            ctor_exports: HashMap::new(),
            export_hash: HashMap::new(),
        }
    }
}

impl Merged {
    fn string(&mut self, text: &str) -> u32 {
        if let Some(idx) = self.string_index.get(text) {
            return *idx;
        }
        let idx = self.strings.len() as u32;
        self.strings.push(text.to_string());
        self.string_index.insert(text.to_string(), idx);
        idx
    }

    fn selector(&mut self, name: &str) -> u32 {
        if let Some(idx) = self.selector_index.get(name) {
            return *idx;
        }
        let idx = self.selectors.len() as u32;
        self.selectors.push(name.to_string());
        self.selector_index.insert(name.to_string(), idx);
        idx
    }

    fn ty(&mut self, ty: BcType) -> u32 {
        if let Some(idx) = self.type_index.get(&ty) {
            return *idx;
        }
        let idx = self.types.len() as u32;
        self.types.push(ty.clone());
        self.type_index.insert(ty, idx);
        idx
    }

    fn app(&mut self, app: TypeApp) -> u32 {
        if let Some(idx) = self.app_index.get(&app) {
            return *idx;
        }
        let idx = self.apps.len() as u32;
        self.apps.push(app.clone());
        self.app_index.insert(app, idx);
        idx
    }
}

/// One module's relocation maps.
struct Reloc {
    strings: Vec<u32>,
    types: Vec<u32>,
    selectors: Vec<u32>,
    apps: Vec<u32>,
    classes: Vec<u32>,
    funcs: Vec<u32>,
}

/// Link one program from its root module.
///
/// The order is a topological order of the import graph: a module
/// links after every module it imports.
pub fn link(root: &str, env: &FrozenLinkEnv) -> Result<LinkedProgram, LinkError> {
    let order = link_order(root, env)?;
    let mut merged = Merged::default();
    let mut entry: Option<u32> = None;
    for path in &order {
        let unit = env
            .unit(path)
            .ok_or_else(|| fail(format!("the module `{path}` is not bound")))?;
        let identity = module_identity(&unit.module)
            .map_err(|e| fail(format!("the module `{path}` does not hash: {e}")))?;
        let reloc = relocate(&mut merged, &unit.module, &identity, path)?;
        register_exports(&mut merged, &unit.module, &unit.interface, path, &reloc)?;
        if path == root {
            entry = Some(reloc.funcs[unit.module.entry as usize]);
        }
    }
    let entry = entry.ok_or_else(|| fail("the root module is not bound"))?;
    let module = Module {
        strings: merged.strings,
        types: merged.types,
        selectors: merged.selectors,
        apps: merged.apps,
        imports: Vec::new(),
        core_roles: merged.core_roles,
        classes: merged.classes,
        funcs: merged.funcs,
        entry,
        exports: Vec::new(),
        bindings: merged.bindings,
    };
    // The merged program meets the whole verifier before it runs.
    lm_verify::verify_module(&module)
        .map_err(|e| fail(format!("the linked program does not verify: {e}")))?;
    let artifact = lm_bytecode::encode(&module);
    let container_hash = lm_bytecode::identity::container_hash(&artifact);
    let semantic_hash = module_identity(&module)
        .map_err(|e| fail(format!("the linked program does not hash: {e}")))?
        .semantic_hash;
    Ok(LinkedProgram {
        module,
        artifact,
        semantic_hash,
        container_hash,
    })
}

/// The link order: every imported module before its importer. The
/// walk rejects a cycle and an unbound module.
fn link_order(root: &str, env: &FrozenLinkEnv) -> Result<Vec<String>, LinkError> {
    let mut order: Vec<String> = Vec::new();
    let mut done: Vec<String> = Vec::new();
    let mut path: Vec<String> = Vec::new();
    // An explicit stack keeps the walk off the host stack.
    let mut stack: Vec<(String, bool)> = vec![(root.to_string(), false)];
    while let Some((name, expanded)) = stack.pop() {
        if done.contains(&name) {
            continue;
        }
        if expanded {
            path.retain(|p| *p != name);
            done.push(name.clone());
            order.push(name);
            continue;
        }
        let unit = env
            .unit(&name)
            .ok_or_else(|| fail(format!("the module `{name}` is not bound")))?;
        path.push(name.clone());
        stack.push((name.clone(), true));
        let mut needs: Vec<&str> = unit
            .module
            .imports
            .iter()
            .map(|i| i.module.as_str())
            .collect();
        needs.sort_unstable();
        needs.dedup();
        for need in needs {
            if path.iter().any(|p| p == need) {
                return Err(fail(format!(
                    "the modules `{name}` and `{need}` import each other"
                )));
            }
            if !done.iter().any(|p| p == need) {
                stack.push((need.to_string(), false));
            }
        }
    }
    Ok(order)
}

/// Relocate one module into the merged tables.
fn relocate(
    merged: &mut Merged,
    module: &Module,
    identity: &ModuleIdentity,
    path: &str,
) -> Result<Reloc, LinkError> {
    check_ctor_bindings(module, path)?;
    let strings: Vec<u32> = module.strings.iter().map(|s| merged.string(s)).collect();
    let selectors: Vec<u32> = module
        .selectors
        .iter()
        .map(|s| merged.selector(s))
        .collect();
    // The class map first: a type may name a class, and an imported
    // class resolves to a definition another module provides.
    let extern_funcs = module.extern_funcs();
    let mut classes: Vec<u32> = vec![u32::MAX; module.classes.len()];
    for (idx, import) in module.imports.iter().enumerate() {
        if import.kind != ImportKind::Class {
            continue;
        }
        let target = resolve_class_import(merged, import, path, idx)?;
        classes[import.def as usize] = target;
    }
    // Types reference only earlier types, so one ascending pass is
    // enough. A class reference of a local class resolves below.
    let mut types: Vec<u32> = vec![u32::MAX; module.types.len()];
    let mut apps: Vec<u32> = vec![u32::MAX; module.apps.len()];
    // The local classes take merged indices in ascending order, so a
    // parent keeps a lower index than its child. A class body needs
    // the type map, and a type may name a class. The indices are
    // therefore assigned first, and the bodies fill after the types
    // relocate.
    let mut created_classes: Vec<u32> = Vec::new();
    let mut shared_classes: Vec<u32> = Vec::new();
    for idx in 0..module.classes.len() as u32 {
        if classes[idx as usize] != u32::MAX {
            continue;
        }
        let source = &module.classes[idx as usize];
        let hash = identity.class_hashes[idx as usize];
        // The merge table of specification 3.6. One qualified key
        // carries one structural hash inside one program.
        match merged.class_version.get(&source.key) {
            Some((seen, provider)) if *seen != hash => {
                return Err(fail(format!(
                    "the class `{}` arrives with two implementations, from `{provider}` and \
                     from `{path}`; rebuild both against one version",
                    source.key
                )));
            }
            Some(_) => {}
            None => {
                merged
                    .class_version
                    .insert(source.key.clone(), (hash, path.to_string()));
            }
        }
        let key = (source.key.clone(), hash);
        match merged.class_by_def.get(&key) {
            Some(existing) => {
                classes[idx as usize] = *existing;
                shared_classes.push(idx);
            }
            None => {
                let at = merged.classes.len() as u32;
                merged.classes.push(BcClass {
                    name: source.name.clone(),
                    key: source.key.clone(),
                    parent: NO_PARENT,
                    parent_args: Vec::new(),
                    type_params: source.type_params,
                    kind: source.kind,
                    fields: Vec::new(),
                    methods: Vec::new(),
                });
                merged.class_by_def.insert(key, at);
                classes[idx as usize] = at;
                created_classes.push(idx);
            }
        }
    }
    for (idx, ty) in module.types.iter().enumerate() {
        let relocated = reloc_type(ty, &types, &classes, &strings);
        types[idx] = merged.ty(relocated);
    }
    for (idx, app) in module.apps.iter().enumerate() {
        let relocated = TypeApp {
            types: app.types.iter().map(|t| types[*t as usize]).collect(),
            rows: app
                .rows
                .iter()
                .map(|row| reloc_row(row, &strings))
                .collect(),
        };
        apps[idx] = merged.app(relocated);
    }
    // The function map: an imported function resolves to a provided
    // definition, a local function shares a merged entry by hash.
    let mut funcs: Vec<u32> = vec![u32::MAX; module.funcs.len()];
    for (idx, import) in module.imports.iter().enumerate() {
        if import.kind == ImportKind::Class {
            continue;
        }
        let target = resolve_func_import(merged, module, import, path, idx, &selectors)?;
        funcs[import.def as usize] = target;
    }
    let mut created_funcs: Vec<u32> = Vec::new();
    let mut shared_funcs: Vec<u32> = Vec::new();
    for idx in 0..module.funcs.len() {
        if extern_funcs[idx] {
            continue;
        }
        let hash = identity.func_hashes[idx];
        match merged.func_by_hash.get(&hash) {
            Some(existing) => {
                funcs[idx] = *existing;
                shared_funcs.push(idx as u32);
            }
            None => {
                let at = merged.funcs.len() as u32;
                // The placeholder keeps the index; the body fills
                // below, once every function index is known.
                merged.funcs.push(Func {
                    name: module.funcs[idx].name.clone(),
                    type_params: 0,
                    effect_params: 0,
                    params: Vec::new(),
                    param_muts: Vec::new(),
                    ret: 0,
                    row: Vec::new(),
                    captures: Vec::new(),
                    local_types: Vec::new(),
                    blocks: Vec::new(),
                });
                merged.func_by_hash.insert(hash, at);
                funcs[idx] = at;
                created_funcs.push(idx as u32);
            }
        }
    }
    let reloc = Reloc {
        strings,
        types,
        selectors,
        apps,
        classes,
        funcs,
    };
    // The core role table: every module carries the same core, and the
    // merge keeps one class per core key, so every module must name
    // the same merged class for a role.
    for (role, slot) in module.core_roles.iter().enumerate() {
        if *slot == lm_bytecode::NO_ROLE {
            continue;
        }
        if *slot as usize >= reloc.classes.len() {
            return Err(fail(format!(
                "the core role {role} of `{path}` names a class outside the module"
            )));
        }
        let target = reloc.classes[*slot as usize];
        if merged.core_roles[role] == lm_bytecode::NO_ROLE {
            merged.core_roles[role] = target;
        } else if merged.core_roles[role] != target {
            return Err(fail(format!(
                "the module `{path}` fills the core role {role} with another class; \
                 rebuild the program against one core"
            )));
        }
    }
    // Fill the created definitions, and prove that every shared one
    // really is the definition its hash claims.
    for idx in created_classes.iter().chain(shared_classes.iter()) {
        let source = &module.classes[*idx as usize];
        let at = reloc.classes[*idx as usize] as usize;
        let filled = BcClass {
            name: source.name.clone(),
            key: source.key.clone(),
            parent: match source.parent() {
                None => NO_PARENT,
                Some(p) => reloc.classes[p as usize],
            },
            parent_args: source
                .parent_args
                .iter()
                .map(|t| reloc.types[*t as usize])
                .collect(),
            type_params: source.type_params,
            kind: source.kind,
            fields: source
                .fields
                .iter()
                .map(|(name, ty)| (name.clone(), reloc.types[*ty as usize]))
                .collect(),
            methods: source
                .methods
                .iter()
                .map(|(sel, func)| (reloc.selectors[*sel as usize], reloc.funcs[*func as usize]))
                .collect(),
        };
        if created_classes.contains(idx) {
            merged.classes[at] = filled;
        } else if merged.classes[at] != filled {
            return Err(fail(format!(
                "the class `{}` of `{path}` shares a qualified key and a structural \
                 hash with a different definition",
                source.key
            )));
        }
    }
    for idx in created_funcs.iter().chain(shared_funcs.iter()) {
        let source = &module.funcs[*idx as usize];
        let at = reloc.funcs[*idx as usize] as usize;
        let filled = reloc_func(source, &reloc);
        if created_funcs.contains(idx) {
            merged.funcs[at] = filled;
        } else if !same_body(&merged.funcs[at], &filled) {
            return Err(fail(format!(
                "the function `{}` of `{path}` shares a definition hash with a \
                 different definition",
                source.name
            )));
        }
    }
    merge_bindings(merged, module, identity, path, &reloc)?;
    Ok(reloc)
}

/// Prove that every class a module defines declares its constructor
/// binding under the qualified key of that class.
///
/// The rule ties the two export-section tables together. A module that
/// names a class one way and its constructor another way is not
/// self-consistent, and the constructor merge rule would then read no
/// class key at all. The compiler derives the binding from the key, so
/// every module it writes passes.
fn check_ctor_bindings(module: &Module, path: &str) -> Result<(), LinkError> {
    let extern_classes = module.extern_classes();
    let extern_funcs = module.extern_funcs();
    // One pass over the bindings fills the constructor of every class,
    // so the whole check stays linear in the module.
    //
    // A key alone proves nothing. A key check on its own let a binding
    // name any function of the module, an import slot included, so two
    // providers hid two constructors behind one binding hash and the
    // conflict rule never fired.
    let mut ctor_of: Vec<Option<u32>> = vec![None; module.classes.len()];
    for binding in &module.bindings {
        if binding.class == lm_bytecode::NO_CLASS {
            continue;
        }
        // A hand-built module reaches the linker without a decoder, so
        // the lookup is defensive.
        let Some(class) = module.classes.get(binding.class as usize) else {
            return Err(fail(format!(
                "the binding `{}` of `{path}` names a class outside the module",
                binding.key
            )));
        };
        let idx = binding.class as usize;
        if extern_classes[idx] {
            return Err(fail(format!(
                "the module `{path}` binds the constructor `{}` to the imported \
                 class `{}`, which it does not define; rebuild the module",
                binding.key, class.key
            )));
        }
        let want = lm_bytecode::ctor_binding_key(&class.key);
        if binding.key != want {
            return Err(fail(format!(
                "the module `{path}` binds `{}` to the class `{}`, which needs \
                 the key `{want}`; rebuild the module",
                binding.key, class.key
            )));
        }
        if extern_funcs[binding.func as usize] {
            return Err(fail(format!(
                "the module `{path}` binds the constructor `{}` to an imported \
                 declaration, which carries no body; rebuild the module",
                binding.key
            )));
        }
        if ctor_of[idx].is_some() {
            return Err(fail(format!(
                "the module `{path}` declares two constructor bindings for the \
                 class `{}`; rebuild the module",
                class.key
            )));
        }
        ctor_of[idx] = Some(binding.func);
    }
    // Every class this module defines declares one constructor binding.
    for (idx, class) in module.classes.iter().enumerate() {
        if extern_classes[idx] || ctor_of[idx].is_some() {
            continue;
        }
        return Err(fail(format!(
            "the module `{path}` defines the class `{}` and declares no \
             constructor binding `{}`; rebuild the module",
            class.key,
            lm_bytecode::ctor_binding_key(&class.key)
        )));
    }
    // One pass over the exports. The export table records the
    // construction function of a class export, and the two must name
    // one function, or a caller reaches a constructor the binding
    // never covered.
    for export in &module.exports {
        if !export.kind.is_class() || export.ctor == lm_bytecode::NO_CTOR {
            continue;
        }
        let Some(class) = module.classes.get(export.def as usize) else {
            return Err(fail(format!(
                "the export `{}` of `{path}` names a class outside the module",
                export.name
            )));
        };
        let idx = export.def as usize;
        if ctor_of[idx] != Some(export.ctor) {
            return Err(fail(format!(
                "the module `{path}` exports the class `{}` with one \
                 construction function and binds `{}` to another; rebuild \
                 the module",
                class.key,
                lm_bytecode::ctor_binding_key(&class.key)
            )));
        }
    }
    Ok(())
}

/// Merge the named function bindings of one module (specification
/// 3.6). The table is exhaustive:
///
/// | Binding key | StructuralHash | Result |
/// | --- | --- | --- |
/// | same | same | share the binding and the code |
/// | same | different | reject: conflicting providers |
/// | different | same | keep both bindings, share the code |
/// | different | different | keep both bindings and both code objects |
///
/// Row 2 is the rule the generated constructor needs. A class
/// structural hash covers no constructor, so two providers of one
/// class key with two different constructors merge into one class.
/// Their constructors carry one binding key and two structural
/// hashes, and this rule rejects them.
fn merge_bindings(
    merged: &mut Merged,
    module: &Module,
    identity: &ModuleIdentity,
    path: &str,
    reloc: &Reloc,
) -> Result<(), LinkError> {
    let extern_funcs = module.extern_funcs();
    for binding in &module.bindings {
        let local = binding.func as usize;
        if local >= module.funcs.len() {
            return Err(fail(format!(
                "the binding `{}` of `{path}` names a function outside the module",
                binding.key
            )));
        }
        if extern_funcs[local] {
            // An imported declaration carries no body, so the module
            // that declares it is not a provider of that name. A
            // constructor binding never reaches this arm, because
            // `check_ctor_bindings` rejects one that names an import
            // slot. Without that rule a skip here switched the
            // conflict test off for the exact case it must catch.
            continue;
        }
        let hash = identity.func_hashes[local];
        match merged.binding_version.get(&binding.key) {
            Some((seen, provider)) if *seen != hash => {
                return Err(fail(format!(
                    "the function `{}` arrives with two implementations, from \
                     `{provider}` and from `{path}`; rebuild both against one version",
                    binding.key
                )));
            }
            Some(_) => continue,
            None => {
                merged
                    .binding_version
                    .insert(binding.key.clone(), (hash, path.to_string()));
            }
        }
        merged.bindings.push(lm_bytecode::FuncBinding {
            key: binding.key.clone(),
            func: reloc.funcs[local],
            class: if binding.class == lm_bytecode::NO_CLASS {
                lm_bytecode::NO_CLASS
            } else {
                reloc.classes[binding.class as usize]
            },
        });
    }
    Ok(())
}

/// Resolve one class import slot against the provided definitions.
fn resolve_class_import(
    merged: &Merged,
    import: &Import,
    path: &str,
    slot: usize,
) -> Result<u32, LinkError> {
    let key = (import.module.clone(), import.name.clone());
    check_pin(merged, import, path, slot)?;
    merged.class_exports.get(&key).copied().ok_or_else(|| {
        fail(format!(
            "`{path}` slot {slot} names the type `{}.{}`, which the module does \
             not export",
            import.module, import.name
        ))
    })
}

/// Compare the pinned interface hash with the provider export.
fn check_pin(merged: &Merged, import: &Import, path: &str, slot: usize) -> Result<(), LinkError> {
    // A method slot pins the interface hash of its class, so the
    // lookup drops the method name.
    let export_name = match import.kind {
        ImportKind::Method => import
            .name
            .rsplit_once('.')
            .map(|(class, _)| class.to_string())
            .unwrap_or_else(|| import.name.clone()),
        _ => import.name.clone(),
    };
    let key = (import.module.clone(), export_name.clone());
    let Some(found) = merged.export_hash.get(&key) else {
        return Err(fail(format!(
            "`{path}` slot {slot} names `{}.{export_name}`, which the module does \
             not export",
            import.module
        )));
    };
    if *found != import.hash {
        return Err(fail(format!(
            "`{path}` slot {slot} pins an interface of `{}.{export_name}` that the \
             module no longer provides; rebuild the importing module",
            import.module
        )));
    }
    Ok(())
}

/// Resolve one function, constructor, or method import slot.
fn resolve_func_import(
    merged: &Merged,
    module: &Module,
    import: &Import,
    path: &str,
    slot: usize,
    selectors: &[u32],
) -> Result<u32, LinkError> {
    check_pin(merged, import, path, slot)?;
    match import.kind {
        ImportKind::Func => {
            let key = (import.module.clone(), import.name.clone());
            merged.func_exports.get(&key).copied().ok_or_else(|| {
                fail(format!(
                    "`{path}` slot {slot} names the function `{}.{}`, which the \
                     module does not export",
                    import.module, import.name
                ))
            })
        }
        ImportKind::Ctor => {
            let key = (import.module.clone(), import.name.clone());
            merged.ctor_exports.get(&key).copied().ok_or_else(|| {
                fail(format!(
                    "`{path}` slot {slot} names the constructor of `{}.{}`, which \
                     the module does not export",
                    import.module, import.name
                ))
            })
        }
        ImportKind::Method => {
            let Some((class_name, method)) = import.name.rsplit_once('.') else {
                return Err(fail(format!(
                    "`{path}` slot {slot} names the method `{}` without a class",
                    import.name
                )));
            };
            let key = (import.module.clone(), class_name.to_string());
            let class = merged.class_exports.get(&key).copied().ok_or_else(|| {
                fail(format!(
                    "`{path}` slot {slot} names a method of `{}.{class_name}`, \
                     which the module does not export",
                    import.module
                ))
            })?;
            // The local selector table holds the method name, so the
            // merged selector index comes from the relocation map.
            let local = module
                .selectors
                .iter()
                .position(|s| s == method)
                .ok_or_else(|| {
                    fail(format!(
                        "`{path}` slot {slot} names the method `{method}`, which \
                         the module does not call"
                    ))
                })?;
            let selector = selectors[local];
            merged.classes[class as usize]
                .methods
                .iter()
                .find(|(sel, _)| *sel == selector)
                .map(|(_, func)| *func)
                .ok_or_else(|| {
                    fail(format!(
                        "`{path}` slot {slot} names the method `{method}`, which \
                         `{}.{class_name}` does not answer",
                        import.module
                    ))
                })
        }
        ImportKind::Class => unreachable!("a class slot never reaches the function map"),
    }
}

/// Record the exports of one module for the modules that follow.
fn register_exports(
    merged: &mut Merged,
    module: &Module,
    interface: &Interface,
    path: &str,
    reloc: &Reloc,
) -> Result<(), LinkError> {
    let extern_classes = module.extern_classes();
    let extern_funcs = module.extern_funcs();
    for export in &module.exports {
        let key = (path.to_string(), export.name.clone());
        if merged.export_hash.contains_key(&key) {
            return Err(fail(format!(
                "the module `{path}` exports the name `{}` twice",
                export.name
            )));
        }
        // The decoder bounds these indices, and a hand-built module
        // reaches the linker without a decoder, so the bound is
        // checked here too.
        let limit = if export.kind.is_class() {
            reloc.classes.len()
        } else {
            reloc.funcs.len()
        };
        if export.def as usize >= limit
            || (export.ctor != lm_bytecode::NO_CTOR && export.ctor as usize >= reloc.funcs.len())
        {
            return Err(fail(format!(
                "the export `{}` of `{path}` names a definition outside the \
                 module",
                export.name
            )));
        }
        // A module exports what it defines. A re-export of an
        // imported declaration would give one definition two
        // qualified names, and a pin would then name a module that
        // does not hold the definition.
        let imported = if export.kind.is_class() {
            extern_classes[export.def as usize]
        } else {
            extern_funcs[export.def as usize]
        };
        if imported {
            return Err(fail(format!(
                "the module `{path}` exports `{}`, which it imports",
                export.name
            )));
        }
        let entry = interface.find(&export.name).ok_or_else(|| {
            fail(format!(
                "the interface of `{path}` does not describe the export `{}`",
                export.name
            ))
        })?;
        merged.export_hash.insert(key.clone(), entry.iface_hash);
        if export.kind.is_class() {
            merged
                .class_exports
                .insert(key.clone(), reloc.classes[export.def as usize]);
            if export.ctor != lm_bytecode::NO_CTOR {
                merged
                    .ctor_exports
                    .insert(key, reloc.funcs[export.ctor as usize]);
            }
        } else {
            merged
                .func_exports
                .insert(key, reloc.funcs[export.def as usize]);
        }
    }
    Ok(())
}

/// Compare two relocated functions without their names.
///
/// A function definition hash excludes the name by design, so two
/// functions that differ only in the name are one definition. The
/// generated constructor stubs of the abstract enum parents are the
/// common case: every one is `unit; return`.
fn same_body(a: &Func, b: &Func) -> bool {
    let mut left = a.clone();
    let mut right = b.clone();
    left.name.clear();
    right.name.clear();
    left == right
}

fn reloc_row(row: &[BcRow], strings: &[u32]) -> Vec<BcRow> {
    row.iter()
        .map(|elem| match elem {
            BcRow::Op(idx) => BcRow::Op(strings[*idx as usize]),
            BcRow::Var(v) => BcRow::Var(*v),
        })
        .collect()
}

fn reloc_type(ty: &BcType, types: &[u32], classes: &[u32], strings: &[u32]) -> BcType {
    match ty {
        BcType::Class(c) => BcType::Class(classes[*c as usize]),
        BcType::Inst(c, args) => BcType::Inst(
            classes[*c as usize],
            args.iter().map(|a| types[*a as usize]).collect(),
        ),
        BcType::List(e) => BcType::List(types[*e as usize]),
        BcType::Map(k, v) => BcType::Map(types[*k as usize], types[*v as usize]),
        BcType::Tuple(elems) => BcType::Tuple(elems.iter().map(|e| types[*e as usize]).collect()),
        BcType::Fn(params, muts, ret, row) => BcType::Fn(
            params.iter().map(|p| types[*p as usize]).collect(),
            muts.clone(),
            types[*ret as usize],
            reloc_row(row, strings),
        ),
        BcType::Vm(t) => BcType::Vm(types[*t as usize]),
        BcType::PendingCall(a, r) => BcType::PendingCall(types[*a as usize], types[*r as usize]),
        BcType::Handle(m, r) => BcType::Handle(types[*m as usize], types[*r as usize]),
        BcType::Op(op, f) => BcType::Op(*op, types[*f as usize]),
        other => other.clone(),
    }
}

fn reloc_func(func: &Func, reloc: &Reloc) -> Func {
    Func {
        name: func.name.clone(),
        type_params: func.type_params,
        effect_params: func.effect_params,
        params: func
            .params
            .iter()
            .map(|t| reloc.types[*t as usize])
            .collect(),
        param_muts: func.param_muts.clone(),
        ret: reloc.types[func.ret as usize],
        row: reloc_row(&func.row, &reloc.strings),
        captures: func
            .captures
            .iter()
            .map(|t| reloc.types[*t as usize])
            .collect(),
        local_types: func
            .local_types
            .iter()
            .map(|t| reloc.types[*t as usize])
            .collect(),
        blocks: func
            .blocks
            .iter()
            .map(|block| block.iter().map(|i| reloc_instr(i, reloc)).collect())
            .collect(),
    }
}

/// Relocate one instruction. The match is exhaustive without a
/// wildcard arm, so a future instruction with a module-global operand
/// fails to compile until its relocation is decided.
fn reloc_instr(instr: &Instr, reloc: &Reloc) -> Instr {
    match instr {
        Instr::ConstStr(idx) => Instr::ConstStr(reloc.strings[*idx as usize]),
        Instr::Call(f) => Instr::Call(reloc.funcs[*f as usize]),
        Instr::CallG { func, app } => Instr::CallG {
            func: reloc.funcs[*func as usize],
            app: reloc.apps[*app as usize],
        },
        Instr::CallVirtual { selector, argc } => Instr::CallVirtual {
            selector: reloc.selectors[*selector as usize],
            argc: *argc,
        },
        Instr::CallVirtualG {
            selector,
            argc,
            app,
        } => Instr::CallVirtualG {
            selector: reloc.selectors[*selector as usize],
            argc: *argc,
            app: reloc.apps[*app as usize],
        },
        Instr::MakeClosure { func, captures } => Instr::MakeClosure {
            func: reloc.funcs[*func as usize],
            captures: *captures,
        },
        Instr::New(c) => Instr::New(reloc.classes[*c as usize]),
        Instr::NewG { class, app } => Instr::NewG {
            class: reloc.classes[*class as usize],
            app: reloc.apps[*app as usize],
        },
        Instr::TupleNew { ty, count } => Instr::TupleNew {
            ty: reloc.types[*ty as usize],
            count: *count,
        },
        Instr::ListNew { ty, count } => Instr::ListNew {
            ty: reloc.types[*ty as usize],
            count: *count,
        },
        Instr::MapNew { ty, count } => Instr::MapNew {
            ty: reloc.types[*ty as usize],
            count: *count,
        },
        Instr::IsType(ty) => Instr::IsType(reloc.types[*ty as usize]),
        Instr::CastType(ty) => Instr::CastType(reloc.types[*ty as usize]),
        // Every remaining operand is function-local or manifest-dense.
        Instr::ConstUnit
        | Instr::ConstBool(_)
        | Instr::ConstInt(_)
        | Instr::LoadLocal(_)
        | Instr::StoreLocal(_)
        | Instr::Pop
        | Instr::Add
        | Instr::Sub
        | Instr::Mul
        | Instr::Div
        | Instr::Rem
        | Instr::Neg
        | Instr::Not
        | Instr::LtInt
        | Instr::LeInt
        | Instr::GtInt
        | Instr::GeInt
        | Instr::EqInt
        | Instr::NeInt
        | Instr::EqBool
        | Instr::NeBool
        | Instr::EqStr
        | Instr::NeStr
        | Instr::EqRef
        | Instr::NeRef
        | Instr::CallValue { .. }
        | Instr::LoadCapture(_)
        | Instr::LoadField(_)
        | Instr::StoreField(_)
        | Instr::TupleGet(_)
        | Instr::ListLen
        | Instr::ListAt
        | Instr::ListPush
        | Instr::MapLen
        | Instr::MapHas
        | Instr::MapAt
        | Instr::MapPut
        | Instr::SbNew
        | Instr::SbAppendStr
        | Instr::SbAppendInt
        | Instr::SbAppendBool
        | Instr::SbBuild
        | Instr::BbNew
        | Instr::BbAppend
        | Instr::BbLen
        | Instr::BbBuild
        | Instr::Freeze
        | Instr::Digest
        | Instr::EqDigest
        | Instr::NeDigest
        | Instr::Jump(_)
        | Instr::JumpIfFalse(_)
        | Instr::JumpIfTrue(_)
        | Instr::Return
        | Instr::Perform { .. }
        | Instr::PerformValue { .. }
        | Instr::OpConst(_)
        | Instr::TableEdit { .. }
        | Instr::AsCall(_)
        | Instr::CallArgs
        | Instr::FaultCode
        | Instr::Unreachable => *instr,
    }
}
