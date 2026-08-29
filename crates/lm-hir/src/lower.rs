//! Lowering from typed HIR to basic-block bytecode.
//!
//! Each expression leaves exactly one value on the operand stack.
//! Each statement leaves the stack unchanged. Every block ends with a
//! terminator. The pass interns strings, types, selectors, and type
//! applications in first-encounter order, so the output is
//! deterministic. It also synthesizes one construction function for
//! each class, and expands `case` patterns and the non-faulting `get`
//! methods into ordinary instructions with scratch locals.

use crate::hir::*;
use lm_bytecode::{
    BcAssociated, BcCallableContract, BcClass, BcClassKind, BcConformance, BcConformancePremise,
    BcInterface, BcInterfaceMethod, BcInterfaceUse, BcRow, BcType, ExtendedInstr, Func, Instr,
    Module, SlotContract, SlotSpec, SlotTarget, TypeApp, NO_APP, NO_PARENT,
};
use lm_source::ast::BinOp;
use lm_types::{
    ClassKind, Row, RowElem, Type, TypeId, TypeStore, BOOL, DIGEST, INT, NEVER, STRING, UNIT,
};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;

fn extended(instr: ExtendedInstr) -> Instr {
    Instr::Extended(instr)
}

fn interface_call(interface: u32, method: u32, recv_ty: u32, app: u32) -> Instr {
    let site = lm_bytecode::pack_interface_call_site(interface, method)
        .expect("the checker limits interface call indices");
    Instr::CallInterface { site, recv_ty, app }
}

/// Module-wide interning state during lowering.
struct ModLowerer<'m> {
    store: &'m TypeStore,
    bundle: &'m lm_abi::AbiBundle,
    funcs: &'m [HirFunc],
    interfaces: &'m [HirInterface],
    classes: &'m [HirClass],
    /// Small expression bodies that direct calls can inline.
    inline_bodies: Vec<Option<HExpr>>,
    strings: Vec<String>,
    string_index: HashMap<String, u32>,
    bytes: Vec<Vec<u8>>,
    byte_index: HashMap<Vec<u8>, u32>,
    types: Vec<BcType>,
    type_index: HashMap<BcType, u32>,
    selectors: Vec<String>,
    selector_index: HashMap<String, u32>,
    apps: Vec<TypeApp>,
    app_index: HashMap<(Vec<u32>, Vec<Vec<BcRow>>), u32>,
    /// The function index of the first synthesized `<new>` function.
    new_base: u32,
    /// Pinned core indices for the `get` expansions.
    core: CoreIds,
    /// The nominal class of native string builders.
    string_builder_class: u32,
    /// Dense late-call slot by function index.
    function_slots: HashMap<u32, u32>,
    /// Dense late-allocation slot by class index.
    class_slots: HashMap<u32, u32>,
    /// Local dispatch function for each late class constructor.
    class_dispatch: HashMap<u32, u32>,
    /// Operand counts for the published slots, in slot order.
    slot_param_counts: Vec<usize>,
    /// Operand counts for direct-call targets, in function order.
    func_param_counts: Vec<usize>,
}

/// The callable kind of one late function reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LateCallableKind {
    Function,
    Method,
}

/// One late callable selected before bytecode lowering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LateCallable {
    pub binding: String,
    pub late: bool,
    pub key: [u8; 32],
    pub contract_hash: [u8; 32],
    pub kind: LateCallableKind,
}

/// One published class binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LateClass {
    pub binding: String,
    pub late: bool,
    pub key: [u8; 32],
    pub abi: [u8; 32],
}

/// Late-linkage choices for one checked module.
#[derive(Debug, Clone, Default)]
pub struct LowerLinkage {
    /// Every callable binding that the artifact publishes.
    pub functions: BTreeMap<u32, LateCallable>,
    /// Every class binding that the artifact publishes.
    pub classes: BTreeMap<u32, LateClass>,
    /// Published callables that compiled calls must read through slots.
    pub dynamic_functions: BTreeSet<u32>,
    /// Published classes that compiled construction must read through slots.
    pub dynamic_classes: BTreeSet<u32>,
}

impl<'m> ModLowerer<'m> {
    fn intern_string(&mut self, value: &str) -> u32 {
        if let Some(idx) = self.string_index.get(value) {
            return *idx;
        }
        let idx = self.strings.len() as u32;
        self.strings.push(value.to_string());
        self.string_index.insert(value.to_string(), idx);
        idx
    }

    fn intern_bytes(&mut self, value: &[u8]) -> u32 {
        if let Some(idx) = self.byte_index.get(value) {
            return *idx;
        }
        let idx = self.bytes.len() as u32;
        let owned = value.to_vec();
        self.bytes.push(owned.clone());
        self.byte_index.insert(owned, idx);
        idx
    }

    fn intern_type(&mut self, ty: BcType) -> u32 {
        if let Some(idx) = self.type_index.get(&ty) {
            return *idx;
        }
        let idx = self.types.len() as u32;
        self.types.push(ty.clone());
        self.type_index.insert(ty, idx);
        idx
    }

    /// Intern the exact callback type of one function body.
    fn callback_type(&mut self, func: u32) -> u32 {
        let (params, muts, ret, row) = {
            let target = &self.funcs[func as usize];
            (
                target.params.clone(),
                target.param_muts.clone(),
                target.ret,
                target.row.clone(),
            )
        };
        let params = params.into_iter().map(|ty| self.bc_ty(ty)).collect();
        let ret = self.bc_ty(ret);
        let row = self.bc_row(&row);
        self.intern_type(BcType::Callback(params, muts, ret, row))
    }

    /// Convert a checker row into exact ABI slots.
    fn bc_row(&mut self, row: &Row) -> Vec<BcRow> {
        let mut lowered: Vec<BcRow> = row
            .iter()
            .map(|elem| match elem {
                RowElem::Op(name) => {
                    let text = self.store.row_name(*name);
                    if let Some(operation) = self.bundle.op_by_name(text) {
                        BcRow::Op(operation)
                    } else {
                        BcRow::Group(
                            self.bundle
                                .group_by_name(text)
                                .expect("the checker accepts only ABI row names"),
                        )
                    }
                }
                RowElem::Var(v) => BcRow::Var(*v),
            })
            .collect();
        lowered.sort_unstable();
        lowered.dedup();
        lowered
    }

    fn intern_app(&mut self, types: Vec<u32>, rows: Vec<Vec<BcRow>>) -> u32 {
        let key = (types.clone(), rows.clone());
        if let Some(idx) = self.app_index.get(&key) {
            return *idx;
        }
        let idx = self.apps.len() as u32;
        self.apps.push(TypeApp { types, rows });
        self.app_index.insert(key, idx);
        idx
    }

    /// Build a type application from checker types and rows.
    fn app_of(&mut self, targs: &[TypeId], rowargs: &[Row]) -> u32 {
        let types: Vec<u32> = targs.iter().map(|t| self.bc_ty(*t)).collect();
        let rows: Vec<Vec<BcRow>> = rowargs.iter().map(|r| self.bc_row(r)).collect();
        self.intern_app(types, rows)
    }

    fn interface_use(&mut self, application: &HirInterfaceUse) -> BcInterfaceUse {
        BcInterfaceUse {
            interface: application.interface,
            types: application
                .types
                .iter()
                .map(|item| self.bc_ty(*item))
                .collect(),
            rows: application
                .rows
                .iter()
                .map(|row| self.bc_row(row))
                .collect(),
        }
    }

    fn bounds(&mut self, bounds: &[Vec<HirInterfaceUse>]) -> Vec<Vec<BcInterfaceUse>> {
        bounds
            .iter()
            .map(|items| items.iter().map(|item| self.interface_use(item)).collect())
            .collect()
    }

    /// Convert an interned checker type to a type-table index.
    /// Convert one checked type to its canonical bytecode type.
    fn bc_ty(&mut self, id: TypeId) -> u32 {
        match self.store.get(id).clone() {
            Type::Unit => self.intern_type(BcType::Unit),
            Type::Never => self.intern_type(BcType::Never),
            Type::Bool => self.intern_type(BcType::Bool),
            Type::Int => self.intern_type(BcType::Int),
            Type::Float => self.intern_type(BcType::Float),
            Type::String => self.intern_type(BcType::Str),
            Type::Bytes => self.intern_type(BcType::Bytes),
            Type::FileHandle => self.intern_type(BcType::FileHandle),
            Type::ResourceHandle => self.intern_type(BcType::ResourceHandle),
            Type::HostResource => self.intern_type(BcType::HostResource),
            Type::Digest => self.intern_type(BcType::Digest),
            Type::Class(c) => self.intern_type(BcType::Class(c.0)),
            Type::Inst(c, args) => {
                let args: Vec<u32> = args.iter().map(|a| self.bc_ty(*a)).collect();
                self.intern_type(BcType::Inst(c.0, args))
            }
            Type::List(e) => {
                let e = self.bc_ty(e);
                self.intern_type(BcType::List(e))
            }
            Type::Map(k, v) => {
                let k = self.bc_ty(k);
                let v = self.bc_ty(v);
                self.intern_type(BcType::Map(k, v))
            }
            Type::Tuple(elems) => {
                let elems: Vec<u32> = elems.iter().map(|e| self.bc_ty(*e)).collect();
                self.intern_type(BcType::Tuple(elems))
            }
            Type::Fn(params, muts, ret, row) => {
                let params: Vec<u32> = params.iter().map(|p| self.bc_ty(*p)).collect();
                let ret = self.bc_ty(ret);
                let row = self.bc_row(&row);
                self.intern_type(BcType::Fn(params, muts, ret, row))
            }
            Type::Callback(params, muts, ret, row) => {
                let params: Vec<u32> = params.iter().map(|p| self.bc_ty(*p)).collect();
                let ret = self.bc_ty(ret);
                let row = self.bc_row(&row);
                self.intern_type(BcType::Callback(params, muts, ret, row))
            }
            Type::Var(i) => self.intern_type(BcType::Var(i)),
            Type::Projection {
                base,
                interface,
                assoc,
            } => {
                let base = self.bc_ty(base);
                self.intern_type(BcType::Projection {
                    base,
                    interface: interface.0,
                    assoc,
                })
            }
            Type::Fault => self.intern_type(BcType::Fault),
            Type::Request => self.intern_type(BcType::Request),
            Type::PolicyTable => self.intern_type(BcType::PolicyTable),
            Type::Vm => self.intern_type(BcType::Vm),
            Type::VmSnapshot => self.intern_type(BcType::VmSnapshot),
            Type::RunSnapshot(t) => {
                let t = self.bc_ty(t);
                self.intern_type(BcType::RunSnapshot(t))
            }
            Type::Run(t) => {
                let t = self.bc_ty(t);
                self.intern_type(BcType::Run(t))
            }
            Type::Wait(t) => {
                let t = self.bc_ty(t);
                self.intern_type(BcType::Wait(t))
            }
            Type::PendingCall(a, r) => {
                let a = self.bc_ty(a);
                let r = self.bc_ty(r);
                self.intern_type(BcType::PendingCall(a, r))
            }
            Type::Handle(m, r) => {
                let m = self.bc_ty(m);
                let r = self.bc_ty(r);
                self.intern_type(BcType::Handle(m, r))
            }
            Type::Op(op, f) => {
                let f = self.bc_ty(f);
                self.intern_type(BcType::Op(op, f))
            }
        }
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
}

/// Lower a checked module to decoded bytecode.
pub fn lower_module(hir: &HirModule) -> Module {
    lower_module_with_linkage(hir, &LowerLinkage::default())
        .expect("static lowering has no late-linkage failure")
}

/// Lower a checked module with explicit late-linkage choices.
pub fn lower_module_with_linkage(
    hir: &HirModule,
    linkage: &LowerLinkage,
) -> Result<Module, String> {
    let mut function_slots = HashMap::new();
    let mut class_slots = HashMap::new();
    let mut slot_param_counts = Vec::new();
    let mut next_slot = 0u32;
    for function in linkage.functions.keys() {
        if linkage.dynamic_functions.contains(function) {
            function_slots.insert(*function, next_slot);
        }
        slot_param_counts.push(hir.funcs[*function as usize].params.len());
        next_slot += 1;
    }
    for class in linkage.classes.keys() {
        if linkage.dynamic_classes.contains(class) {
            class_slots.insert(*class, next_slot);
        }
        slot_param_counts.push(hir.classes[*class as usize].ctor_params.len());
        next_slot += 1;
    }
    let dispatch_base = hir.funcs.len() as u32 + hir.classes.len() as u32;
    let class_dispatch = linkage
        .dynamic_classes
        .iter()
        .enumerate()
        .map(|(offset, class)| (*class, dispatch_base + offset as u32))
        .collect();
    let mut func_param_counts: Vec<usize> = hir
        .funcs
        .iter()
        .map(|function| function.params.len())
        .collect();
    func_param_counts.extend(hir.classes.iter().map(|class| class.ctor_params.len()));
    func_param_counts.extend(
        linkage
            .dynamic_classes
            .iter()
            .map(|class| hir.classes[*class as usize].ctor_params.len()),
    );
    let mut inline_bodies: Vec<Option<HExpr>> = hir.funcs.iter().map(inline_body).collect();
    for function in &linkage.dynamic_functions {
        let body = inline_bodies
            .get_mut(*function as usize)
            .ok_or_else(|| format!("late function {function} does not exist"))?;
        *body = None;
    }
    let mut m = ModLowerer {
        store: &hir.store,
        bundle: &hir.bundle,
        funcs: &hir.funcs,
        interfaces: &hir.interfaces,
        classes: &hir.classes,
        inline_bodies,
        strings: Vec::new(),
        string_index: HashMap::new(),
        bytes: Vec::new(),
        byte_index: HashMap::new(),
        types: Vec::new(),
        type_index: HashMap::new(),
        selectors: Vec::new(),
        selector_index: HashMap::new(),
        apps: Vec::new(),
        app_index: HashMap::new(),
        new_base: hir.funcs.len() as u32,
        core: hir.core,
        string_builder_class: hir.core_roles[lm_bytecode::corepin::ROLE_STRING_BUILDER],
        function_slots,
        class_slots,
        class_dispatch,
        slot_param_counts,
        func_param_counts,
    };
    // The canonical primitive prefix required by the verifier.
    m.intern_type(BcType::Unit);
    m.intern_type(BcType::Bool);
    m.intern_type(BcType::Int);
    m.intern_type(BcType::Str);
    m.intern_type(BcType::Float);
    // Selectors in interface and class declaration order.
    for interface in &hir.interfaces {
        for method in &interface.methods {
            m.selector(&method.selector);
        }
    }
    for class in &hir.classes {
        for (name, _) in &class.methods {
            m.selector(name);
        }
    }
    let mut funcs = Vec::new();
    for func in &hir.funcs {
        funcs.push(lower_func(&mut m, func));
    }
    for (cidx, class) in hir.classes.iter().enumerate() {
        funcs.push(lower_new_func(&mut m, class, cidx as u32));
    }
    for class in &linkage.dynamic_classes {
        funcs.push(lower_new_dispatch_func(
            &mut m,
            &hir.classes[*class as usize],
            *class,
        ));
    }
    let interfaces: Vec<BcInterface> = hir
        .interfaces
        .iter()
        .map(|interface| BcInterface {
            name: interface.name.clone(),
            key: interface.key.clone(),
            type_params: interface.type_params,
            effect_params: interface.effect_params,
            generic_is_effect: interface.generic_is_effect.clone(),
            parents: interface
                .parents
                .iter()
                .map(|parent| m.interface_use(parent))
                .collect(),
            type_bounds: m.bounds(&interface.type_bounds),
            associated: interface
                .associated
                .iter()
                .map(|item| BcAssociated {
                    name: item.name.clone(),
                    bounds: item
                        .bounds
                        .iter()
                        .map(|bound| m.interface_use(bound))
                        .collect(),
                })
                .collect(),
            methods: interface
                .methods
                .iter()
                .map(|method| BcInterfaceMethod {
                    selector: m.selector(&method.selector),
                    mut_self: method.mut_self,
                    type_params: method.type_params,
                    type_bounds: m.bounds(&method.type_bounds),
                    effect_params: method.effect_params,
                    premises: method
                        .premises
                        .iter()
                        .map(|premise| lm_bytecode::BcTypePremise {
                            subject: m.bc_ty(premise.subject),
                            bounds: premise
                                .bounds
                                .iter()
                                .map(|bound| m.interface_use(bound))
                                .collect(),
                        })
                        .collect(),
                    params: method.params.iter().map(|item| m.bc_ty(*item)).collect(),
                    param_muts: method.param_muts.clone(),
                    param_names: method.param_names.clone(),
                    ret: m.bc_ty(method.ret),
                    row: m.bc_row(&method.row),
                    default: method.default.unwrap_or(lm_bytecode::NO_FUNC),
                })
                .collect(),
        })
        .collect();
    let conformances: Vec<BcConformance> = hir
        .conformances
        .iter()
        .map(|conformance| BcConformance {
            class: conformance.class,
            application: m.interface_use(&conformance.application),
            premises: conformance
                .premises
                .iter()
                .map(|premise| BcConformancePremise {
                    param: premise.param,
                    bounds: premise
                        .bounds
                        .iter()
                        .map(|bound| m.interface_use(bound))
                        .collect(),
                })
                .collect(),
            associated: conformance
                .associated
                .iter()
                .map(|item| m.bc_ty(*item))
                .collect(),
            method_overrides: conformance.method_overrides.clone(),
        })
        .collect();
    let mut func_bounds: Vec<Vec<Vec<BcInterfaceUse>>> = hir
        .funcs
        .iter()
        .map(|func| m.bounds(&func.type_bounds))
        .collect();
    for (index, class) in hir.classes.iter().enumerate() {
        let constructor = &funcs[hir.funcs.len() + index];
        if constructor.type_params == 0 {
            func_bounds.push(Vec::new());
        } else {
            func_bounds.push(m.bounds(&class.type_bounds));
        }
    }
    for class in &linkage.dynamic_classes {
        if hir.classes[*class as usize].type_params == 0 {
            func_bounds.push(Vec::new());
        } else {
            func_bounds.push(m.bounds(&hir.classes[*class as usize].type_bounds));
        }
    }
    let class_bounds = hir
        .classes
        .iter()
        .map(|class| m.bounds(&class.type_bounds))
        .collect();
    let classes: Vec<BcClass> = hir
        .classes
        .iter()
        .map(|class| BcClass {
            name: class.name.clone(),
            key: class.key.clone(),
            is_final: class.is_final,
            is_frozen: class.is_frozen,
            parent: class.parent.unwrap_or(NO_PARENT),
            parent_args: class.parent_args.iter().map(|t| m.bc_ty(*t)).collect(),
            type_params: class.type_params,
            kind: match class.kind {
                ClassKind::Normal if class.native_repr == Some(NativeRepr::Text) => {
                    BcClassKind::Abstract
                }
                ClassKind::Normal => BcClassKind::Normal,
                ClassKind::EnumParent => BcClassKind::Abstract,
                ClassKind::EnumCase => BcClassKind::Case,
            },
            fields: class
                .field_names
                .iter()
                .zip(class.field_tys.iter())
                .map(|(name, ty)| (name.clone(), m.bc_ty(*ty)))
                .collect(),
            field_defaults: class.field_defaults.clone(),
            own_start: class.own_start,
            has_init: class.init.is_some(),
            methods: class
                .methods
                .iter()
                .map(|(name, func)| (m.selector(name), *func))
                .collect(),
        })
        .collect();
    // The construction function of class `c` sits at `new_base + c`.
    let new_base = hir.funcs.len() as u32;
    for import in &hir.imports {
        if import.kind == lm_bytecode::ImportKind::Method {
            if let Some((_, method)) = import.name.rsplit_once('.') {
                m.selector(method);
            }
        }
    }
    let imports = hir
        .imports
        .iter()
        .map(|i| lm_bytecode::Import {
            module: i.module.clone(),
            name: i.name.clone(),
            kind: i.kind,
            def: match i.def {
                crate::hir::HirImportDef::Class(c) => c,
                crate::hir::HirImportDef::Func(f) => f,
                crate::hir::HirImportDef::Ctor(c) => new_base + c,
                crate::hir::HirImportDef::Constant => lm_bytecode::NO_IMPORT_DEF,
            },
            hash: i.hash,
        })
        .collect();
    let mut exports: Vec<lm_bytecode::Export> = hir
        .exports
        .iter()
        .map(|e| lm_bytecode::Export {
            kind: e.kind,
            name: e.name.clone(),
            def: e.def,
            ctor: if e.kind.is_class() {
                new_base + e.def
            } else {
                lm_bytecode::NO_CTOR
            },
            constant: None,
        })
        .collect();
    for constant in &hir.constants {
        exports.push(lm_bytecode::Export {
            kind: lm_bytecode::ExportKind::Constant,
            name: constant.name.clone(),
            def: lm_bytecode::NO_CTOR,
            ctor: lm_bytecode::NO_CTOR,
            constant: Some(lm_bytecode::Constant {
                ty: m.bc_ty(constant.ty),
                value: lower_const_value(&constant.value)?,
            }),
        });
    }
    // The generated constructor of a class takes a binding derived
    // from the qualified key of that class. The class structural hash
    // covers no constructor, because the constructor is a function
    // value of its own. The binding is what makes two providers of one
    // class key with two constructors a rejection instead of a merge.
    let mut bindings = hir.bindings.clone();
    for (cidx, class) in hir.classes.iter().enumerate() {
        if class.imported {
            continue;
        }
        bindings.push(lm_bytecode::FuncBinding {
            key: lm_bytecode::ctor_binding_key(&class.key),
            func: new_base + cidx as u32,
            class: cidx as u32,
        });
    }
    let mut module = Module {
        strings: m.strings,
        bytes: m.bytes,
        types: m.types,
        selectors: m.selectors,
        apps: m.apps,
        interfaces,
        conformances,
        class_bounds,
        func_bounds,
        imports,
        slots: Vec::new(),
        core_roles: hir.core_roles,
        classes,
        funcs,
        entry: hir.entry as u32,
        exports,
        bindings,
        debug: Vec::new(),
    };
    for (function, selected) in &linkage.functions {
        let func = module
            .funcs
            .get(*function as usize)
            .ok_or_else(|| format!("late function {function} does not exist"))?;
        if !func.captures.is_empty() {
            return Err(format!("late function {function} has captures"));
        }
        let contract = BcCallableContract {
            type_params: func.type_params,
            effect_params: func.effect_params,
            type_bounds: module.func_bounds[*function as usize].clone(),
            params: func.params.clone(),
            param_muts: func.param_muts.clone(),
            ret: func.ret,
            row: func.row.clone(),
        };
        module.slots.push(SlotSpec {
            binding: selected.binding.clone(),
            late: selected.late,
            key: selected.key,
            contract_hash: selected.contract_hash,
            contract: match selected.kind {
                LateCallableKind::Function => SlotContract::Function(contract),
                LateCallableKind::Method => SlotContract::Method(contract),
            },
            initial: Some(SlotTarget::Function(*function)),
        });
    }
    for (class, selected) in &linkage.classes {
        let definition = module
            .classes
            .get(*class as usize)
            .ok_or_else(|| format!("late class {class} does not exist"))?;
        if definition.kind == BcClassKind::Abstract {
            return Err(format!("late class {class} is abstract"));
        }
        let constructor = module
            .funcs
            .get(hir.funcs.len() + *class as usize)
            .ok_or_else(|| format!("late class {class} has no constructor"))?;
        let constructor_index = hir.funcs.len() as u32 + *class;
        let ty = constructor.ret;
        match module.types.get(ty as usize) {
            Some(BcType::Class(found)) if found == class => {}
            Some(BcType::Inst(found, args))
                if found == class && args.len() == definition.type_params as usize => {}
            _ => return Err(format!("late class {class} has a native representation")),
        }
        module.slots.push(SlotSpec {
            binding: selected.binding.clone(),
            late: selected.late,
            key: selected.key,
            contract_hash: selected.abi,
            contract: SlotContract::Class {
                type_params: definition.type_params,
                abi: selected.abi,
                ty,
                constructor: BcCallableContract {
                    type_params: constructor.type_params,
                    effect_params: constructor.effect_params,
                    type_bounds: module.func_bounds[constructor_index as usize].clone(),
                    params: constructor.params.clone(),
                    param_muts: constructor.param_muts.clone(),
                    ret: constructor.ret,
                    row: constructor.row.clone(),
                },
            },
            initial: Some(SlotTarget::Class {
                class: *class,
                constructor: constructor_index,
            }),
        });
    }
    Ok(module)
}

/// Lower one checked constant without creating runtime code.
fn lower_const_value(expr: &HExpr) -> Result<lm_bytecode::ConstValue, String> {
    Ok(match &expr.kind {
        HExprKind::Unit => lm_bytecode::ConstValue::Unit,
        HExprKind::Bool(value) => lm_bytecode::ConstValue::Bool(*value),
        HExprKind::Int(value) => lm_bytecode::ConstValue::Int(*value),
        HExprKind::Float(bits) => lm_bytecode::ConstValue::Float(*bits),
        HExprKind::Char(value) => lm_bytecode::ConstValue::Char(*value),
        HExprKind::Str(value) => lm_bytecode::ConstValue::String(value.clone()),
        HExprKind::Bytes(value) => lm_bytecode::ConstValue::Bytes(value.clone()),
        HExprKind::TupleLit(items) => lm_bytecode::ConstValue::Tuple(
            items
                .iter()
                .map(lower_const_value)
                .collect::<Result<_, _>>()?,
        ),
        _ => return Err("a checked constant contains a runtime expression".to_string()),
    })
}

#[derive(Clone, Copy)]
struct LoopTargets {
    continue_block: u32,
    exit_block: u32,
    result_slot: Option<u32>,
    entry_depth: usize,
}

struct Lowerer<'a, 'm> {
    m: &'a mut ModLowerer<'m>,
    blocks: Vec<Vec<Instr>>,
    cur: usize,
    /// Static operand depth at the current instruction.
    stack_depth: usize,
    /// Static operand depth at each block entry.
    block_depths: Vec<Option<usize>>,
    /// Targets and entry depth for each active loop.
    loops: Vec<LoopTargets>,
    /// The declared type of every local slot so far. The checker
    /// types come first; scratch slots append their true types. The
    /// slot count is the vector length.
    local_types: Vec<u32>,
}

#[derive(Clone, Copy)]
enum MapAction {
    Has,
    At,
    Get,
    Put,
    Remove,
    PutDiscard,
}

impl<'a, 'm> Lowerer<'a, 'm> {
    fn new(m: &'a mut ModLowerer<'m>, local_types: Vec<u32>) -> Lowerer<'a, 'm> {
        Lowerer {
            m,
            blocks: vec![Vec::new()],
            cur: 0,
            stack_depth: 0,
            block_depths: vec![Some(0)],
            loops: Vec::new(),
            local_types,
        }
    }

    fn emit(&mut self, instr: Instr) {
        let (pops, pushes) = stack_effect(self.m, &instr);
        debug_assert!(
            self.stack_depth >= pops,
            "instruction {instr:?} pops {pops} values from a stack of depth {}",
            self.stack_depth
        );
        self.stack_depth = self.stack_depth.saturating_sub(pops) + pushes;
        match &instr {
            Instr::Jump(target) | Instr::JumpIfFalse(target) | Instr::JumpIfTrue(target) => {
                self.record_block_depth(*target);
            }
            _ => {}
        }
        if matches!(instr, Instr::Pop) {
            if let Some(Instr::MapPut { discard, .. }) = self.blocks[self.cur].last_mut() {
                if !*discard {
                    *discard = true;
                    return;
                }
            }
        }
        self.blocks[self.cur].push(instr);
    }

    fn new_block(&mut self) -> u32 {
        self.blocks.push(Vec::new());
        self.block_depths.push(None);
        (self.blocks.len() - 1) as u32
    }

    fn switch_to(&mut self, block: u32) {
        self.cur = block as usize;
        self.stack_depth = self.block_depths[self.cur].unwrap_or(0);
    }

    fn record_block_depth(&mut self, block: u32) {
        let depth = &mut self.block_depths[block as usize];
        if depth.is_none() {
            *depth = Some(self.stack_depth);
        }
    }

    /// Remove operands that the current expression left above one loop.
    fn unwind_to_loop(&mut self, entry_depth: usize) {
        while self.stack_depth > entry_depth {
            self.emit(Instr::Pop);
        }
    }

    fn push_loop(&mut self, continue_block: u32, exit_block: u32, result_slot: Option<u32>) {
        self.loops.push(LoopTargets {
            continue_block,
            exit_block,
            result_slot,
            entry_depth: self.stack_depth,
        });
    }

    /// Lower one operand and report whether evaluation can continue.
    fn lower_operand(&mut self, expr: &HExpr) -> bool {
        self.lower_expr(expr);
        expr.flow == Flow::Normal
    }

    /// Lower strict operands until one transfers control.
    fn lower_operands<'e>(&mut self, exprs: impl IntoIterator<Item = &'e HExpr>) -> bool {
        for expr in exprs {
            if !self.lower_operand(expr) {
                return false;
            }
        }
        true
    }

    /// Allocate one scratch local slot with its declared type.
    fn scratch(&mut self, ty: u32) -> u32 {
        let slot = self.local_types.len() as u32;
        self.local_types.push(ty);
        slot
    }

    /// Test whether one type names an enum family or one of its arms.
    ///
    /// A constructor builds a family value, so an arm type reaches
    /// this test through an annotation alone. Both answer yes: the
    /// value of either is a case with fields, and equality compares
    /// the case and the fields.
    fn is_enum_family(&self, ty: TypeId) -> bool {
        let Some((class, _)) = self.m.store.nominal_class(ty) else {
            return false;
        };
        matches!(
            self.m.store.class_meta(class).kind,
            lm_types::ClassKind::EnumParent | lm_types::ClassKind::EnumCase
        )
    }

    /// Allocate one scratch slot for a checker type.
    fn scratch_of(&mut self, ty: TypeId) -> u32 {
        let bc = self.m.bc_ty(ty);
        self.scratch(bc)
    }

    /// Test whether the existing native map instructions support one key type.
    fn native_map_key(&self, ty: TypeId) -> bool {
        match self.m.store.get(ty) {
            Type::Bool | Type::Int | Type::Float | Type::String | Type::Bytes => true,
            Type::Class(class) | Type::Inst(class, _) => matches!(
                self.m.classes[class.0 as usize].native_repr,
                Some(
                    NativeRepr::Text
                        | NativeRepr::String
                        | NativeRepr::Substring
                        | NativeRepr::Char
                        | NativeRepr::Bytes
                )
            ),
            _ => false,
        }
    }

    /// Emit a `Hashable.__hash__` call for one value in a local slot.
    fn lower_hash_call(&mut self, key: u32, key_ty: TypeId) {
        self.emit(Instr::LoadLocal(key));
        let recv_ty = self.m.bc_ty(key_ty);
        self.emit(interface_call(
            self.m.core.hashable_interface,
            self.m.core.hashable_method,
            recv_ty,
            lm_bytecode::NO_APP,
        ));
    }

    /// Emit a `PartialEq.__eq__` call for one stored and query key.
    fn lower_key_eq_call(&mut self, map: u32, token: u32, key: u32, key_ty: TypeId) {
        self.emit(Instr::LoadLocal(map));
        self.emit(Instr::LoadLocal(token));
        self.emit(extended(ExtendedInstr::MapProbeKey));
        self.emit(Instr::LoadLocal(key));
        let recv_ty = self.m.bc_ty(key_ty);
        self.emit(interface_call(
            self.m.core.partial_eq_interface,
            self.m.core.partial_eq_method,
            recv_ty,
            lm_bytecode::NO_APP,
        ));
    }

    /// Lower one map operation whose key strategy uses verified interfaces.
    fn lower_hashable_map_action(
        &mut self,
        action: MapAction,
        args: &[HExpr],
        reply: Option<TypeId>,
    ) -> bool {
        let (key_ty, value_ty) = match self.m.store.get(args[0].ty) {
            Type::Map(key, value) => (*key, *value),
            _ => unreachable!("a map intrinsic receives a map"),
        };
        let map = self.scratch_of(args[0].ty);
        let key = self.scratch_of(key_ty);
        let value = matches!(action, MapAction::Put | MapAction::PutDiscard)
            .then(|| self.scratch_of(value_ty));
        let hash = self.scratch_of(INT);
        let token = self.scratch_of(INT);
        let reply_ty = reply.map(|ty| self.m.bc_ty(ty));

        if !self.lower_operand(&args[0]) {
            return false;
        }
        self.emit(Instr::StoreLocal(map));
        if !self.lower_operand(&args[1]) {
            return false;
        }
        self.emit(Instr::StoreLocal(key));
        if let Some(slot) = value {
            if !self.lower_operand(&args[2]) {
                return false;
            }
            self.emit(Instr::StoreLocal(slot));
        }
        if matches!(
            action,
            MapAction::Put | MapAction::PutDiscard | MapAction::Remove
        ) {
            self.emit(Instr::LoadLocal(map));
            self.emit(extended(ExtendedInstr::MapWriteGuard));
            self.emit(Instr::Pop);
        }
        self.lower_hash_call(key, key_ty);
        self.emit(Instr::StoreLocal(hash));
        self.emit(Instr::LoadLocal(map));
        self.emit(Instr::LoadLocal(hash));
        self.emit(Instr::ConstInt(0));
        self.emit(extended(ExtendedInstr::MapProbe));
        self.emit(Instr::StoreLocal(token));

        let probe = self.new_block();
        let compare = self.new_block();
        let advance = self.new_block();
        let hit = self.new_block();
        let miss = self.new_block();
        let join = self.new_block();
        self.emit(Instr::Jump(probe));

        self.switch_to(probe);
        self.emit(Instr::LoadLocal(token));
        self.emit(extended(ExtendedInstr::MapProbeFound));
        self.emit(Instr::JumpIfTrue(compare));
        self.emit(Instr::Jump(miss));

        self.switch_to(compare);
        self.lower_key_eq_call(map, token, key, key_ty);
        self.emit(Instr::JumpIfTrue(hit));
        self.emit(Instr::Jump(advance));

        self.switch_to(advance);
        self.emit(Instr::LoadLocal(map));
        self.emit(Instr::LoadLocal(hash));
        self.emit(Instr::LoadLocal(token));
        self.emit(extended(ExtendedInstr::MapProbe));
        self.emit(Instr::StoreLocal(token));
        self.emit(Instr::Jump(probe));

        self.switch_to(hit);
        match action {
            MapAction::Has => self.emit(Instr::ConstBool(true)),
            MapAction::At | MapAction::Get => {
                self.emit(Instr::LoadLocal(map));
                self.emit(Instr::LoadLocal(token));
                self.emit(extended(ExtendedInstr::MapProbeValue));
                if matches!(action, MapAction::Get) {
                    self.emit(extended(ExtendedInstr::OptionSome {
                        ty: reply_ty.expect("get returns Option"),
                    }));
                }
            }
            MapAction::Put => {
                let old = self.scratch_of(value_ty);
                self.emit(Instr::LoadLocal(map));
                self.emit(Instr::LoadLocal(token));
                self.emit(extended(ExtendedInstr::MapProbeValue));
                self.emit(Instr::StoreLocal(old));
                self.emit(Instr::LoadLocal(map));
                self.emit(Instr::LoadLocal(token));
                self.emit(Instr::LoadLocal(value.expect("put has a value")));
                self.emit(extended(ExtendedInstr::MapProbeSetValue));
                self.emit(Instr::Pop);
                self.emit(Instr::LoadLocal(old));
                self.emit(extended(ExtendedInstr::OptionSome {
                    ty: reply_ty.expect("put returns Option"),
                }));
            }
            MapAction::Remove => {
                self.emit(Instr::LoadLocal(map));
                self.emit(Instr::LoadLocal(token));
                self.emit(extended(ExtendedInstr::MapProbeRemove));
                self.emit(extended(ExtendedInstr::OptionSome {
                    ty: reply_ty.expect("remove returns Option"),
                }));
            }
            MapAction::PutDiscard => {
                self.emit(Instr::LoadLocal(map));
                self.emit(Instr::LoadLocal(token));
                self.emit(Instr::LoadLocal(value.expect("put has a value")));
                self.emit(extended(ExtendedInstr::MapProbeSetValue));
            }
        }
        self.emit(Instr::Jump(join));

        self.switch_to(miss);
        match action {
            MapAction::Has => self.emit(Instr::ConstBool(false)),
            MapAction::At => {
                self.emit(Instr::LoadLocal(map));
                self.emit(Instr::LoadLocal(token));
                self.emit(extended(ExtendedInstr::MapProbeValue));
            }
            MapAction::Get | MapAction::Remove => {
                self.emit(extended(ExtendedInstr::OptionNone {
                    ty: reply_ty.expect("the operation returns Option"),
                }));
            }
            MapAction::Put | MapAction::PutDiscard => {
                self.emit(Instr::LoadLocal(map));
                self.emit(Instr::LoadLocal(key));
                self.emit(Instr::LoadLocal(value.expect("put has a value")));
                self.emit(Instr::LoadLocal(hash));
                self.emit(Instr::LoadLocal(token));
                self.emit(extended(ExtendedInstr::MapInsertHashed));
                if matches!(action, MapAction::Put) {
                    self.emit(Instr::Pop);
                    self.emit(extended(ExtendedInstr::OptionNone {
                        ty: reply_ty.expect("put returns Option"),
                    }));
                }
            }
        }
        self.emit(Instr::Jump(join));
        self.switch_to(join);
        true
    }

    /// Emit a structural comparison of the tuples in the locals `a`
    /// and `b`. The expansion leaves one `Bool` on the operand stack.
    /// Unit elements are always equal, so they emit no test.
    fn lower_tuple_eq(&mut self, a: u32, b: u32, ty: TypeId) {
        let elems = match self.m.store.get(ty) {
            Type::Tuple(elems) => elems.clone(),
            _ => unreachable!("tuple equality on a non-tuple type"),
        };
        let tested: Vec<(usize, TypeId)> = elems
            .iter()
            .copied()
            .enumerate()
            .filter(|(_, e)| *e != UNIT)
            .collect();
        if tested.is_empty() {
            // Every element is unit, so the result is a constant and
            // a failure block would have no predecessor.
            self.emit(Instr::ConstBool(true));
            return;
        }
        let false_b = self.new_block();
        let join_b = self.new_block();
        for (i, elem) in &tested {
            if matches!(self.m.store.get(*elem), Type::Tuple(_)) {
                let sa = self.scratch_of(*elem);
                let sb = self.scratch_of(*elem);
                self.emit(Instr::LoadLocal(a));
                self.emit(Instr::TupleGet(*i as u32));
                self.emit(Instr::StoreLocal(sa));
                self.emit(Instr::LoadLocal(b));
                self.emit(Instr::TupleGet(*i as u32));
                self.emit(Instr::StoreLocal(sb));
                self.lower_tuple_eq(sa, sb, *elem);
            } else {
                self.emit(Instr::LoadLocal(a));
                self.emit(Instr::TupleGet(*i as u32));
                self.emit(Instr::LoadLocal(b));
                self.emit(Instr::TupleGet(*i as u32));
                self.emit(binary_instr(BinOp::Eq, *elem));
            }
            self.emit(Instr::JumpIfFalse(false_b));
        }
        self.emit(Instr::ConstBool(true));
        self.emit(Instr::Jump(join_b));
        self.switch_to(false_b);
        self.emit(Instr::ConstBool(false));
        self.emit(Instr::Jump(join_b));
        self.switch_to(join_b);
    }

    /// Close every open block and return the block list.
    fn finish(mut self, pushed: bool) -> Vec<Vec<Instr>> {
        if pushed {
            self.emit(Instr::Return);
        }
        // Close every open block. Only dead continuation blocks stay
        // open here. They receive an explicit return, so the structure
        // is valid.
        for block in &mut self.blocks {
            let terminated = block.last().map(Instr::is_terminator).unwrap_or(false);
            if !terminated {
                block.push(Instr::ConstUnit);
                block.push(Instr::Return);
            }
        }
        self.blocks
    }

    /// Lower a statement. The operand stack is unchanged.
    fn lower_stmt(&mut self, stmt: &HStmt) {
        match stmt {
            HStmt::Assign { slot, value } => {
                if !self.lower_operand(value) {
                    return;
                }
                self.emit(Instr::StoreLocal(*slot));
            }
            HStmt::AssignField { recv, field, value } => {
                if !self.lower_operand(recv) || !self.lower_operand(value) {
                    return;
                }
                self.emit(Instr::StoreField(*field));
            }
            HStmt::While { cond, body } => {
                let cond_b = self.new_block();
                self.emit(Instr::Jump(cond_b));
                self.switch_to(cond_b);
                let body_b = self.new_block();
                let exit_b = self.new_block();
                if !self.lower_operand(cond) {
                    return;
                }
                self.emit(Instr::JumpIfFalse(exit_b));
                self.emit(Instr::Jump(body_b));
                self.switch_to(body_b);
                self.push_loop(cond_b, exit_b, None);
                let diverged = self.lower_block_stmt(body);
                self.loops.pop();
                if !diverged {
                    self.emit(Instr::Jump(cond_b));
                }
                self.switch_to(exit_b);
            }
            HStmt::For {
                source,
                bindings,
                kind,
                body,
            } => self.lower_for(source, bindings, kind, body),
            HStmt::Return { value } => {
                match value {
                    Some(value) if !self.lower_operand(value) => return,
                    Some(_) => {}
                    None => self.emit(Instr::ConstUnit),
                }
                self.emit(Instr::Return);
                let dead = self.new_block();
                self.switch_to(dead);
            }
            HStmt::Break { value } => {
                let targets = *self.loops.last().expect("checked loop context");
                if let Some(value) = value {
                    self.lower_expr(value);
                    if value.flow == Flow::Never {
                        return;
                    }
                    if let Some(slot) = targets.result_slot {
                        self.emit(Instr::StoreLocal(slot));
                    } else {
                        self.emit(Instr::Pop);
                    }
                }
                self.unwind_to_loop(targets.entry_depth);
                self.emit(Instr::Jump(targets.exit_block));
                let dead = self.new_block();
                self.switch_to(dead);
            }
            HStmt::Continue => {
                let targets = *self.loops.last().expect("checked loop context");
                self.unwind_to_loop(targets.entry_depth);
                self.emit(Instr::Jump(targets.continue_block));
                let dead = self.new_block();
                self.switch_to(dead);
            }
            HStmt::Expr(expr) => {
                self.lower_expr(expr);
                if expr.flow == Flow::Normal {
                    self.emit(Instr::Pop);
                }
            }
        }
    }

    /// Lower one checked traversal strategy.
    fn lower_for(&mut self, source: &HExpr, bindings: &[u32], kind: &HForKind, body: &[HStmt]) {
        let source_slot = match kind {
            HForKind::List { source_slot, .. }
            | HForKind::Map { source_slot, .. }
            | HForKind::Text { source_slot, .. }
            | HForKind::Range { source_slot, .. }
            | HForKind::Generic { source_slot, .. } => *source_slot,
        };
        if !self.lower_operand(source) {
            return;
        }
        self.emit(Instr::StoreLocal(source_slot));

        match kind {
            HForKind::List {
                index_slot,
                epoch_slot,
                element,
                ..
            } => {
                self.m.bc_ty(*element);
                self.emit(Instr::LoadLocal(source_slot));
                self.emit(extended(ExtendedInstr::ListEpoch));
                self.emit(Instr::StoreLocal(*epoch_slot));
                self.emit(Instr::ConstInt(0));
                self.emit(Instr::StoreLocal(*index_slot));

                let cond_b = self.new_block();
                let body_b = self.new_block();
                let exit_b = self.new_block();
                self.emit(Instr::Jump(cond_b));
                self.switch_to(cond_b);
                self.emit(Instr::LoadLocal(*index_slot));
                self.emit(Instr::LoadLocal(source_slot));
                self.emit(Instr::LoadLocal(*epoch_slot));
                self.emit(extended(ExtendedInstr::ListIterLen));
                self.emit(Instr::LtInt);
                self.emit(Instr::JumpIfFalse(exit_b));
                self.emit(Instr::Jump(body_b));

                self.switch_to(body_b);
                self.emit(Instr::LoadLocal(source_slot));
                self.emit(Instr::LoadLocal(*index_slot));
                self.emit(Instr::ListAt);
                self.emit(Instr::StoreLocal(bindings[0]));
                self.increment_local(*index_slot);
                self.push_loop(cond_b, exit_b, None);
                let diverged = self.lower_block_stmt(body);
                self.loops.pop();
                if !diverged {
                    self.emit(Instr::Jump(cond_b));
                }
                self.switch_to(exit_b);
            }
            HForKind::Map {
                index_slot,
                epoch_slot,
                key,
                value,
                pair,
                ..
            } => {
                self.m.bc_ty(*key);
                self.m.bc_ty(*value);
                self.emit(Instr::LoadLocal(source_slot));
                self.emit(extended(ExtendedInstr::MapEpoch));
                self.emit(Instr::StoreLocal(*epoch_slot));
                self.emit(Instr::ConstInt(0));
                self.emit(Instr::StoreLocal(*index_slot));

                let cond_b = self.new_block();
                let body_b = self.new_block();
                let exit_b = self.new_block();
                self.emit(Instr::Jump(cond_b));
                self.switch_to(cond_b);
                self.emit(Instr::LoadLocal(source_slot));
                self.emit(Instr::LoadLocal(*index_slot));
                self.emit(Instr::LoadLocal(*epoch_slot));
                self.emit(extended(ExtendedInstr::MapNextIndex));
                self.emit(Instr::StoreLocal(*index_slot));
                self.emit(Instr::LoadLocal(*index_slot));
                self.emit(Instr::ConstInt(0));
                self.emit(Instr::LtInt);
                self.emit(Instr::JumpIfTrue(exit_b));
                self.emit(Instr::Jump(body_b));

                self.switch_to(body_b);
                if bindings.len() == 1 {
                    self.emit(Instr::LoadLocal(source_slot));
                    self.emit(Instr::LoadLocal(*index_slot));
                    self.emit(extended(ExtendedInstr::MapKeyAt));
                    self.emit(Instr::LoadLocal(source_slot));
                    self.emit(Instr::LoadLocal(*index_slot));
                    self.emit(extended(ExtendedInstr::MapValueAt));
                    let pair = self.m.bc_ty(*pair);
                    self.emit(Instr::TupleNew { ty: pair, count: 2 });
                    self.emit(Instr::StoreLocal(bindings[0]));
                } else {
                    self.emit(Instr::LoadLocal(source_slot));
                    self.emit(Instr::LoadLocal(*index_slot));
                    self.emit(extended(ExtendedInstr::MapKeyAt));
                    self.emit(Instr::StoreLocal(bindings[0]));
                    self.emit(Instr::LoadLocal(source_slot));
                    self.emit(Instr::LoadLocal(*index_slot));
                    self.emit(extended(ExtendedInstr::MapValueAt));
                    self.emit(Instr::StoreLocal(bindings[1]));
                }
                self.increment_local(*index_slot);
                self.push_loop(cond_b, exit_b, None);
                let diverged = self.lower_block_stmt(body);
                self.loops.pop();
                if !diverged {
                    self.emit(Instr::Jump(cond_b));
                }
                self.switch_to(exit_b);
            }
            HForKind::Text {
                cursor_slot, item, ..
            } => {
                self.m.bc_ty(*item);
                self.emit(Instr::ConstInt(0));
                self.emit(Instr::StoreLocal(*cursor_slot));

                let cond_b = self.new_block();
                let body_b = self.new_block();
                let exit_b = self.new_block();
                self.emit(Instr::Jump(cond_b));
                self.switch_to(cond_b);
                self.emit(Instr::LoadLocal(*cursor_slot));
                self.emit(Instr::LoadLocal(source_slot));
                self.emit(Instr::Native(lm_bytecode::NativeInstr::StrByteLen));
                self.emit(Instr::LtInt);
                self.emit(Instr::JumpIfFalse(exit_b));
                self.emit(Instr::Jump(body_b));

                self.switch_to(body_b);
                self.emit(Instr::LoadLocal(source_slot));
                self.emit(Instr::LoadLocal(*cursor_slot));
                self.emit(Instr::Native(lm_bytecode::NativeInstr::TextAtByte));
                self.emit(Instr::StoreLocal(bindings[0]));
                self.emit(Instr::LoadLocal(*cursor_slot));
                self.emit(Instr::LoadLocal(bindings[0]));
                self.emit(Instr::Native(lm_bytecode::NativeInstr::CharUtf8Len));
                self.emit(Instr::Add);
                self.emit(Instr::StoreLocal(*cursor_slot));
                self.push_loop(cond_b, exit_b, None);
                let diverged = self.lower_block_stmt(body);
                self.loops.pop();
                if !diverged {
                    self.emit(Instr::Jump(cond_b));
                }
                self.switch_to(exit_b);
            }
            HForKind::Range {
                cursor_slot,
                stop_slot,
                ..
            } => {
                self.emit(Instr::LoadLocal(source_slot));
                self.emit(Instr::LoadField(0));
                self.emit(Instr::StoreLocal(*cursor_slot));
                self.emit(Instr::LoadLocal(source_slot));
                self.emit(Instr::LoadField(1));
                self.emit(Instr::StoreLocal(*stop_slot));

                let cond_b = self.new_block();
                let body_b = self.new_block();
                let exit_b = self.new_block();
                self.emit(Instr::Jump(cond_b));
                self.switch_to(cond_b);
                self.emit(Instr::LoadLocal(*cursor_slot));
                self.emit(Instr::LoadLocal(*stop_slot));
                self.emit(Instr::LtInt);
                self.emit(Instr::JumpIfFalse(exit_b));
                self.emit(Instr::Jump(body_b));

                self.switch_to(body_b);
                self.emit(Instr::LoadLocal(*cursor_slot));
                self.emit(Instr::StoreLocal(bindings[0]));
                self.increment_local(*cursor_slot);
                self.push_loop(cond_b, exit_b, None);
                let diverged = self.lower_block_stmt(body);
                self.loops.pop();
                if !diverged {
                    self.emit(Instr::Jump(cond_b));
                }
                self.switch_to(exit_b);
            }
            HForKind::Generic {
                iterator_slot,
                option_slot,
                item_slot,
                iterator,
                next,
                some_ty,
                item,
                ..
            } => {
                self.m.bc_ty(*item);
                self.lower_expr(iterator);
                self.emit(Instr::StoreLocal(*iterator_slot));

                let cond_b = self.new_block();
                let body_b = self.new_block();
                let exit_b = self.new_block();
                self.emit(Instr::Jump(cond_b));
                self.switch_to(cond_b);
                self.lower_expr(next);
                self.emit(Instr::StoreLocal(*option_slot));
                self.emit(Instr::LoadLocal(*option_slot));
                let some_ty = self.m.bc_ty(*some_ty);
                self.emit(Instr::IsType(some_ty));
                self.emit(Instr::JumpIfFalse(exit_b));
                self.emit(Instr::Jump(body_b));

                self.switch_to(body_b);
                self.emit(Instr::LoadLocal(*option_slot));
                self.emit(Instr::CastType(some_ty));
                self.emit(extended(ExtendedInstr::OptionPayload { ty: some_ty }));
                if let Some(item_slot) = item_slot {
                    self.emit(Instr::StoreLocal(*item_slot));
                    self.emit(Instr::LoadLocal(*item_slot));
                    self.emit(Instr::TupleGet(0));
                    self.emit(Instr::StoreLocal(bindings[0]));
                    self.emit(Instr::LoadLocal(*item_slot));
                    self.emit(Instr::TupleGet(1));
                    self.emit(Instr::StoreLocal(bindings[1]));
                } else {
                    self.emit(Instr::StoreLocal(bindings[0]));
                }
                self.push_loop(cond_b, exit_b, None);
                let diverged = self.lower_block_stmt(body);
                self.loops.pop();
                if !diverged {
                    self.emit(Instr::Jump(cond_b));
                }
                self.switch_to(exit_b);
            }
        }
    }

    fn increment_local(&mut self, slot: u32) {
        self.emit(Instr::LoadLocal(slot));
        self.emit(Instr::ConstInt(1));
        self.emit(Instr::Add);
        self.emit(Instr::StoreLocal(slot));
    }

    /// Lower a statement list without a value. Return true when the
    /// list ends with a diverging statement.
    fn lower_block_stmt(&mut self, stmts: &[HStmt]) -> bool {
        for stmt in stmts {
            self.lower_stmt(stmt);
            if stmt.diverges() {
                return true;
            }
        }
        false
    }

    /// Lower a statement list that produces one value. Return false
    /// when the list ends with a diverging statement and pushes nothing.
    fn lower_block_value(&mut self, stmts: &[HStmt]) -> bool {
        let Some((last, init)) = stmts.split_last() else {
            self.emit(Instr::ConstUnit);
            return true;
        };
        for stmt in init {
            self.lower_stmt(stmt);
        }
        match last {
            HStmt::Expr(expr) => {
                self.lower_expr(expr);
                expr.flow == Flow::Normal
            }
            stmt if stmt.diverges() => {
                self.lower_stmt(stmt);
                false
            }
            stmt => {
                self.lower_stmt(stmt);
                self.emit(Instr::ConstUnit);
                true
            }
        }
    }

    /// Emit a direct call, generic when arguments are present.
    fn emit_call(&mut self, func: u32, targs: &[TypeId], rowargs: &[Row]) {
        if let Some(slot) = self.m.function_slots.get(&func).copied() {
            let app = if targs.is_empty() && rowargs.is_empty() {
                NO_APP
            } else {
                self.m.app_of(targs, rowargs)
            };
            self.emit(extended(ExtendedInstr::CallSlot { slot, app }));
            return;
        }
        if targs.is_empty() && rowargs.is_empty() {
            self.emit(Instr::Call(func));
        } else {
            let app = self.m.app_of(targs, rowargs);
            self.emit(Instr::CallG { func, app });
        }
    }

    /// Lower an expression. Exactly one value is pushed unless the
    /// expression cannot complete.
    fn lower_expr(&mut self, expr: &HExpr) {
        match &expr.kind {
            HExprKind::Unit => self.emit(Instr::ConstUnit),
            HExprKind::Int(v) => self.emit(Instr::ConstInt(*v)),
            HExprKind::Float(bits) => self.emit(Instr::ConstFloat(*bits)),
            HExprKind::Char(value) => self.emit(Instr::ConstChar(u32::from(*value))),
            HExprKind::Bool(v) => self.emit(Instr::ConstBool(*v)),
            HExprKind::Str(v) => {
                let idx = self.m.intern_string(v);
                self.emit(Instr::ConstStr(idx));
            }
            HExprKind::Bytes(v) => {
                let idx = self.m.intern_bytes(v);
                self.emit(Instr::ConstBytes(idx));
            }
            HExprKind::Local(slot) => self.emit(Instr::LoadLocal(*slot)),
            HExprKind::Capture(idx) => self.emit(Instr::LoadCapture(*idx)),
            HExprKind::Not(inner) => {
                if !self.lower_operand(inner) {
                    return;
                }
                self.emit(Instr::Not);
            }
            HExprKind::Neg(inner) => {
                if !self.lower_operand(inner) {
                    return;
                }
                self.emit(Instr::Neg);
            }
            HExprKind::Binary {
                op,
                operand_ty,
                left,
                right,
            } => {
                if matches!(op, BinOp::Eq | BinOp::Ne)
                    && matches!(self.m.store.get(*operand_ty), Type::Tuple(_))
                {
                    if !self.lower_operand(left) {
                        return;
                    }
                    let a = self.scratch_of(*operand_ty);
                    self.emit(Instr::StoreLocal(a));
                    if !self.lower_operand(right) {
                        return;
                    }
                    let b = self.scratch_of(*operand_ty);
                    self.emit(Instr::StoreLocal(b));
                    self.lower_tuple_eq(a, b, *operand_ty);
                    if matches!(op, BinOp::Ne) {
                        self.emit(Instr::Not);
                    }
                } else if matches!(op, BinOp::Eq | BinOp::Ne) && self.is_enum_family(*operand_ty) {
                    // A sealed enum case is a value, so equality is
                    // the arm plus the fields. The comparison runs in
                    // the machine, which keeps its own stack, so a
                    // deep value costs no host frame.
                    if !self.lower_operand(left) || !self.lower_operand(right) {
                        return;
                    }
                    self.emit(if matches!(op, BinOp::Eq) {
                        Instr::EqValue
                    } else {
                        Instr::NeValue
                    });
                } else {
                    if !self.lower_operand(left) || !self.lower_operand(right) {
                        return;
                    }
                    self.emit(binary_instr(*op, *operand_ty));
                }
            }
            HExprKind::And(left, right) => {
                if !self.lower_operand(left) {
                    return;
                }
                let false_b = self.new_block();
                let join_b = self.new_block();
                self.emit(Instr::JumpIfFalse(false_b));
                if self.lower_operand(right) {
                    self.emit(Instr::Jump(join_b));
                }
                self.switch_to(false_b);
                self.emit(Instr::ConstBool(false));
                self.emit(Instr::Jump(join_b));
                self.switch_to(join_b);
            }
            HExprKind::Or(left, right) => {
                if !self.lower_operand(left) {
                    return;
                }
                let true_b = self.new_block();
                let join_b = self.new_block();
                self.emit(Instr::JumpIfTrue(true_b));
                if self.lower_operand(right) {
                    self.emit(Instr::Jump(join_b));
                }
                self.switch_to(true_b);
                self.emit(Instr::ConstBool(true));
                self.emit(Instr::Jump(join_b));
                self.switch_to(join_b);
            }
            HExprKind::Call {
                func,
                targs,
                rowargs,
                args,
            } => {
                let inline = self
                    .m
                    .inline_bodies
                    .get(*func as usize)
                    .and_then(Clone::clone);
                if rowargs.is_empty() && args.iter().all(|arg| arg.flow == Flow::Normal) {
                    if let Some(template) = inline {
                        if let Some(mut expanded) =
                            instantiate_inline(&template, args, &self.m.inline_bodies)
                        {
                            // The caller has already substituted each
                            // function type variable in the result.
                            expanded.ty = expr.ty;
                            self.lower_expr(&expanded);
                            return;
                        }
                    }
                }
                if !self.lower_operands(args) {
                    return;
                }
                self.emit_call(*func, targs, rowargs);
                if self.m.funcs[*func as usize].ret == NEVER {
                    self.emit(Instr::Unreachable);
                }
            }
            HExprKind::Construct { class, targs, args } => {
                if *class == self.m.core.some_class {
                    if !self.lower_operands(args) {
                        return;
                    }
                    let ty = self.m.bc_ty(expr.ty);
                    self.emit(extended(ExtendedInstr::OptionSome { ty }));
                    return;
                }
                if *class == self.m.core.none_class {
                    debug_assert!(args.is_empty());
                    let ty = self.m.bc_ty(expr.ty);
                    self.emit(extended(ExtendedInstr::OptionNone { ty }));
                    return;
                }
                if !self.lower_operands(args) {
                    return;
                }
                if let Some(slot) = self.m.class_slots.get(class).copied() {
                    let app = if targs.is_empty() {
                        NO_APP
                    } else {
                        self.m.app_of(targs, &[])
                    };
                    self.emit(extended(ExtendedInstr::NewSlot { slot, app }));
                } else {
                    let target = self.m.new_base + *class;
                    self.emit_call(target, targs, &[]);
                }
            }
            HExprKind::MethodCall {
                recv,
                selector,
                generic_owner,
                own_targs,
                own_rowargs,
                args,
            } => {
                if !self.lower_operand(recv) || !self.lower_operands(args) {
                    return;
                }
                let sel = self.m.selector(selector);
                let generic_recv = matches!(self.m.store.get(recv.ty), Type::Inst(_, _));
                if generic_recv
                    || *generic_owner
                    || !own_targs.is_empty()
                    || !own_rowargs.is_empty()
                {
                    let app = self.m.app_of(own_targs, own_rowargs);
                    self.emit(Instr::CallVirtualG {
                        selector: sel,
                        argc: args.len() as u32,
                        app,
                    });
                } else {
                    self.emit(Instr::CallVirtual {
                        selector: sel,
                        argc: args.len() as u32,
                    });
                }
            }
            HExprKind::InterfaceCall {
                recv,
                interface,
                method,
                selector: _,
                own_targs,
                own_rowargs,
                args,
                ..
            } => {
                if !self.lower_operand(recv) || !self.lower_operands(args) {
                    return;
                }
                let recv_ty = self.m.bc_ty(recv.ty);
                let app = if own_targs.is_empty() && own_rowargs.is_empty() {
                    lm_bytecode::NO_APP
                } else {
                    self.m.app_of(own_targs, own_rowargs)
                };
                self.emit(interface_call(*interface, *method, recv_ty, app));
            }
            HExprKind::FieldGet { recv, field } => {
                if !self.lower_operand(recv) {
                    return;
                }
                let native_option_payload = *field == 0
                    && matches!(
                        self.m.store.get(recv.ty),
                        Type::Inst(class, _) if class.0 == self.m.core.some_class
                    );
                let recv_ty = self.m.bc_ty(recv.ty);
                self.emit(if native_option_payload {
                    extended(ExtendedInstr::OptionPayload { ty: recv_ty })
                } else {
                    Instr::LoadField(*field)
                });
            }
            HExprKind::MakeClosure { func, captures } => {
                if !self.lower_operands(captures) {
                    return;
                }
                // The verifier resolves the closure type through the
                // type table, so the entry must exist.
                self.m.bc_ty(expr.ty);
                self.emit(Instr::MakeClosure {
                    func: *func,
                    captures: captures.len() as u32,
                });
            }
            HExprKind::FunctionCode { func } => {
                self.m.bc_ty(expr.ty);
                self.emit(extended(ExtendedInstr::FunctionCode { func: *func }));
            }
            HExprKind::ClassCode { class } => {
                self.m.bc_ty(expr.ty);
                self.emit(extended(ExtendedInstr::ClassCode { class: *class }));
            }
            HExprKind::CodeSource { code, .. } => {
                if !self.lower_operand(code) {
                    return;
                }
                let ty = self.m.bc_ty(expr.ty);
                self.emit(extended(ExtendedInstr::CodeSource { ty }));
            }
            HExprKind::CodeDefinition { code } => {
                if !self.lower_operand(code) {
                    return;
                }
                self.m.bc_ty(expr.ty);
                self.emit(extended(ExtendedInstr::CodeDefinition));
            }
            HExprKind::MakeCallback { func, captures } => {
                if !self.lower_operands(captures) {
                    return;
                }
                self.m.callback_type(*func);
                self.m.bc_ty(expr.ty);
                self.emit(extended(ExtendedInstr::MakeCallback {
                    func: *func,
                    captures: captures.len() as u32,
                }));
            }
            HExprKind::AsCallback(value) => {
                if !self.lower_operand(value) {
                    return;
                }
                self.m.bc_ty(expr.ty);
                self.emit(extended(ExtendedInstr::AsCallback));
            }
            HExprKind::CallValue { callee, args } => {
                let is_op = matches!(self.m.store.get(callee.ty), Type::Op(_, _));
                if !self.lower_operand(callee) || !self.lower_operands(args) {
                    return;
                }
                if is_op {
                    // The instruction carries the reply type, so the
                    // world can check the reply value at a boundary.
                    let reply_ty = self.m.bc_ty(expr.ty);
                    self.emit(Instr::PerformValue {
                        argc: args.len() as u32,
                        reply_ty,
                    });
                } else {
                    self.emit(Instr::CallValue {
                        argc: args.len() as u32,
                    });
                }
            }
            HExprKind::Spawn {
                class,
                body,
                ctor_ty,
                body_ty,
                args,
            } => {
                // The verifier reads the closure type out of the
                // module type table, so both function types must be
                // present before the instruction runs.
                self.m.bc_ty(*ctor_ty);
                self.m.bc_ty(*body_ty);
                // The sugar expands into what a user would write: the
                // construction function, the proc body, and the typed
                // argument tuple, then one `Proc.Spawn` perform.
                self.emit(Instr::MakeClosure {
                    func: self
                        .m
                        .class_dispatch
                        .get(class)
                        .copied()
                        .unwrap_or(self.m.new_base + *class),
                    captures: 0,
                });
                self.emit(Instr::MakeClosure {
                    func: *body,
                    captures: 0,
                });
                if args.is_empty() {
                    self.emit(Instr::ConstUnit);
                } else {
                    if !self.lower_operands(args) {
                        return;
                    }
                    let tys: Vec<TypeId> = args.iter().map(|a| a.ty).collect();
                    let tuple = self.m.store.find(&Type::Tuple(tys));
                    let ty = match tuple {
                        Some(id) => self.m.bc_ty(id),
                        None => unreachable!("the checker interned the argument tuple type"),
                    };
                    self.emit(Instr::TupleNew {
                        ty,
                        count: args.len() as u32,
                    });
                }
                // The spawn sugar expands into one perform, so the
                // instruction states the handle type it pushes.
                let reply_ty = self.m.bc_ty(expr.ty);
                self.emit(Instr::Perform {
                    op: lm_abi::OP_PROC_SPAWN,
                    argc: 3,
                    reply_ty,
                });
            }
            HExprKind::TupleLit(items) => {
                if !self.lower_operands(items) {
                    return;
                }
                let ty = self.m.bc_ty(expr.ty);
                self.emit(Instr::TupleNew {
                    ty,
                    count: items.len() as u32,
                });
            }
            HExprKind::TupleGet { tuple, index } => {
                if !self.lower_operand(tuple) {
                    return;
                }
                self.emit(Instr::TupleGet(*index));
            }
            HExprKind::IsType { value, ty } => {
                if !self.lower_operand(value) {
                    return;
                }
                let ty = self.m.bc_ty(*ty);
                self.emit(Instr::IsType(ty));
            }
            HExprKind::CastType { value, ty } => {
                if !self.lower_operand(value) {
                    return;
                }
                let ty = self.m.bc_ty(*ty);
                self.emit(Instr::CastType(ty));
            }
            HExprKind::ListLit(items) => {
                if !self.lower_operands(items) {
                    return;
                }
                let ty = self.m.bc_ty(expr.ty);
                self.emit(Instr::ListNew {
                    ty,
                    count: items.len() as u32,
                });
            }
            HExprKind::MapLit(entries) => {
                let (key_ty, _) = match self.m.store.get(expr.ty) {
                    Type::Map(key, value) => (*key, *value),
                    _ => unreachable!("a map literal has a map type"),
                };
                if self.native_map_key(key_ty) {
                    for (key, value) in entries {
                        if !self.lower_operand(key) || !self.lower_operand(value) {
                            return;
                        }
                    }
                    let ty = self.m.bc_ty(expr.ty);
                    self.emit(Instr::MapNew {
                        ty,
                        count: entries.len() as u32,
                    });
                } else {
                    let map = self.scratch_of(expr.ty);
                    let ty = self.m.bc_ty(expr.ty);
                    self.emit(Instr::MapNew { ty, count: 0 });
                    self.emit(Instr::StoreLocal(map));
                    for (key, value) in entries {
                        let args = vec![
                            HExpr {
                                flow: Flow::Normal,
                                ty: expr.ty,
                                mutable: true,
                                kind: HExprKind::Local(map),
                            },
                            key.clone(),
                            value.clone(),
                        ];
                        if !self.lower_hashable_map_action(MapAction::PutDiscard, &args, None) {
                            return;
                        }
                        self.emit(Instr::Pop);
                    }
                    self.emit(Instr::LoadLocal(map));
                }
            }
            HExprKind::Native { args, .. } if args.iter().any(|arg| arg.flow == Flow::Never) => {
                let _ = self.lower_operands(args);
            }
            HExprKind::Native {
                op: NativeOp::ListGet,
                args,
            } => self.lower_list_get(expr, args),
            HExprKind::Native {
                op: NativeOp::MapGet,
                args,
            } => self.lower_map_get(expr, args),
            HExprKind::Native { op, args } => {
                let map_action = match op {
                    NativeOp::MapHas => Some(MapAction::Has),
                    NativeOp::MapAt => Some(MapAction::At),
                    NativeOp::MapPut => Some(MapAction::Put),
                    _ => None,
                };
                if let Some(action) = map_action {
                    let key_ty = match self.m.store.get(args[0].ty) {
                        Type::Map(key, _) => *key,
                        _ => unreachable!("a map operation receives a map"),
                    };
                    if !self.native_map_key(key_ty) {
                        let _ = self.lower_hashable_map_action(action, args, Some(expr.ty));
                        return;
                    }
                }
                let operand_ty = args.first().map(|arg| arg.ty);
                if !self.lower_operands(args) {
                    return;
                }
                let instr = match op {
                    NativeOp::ListLen => Instr::ListLen,
                    NativeOp::ListAt => Instr::ListAt,
                    NativeOp::ListPush => Instr::ListPush,
                    NativeOp::MapLen => Instr::MapLen,
                    NativeOp::MapHas => Instr::MapHas,
                    NativeOp::MapAt => Instr::MapAt,
                    NativeOp::MapPut => Instr::MapPut {
                        ty: self.m.bc_ty(expr.ty),
                        discard: false,
                    },
                    NativeOp::BytesNew => {
                        self.m.intern_type(BcType::Bytes);
                        Instr::Native(lm_bytecode::NativeInstr::BytesNew)
                    }
                    NativeOp::Freeze => Instr::Freeze,
                    NativeOp::Digest => {
                        // The result type must exist in the module
                        // type table before the verifier reads it.
                        self.m.intern_type(BcType::Digest);
                        Instr::Digest {
                            ty: self
                                .m
                                .bc_ty(operand_ty.expect("digest has one receiver argument")),
                        }
                    }
                    NativeOp::ListGet | NativeOp::MapGet => unreachable!("handled above"),
                };
                self.emit(instr);
            }
            HExprKind::Intrinsic { intrinsic, args } => {
                if args.iter().any(|arg| arg.flow == Flow::Never) {
                    let _ = self.lower_operands(args);
                    return;
                }
                self.lower_intrinsic(*intrinsic, args, expr.ty)
            }
            HExprKind::Interp(parts) => {
                self.m
                    .intern_type(BcType::Class(self.m.string_builder_class));
                let builder_slot = parts.iter().find_map(|part| match part {
                    HInterpPart::Display { builder, .. } => Some(*builder),
                    _ => None,
                });
                self.emit(Instr::Native(lm_bytecode::NativeInstr::SbNew));
                if let Some(builder) = builder_slot {
                    self.emit(Instr::StoreLocal(builder));
                }
                for part in parts {
                    match part {
                        HInterpPart::Lit(text) => {
                            if let Some(builder) = builder_slot {
                                self.emit(Instr::LoadLocal(builder));
                            }
                            let idx = self.m.intern_string(text);
                            self.emit(Instr::ConstStr(idx));
                            self.emit(Instr::Native(lm_bytecode::NativeInstr::SbAppendStr));
                            if builder_slot.is_some() {
                                self.emit(Instr::Pop);
                            }
                        }
                        HInterpPart::Native { value, kind } => {
                            if let Some(builder) = builder_slot {
                                self.emit(Instr::LoadLocal(builder));
                            }
                            if !self.lower_operand(value) {
                                return;
                            }
                            self.emit(interp_native_instr(*kind));
                            if builder_slot.is_some() {
                                self.emit(Instr::Pop);
                            }
                        }
                        HInterpPart::Display {
                            value,
                            interface,
                            method,
                            builder,
                            selector: _,
                        } => {
                            if !self.lower_operand(value) {
                                return;
                            }
                            self.emit(Instr::LoadLocal(*builder));
                            let recv_ty = self.m.bc_ty(value.ty);
                            self.emit(interface_call(
                                *interface,
                                *method,
                                recv_ty,
                                lm_bytecode::NO_APP,
                            ));
                            self.emit(Instr::Pop);
                        }
                    }
                }
                if let Some(builder) = builder_slot {
                    self.emit(Instr::LoadLocal(builder));
                }
                self.emit(Instr::Native(lm_bytecode::NativeInstr::SbFinish));
            }
            HExprKind::Block(body) => {
                self.lower_block_stmt(body);
                if expr.flow == Flow::Normal && expr.ty == UNIT {
                    self.emit(Instr::ConstUnit);
                }
            }
            HExprKind::Loop { body, result_slot } => {
                let body_b = self.new_block();
                let exit_b = self.new_block();
                self.emit(Instr::Jump(body_b));
                self.switch_to(body_b);
                self.push_loop(body_b, exit_b, *result_slot);
                let diverged = self.lower_block_stmt(body);
                self.loops.pop();
                if !diverged {
                    self.emit(Instr::Jump(body_b));
                }
                self.switch_to(exit_b);
                if expr.ty == UNIT {
                    self.emit(Instr::ConstUnit);
                } else if let Some(slot) = result_slot {
                    self.emit(Instr::LoadLocal(*slot));
                }
            }
            HExprKind::If { arms, else_body } => {
                let join_b = self.new_block();
                let unit_valued = expr.ty == UNIT;
                let mut condition_diverged = false;
                for (cond, body) in arms {
                    if !self.lower_operand(cond) {
                        condition_diverged = true;
                        break;
                    }
                    let next_b = self.new_block();
                    self.emit(Instr::JumpIfFalse(next_b));
                    self.lower_branch(body, unit_valued, join_b);
                    self.switch_to(next_b);
                }
                if !condition_diverged {
                    match else_body {
                        Some(body) => self.lower_branch(body, unit_valued, join_b),
                        None => {
                            self.emit(Instr::ConstUnit);
                            self.emit(Instr::Jump(join_b));
                        }
                    }
                }
                self.switch_to(join_b);
            }
            HExprKind::Case {
                scrut,
                scrut_slot,
                arms,
            } => self.lower_case(scrut, *scrut_slot, arms, expr.ty == UNIT),
            HExprKind::Perform { op, args } => {
                if !self.lower_operands(args) {
                    return;
                }
                // The verifier reconstructs the perform result type
                // through the module type table, so the entry exists.
                // The instruction states the same index, and the world
                // checks the reply value against it at a boundary.
                let reply_ty = self.m.bc_ty(expr.ty);
                self.emit(Instr::Perform {
                    op: *op,
                    argc: args.len() as u32,
                    reply_ty,
                });
            }
            HExprKind::PrepareWait { op, args } => {
                if !self.lower_operands(args) {
                    return;
                }
                let Type::Wait(reply) = self.m.store.get(expr.ty) else {
                    unreachable!("a prepared wait has a Wait result type")
                };
                let reply_ty = self.m.bc_ty(*reply);
                let instruction = ExtendedInstr::prepare_wait(*op, args.len() as u32, reply_ty)
                    .expect("a checked wait fits the compact instruction");
                self.emit(extended(instruction));
            }
            HExprKind::OpConst(op) => {
                self.m.bc_ty(expr.ty);
                self.emit(Instr::OpConst(*op));
            }
            HExprKind::TableEdit {
                action,
                kind,
                slot,
                table,
                mock,
            } => {
                if !self.lower_operand(table) {
                    return;
                }
                if let Some(mock) = mock {
                    if !self.lower_operand(mock) {
                        return;
                    }
                }
                let action = match action {
                    TableAction::Pass => 0,
                    TableAction::Block => 1,
                    TableAction::Mock => 2,
                    TableAction::Clear => 3,
                };
                let kind = match kind {
                    TargetKind::Exact => 0,
                    TargetKind::Group => 1,
                };
                self.emit(Instr::TableEdit {
                    action,
                    kind,
                    slot: *slot,
                });
            }
            HExprKind::CallArgs { call } => {
                if !self.lower_operand(call) {
                    return;
                }
                self.m.bc_ty(expr.ty);
                self.emit(Instr::CallArgs);
            }
            HExprKind::FaultCodeGet { fault } => {
                if !self.lower_operand(fault) {
                    return;
                }
                self.emit(Instr::FaultCode);
            }
            HExprKind::FaultSiteGet { fault } => {
                if !self.lower_operand(fault) {
                    return;
                }
                let ty = self.m.bc_ty(expr.ty);
                self.emit(extended(ExtendedInstr::FaultSite { ty }));
            }
            HExprKind::FaultTraceGet { fault } => {
                if !self.lower_operand(fault) {
                    return;
                }
                let ty = self.m.bc_ty(expr.ty);
                self.emit(extended(ExtendedInstr::FaultTrace { ty }));
            }
            HExprKind::RequestOpName { request } => {
                if !self.lower_operand(request) {
                    return;
                }
                self.emit(Instr::RequestOp);
            }
            HExprKind::FaultDenied { reason } => {
                if !self.lower_operand(reason) {
                    return;
                }
                self.emit(Instr::FaultDenied);
            }
        }
    }

    /// Lower `list.get(i)` to one checked native read.
    fn lower_list_get(&mut self, expr: &HExpr, args: &[HExpr]) {
        self.lower_expr(&args[0]);
        self.lower_expr(&args[1]);
        let ty = self.m.bc_ty(expr.ty);
        self.emit(extended(ExtendedInstr::ListGet { ty }));
    }

    /// Lower `map.get(k)` to one hash-table probe.
    fn lower_map_get(&mut self, expr: &HExpr, args: &[HExpr]) {
        let key_ty = match self.m.store.get(args[0].ty) {
            Type::Map(key, _) => *key,
            _ => unreachable!("a map operation receives a map"),
        };
        if !self.native_map_key(key_ty) {
            let _ = self.lower_hashable_map_action(MapAction::Get, args, Some(expr.ty));
            return;
        }
        self.lower_expr(&args[0]);
        self.lower_expr(&args[1]);
        let ty = self.m.bc_ty(expr.ty);
        self.emit(extended(ExtendedInstr::MapGet { ty }));
    }

    /// Lower `int.abs` with existing checked integer instructions.
    fn lower_int_abs(&mut self, value: &HExpr) {
        let slot = self.scratch_of(INT);
        self.lower_expr(value);
        self.emit(Instr::StoreLocal(slot));
        self.emit(Instr::LoadLocal(slot));
        self.emit(Instr::ConstInt(0));
        self.emit(Instr::GeInt);
        let negative = self.new_block();
        let join = self.new_block();
        self.emit(Instr::JumpIfFalse(negative));
        self.emit(Instr::LoadLocal(slot));
        self.emit(Instr::Jump(join));
        self.switch_to(negative);
        self.emit(Instr::LoadLocal(slot));
        self.emit(Instr::Neg);
        self.emit(Instr::Jump(join));
        self.switch_to(join);
    }

    /// Lower one manifest intrinsic to existing instructions.
    fn lower_intrinsic(&mut self, intrinsic: lm_abi::IntrinsicSlot, args: &[HExpr], reply: TypeId) {
        if intrinsic == lm_abi::INTRINSIC_INT_ABS {
            self.lower_int_abs(&args[0]);
            return;
        }
        let map_action = match intrinsic {
            lm_abi::INTRINSIC_MAP_HAS => Some(MapAction::Has),
            lm_abi::INTRINSIC_MAP_AT => Some(MapAction::At),
            lm_abi::INTRINSIC_MAP_GET => Some(MapAction::Get),
            lm_abi::INTRINSIC_MAP_PUT => Some(MapAction::Put),
            lm_abi::INTRINSIC_MAP_REMOVE => Some(MapAction::Remove),
            _ => None,
        };
        if let Some(action) = map_action {
            let key_ty = match self.m.store.get(args[0].ty) {
                Type::Map(key, _) => *key,
                _ => unreachable!("a map intrinsic receives a map"),
            };
            if !self.native_map_key(key_ty) {
                let _ = self.lower_hashable_map_action(action, args, Some(reply));
                return;
            }
        }
        for arg in args {
            self.lower_expr(arg);
        }
        let instr = match intrinsic {
            lm_abi::INTRINSIC_INT_NEG => Instr::Neg,
            lm_abi::INTRINSIC_INT_ADD => Instr::Add,
            lm_abi::INTRINSIC_INT_SUB => Instr::Sub,
            lm_abi::INTRINSIC_INT_MUL => Instr::Mul,
            lm_abi::INTRINSIC_INT_DIV => Instr::Div,
            lm_abi::INTRINSIC_INT_REM => Instr::Rem,
            lm_abi::INTRINSIC_INT_EQ => Instr::EqInt,
            lm_abi::INTRINSIC_INT_NE => Instr::NeInt,
            lm_abi::INTRINSIC_INT_LT => Instr::LtInt,
            lm_abi::INTRINSIC_INT_LE => Instr::LeInt,
            lm_abi::INTRINSIC_INT_GT => Instr::GtInt,
            lm_abi::INTRINSIC_INT_GE => Instr::GeInt,
            lm_abi::INTRINSIC_INT_BIT_AND => Instr::Numeric(lm_bytecode::NumericInstr::IntBitAnd),
            lm_abi::INTRINSIC_INT_BIT_OR => Instr::Numeric(lm_bytecode::NumericInstr::IntBitOr),
            lm_abi::INTRINSIC_INT_BIT_XOR => Instr::Numeric(lm_bytecode::NumericInstr::IntBitXor),
            lm_abi::INTRINSIC_INT_BIT_NOT => Instr::Numeric(lm_bytecode::NumericInstr::IntBitNot),
            lm_abi::INTRINSIC_INT_SHL => Instr::Numeric(lm_bytecode::NumericInstr::IntShl),
            lm_abi::INTRINSIC_INT_SHR => Instr::Numeric(lm_bytecode::NumericInstr::IntShr),
            lm_abi::INTRINSIC_INT_USHR => Instr::Numeric(lm_bytecode::NumericInstr::IntUshr),
            lm_abi::INTRINSIC_INT_WRAPPING_ADD => {
                Instr::Numeric(lm_bytecode::NumericInstr::IntWrappingAdd)
            }
            lm_abi::INTRINSIC_INT_WRAPPING_SUB => {
                Instr::Numeric(lm_bytecode::NumericInstr::IntWrappingSub)
            }
            lm_abi::INTRINSIC_INT_WRAPPING_MUL => {
                Instr::Numeric(lm_bytecode::NumericInstr::IntWrappingMul)
            }
            lm_abi::INTRINSIC_INT_ROTATE_LEFT => {
                Instr::Numeric(lm_bytecode::NumericInstr::IntRotateLeft)
            }
            lm_abi::INTRINSIC_INT_ROTATE_RIGHT => {
                Instr::Numeric(lm_bytecode::NumericInstr::IntRotateRight)
            }
            lm_abi::INTRINSIC_INT_TO_FLOAT => Instr::Numeric(lm_bytecode::NumericInstr::IntToFloat),
            lm_abi::INTRINSIC_FLOAT_NEG => Instr::Numeric(lm_bytecode::NumericInstr::FloatNeg),
            lm_abi::INTRINSIC_FLOAT_ADD => Instr::Numeric(lm_bytecode::NumericInstr::FloatAdd),
            lm_abi::INTRINSIC_FLOAT_SUB => Instr::Numeric(lm_bytecode::NumericInstr::FloatSub),
            lm_abi::INTRINSIC_FLOAT_MUL => Instr::Numeric(lm_bytecode::NumericInstr::FloatMul),
            lm_abi::INTRINSIC_FLOAT_DIV => Instr::Numeric(lm_bytecode::NumericInstr::FloatDiv),
            lm_abi::INTRINSIC_FLOAT_EQ => Instr::Numeric(lm_bytecode::NumericInstr::FloatEq),
            lm_abi::INTRINSIC_FLOAT_NE => Instr::Numeric(lm_bytecode::NumericInstr::FloatNe),
            lm_abi::INTRINSIC_FLOAT_LT => Instr::Numeric(lm_bytecode::NumericInstr::FloatLt),
            lm_abi::INTRINSIC_FLOAT_LE => Instr::Numeric(lm_bytecode::NumericInstr::FloatLe),
            lm_abi::INTRINSIC_FLOAT_GT => Instr::Numeric(lm_bytecode::NumericInstr::FloatGt),
            lm_abi::INTRINSIC_FLOAT_GE => Instr::Numeric(lm_bytecode::NumericInstr::FloatGe),
            lm_abi::INTRINSIC_FLOAT_IS_NAN => Instr::Numeric(lm_bytecode::NumericInstr::FloatIsNan),
            lm_abi::INTRINSIC_FLOAT_HASH => Instr::Numeric(lm_bytecode::NumericInstr::FloatHash),
            lm_abi::INTRINSIC_FLOAT_BITS => Instr::Numeric(lm_bytecode::NumericInstr::FloatBits),
            lm_abi::INTRINSIC_FLOAT_FROM_BITS => {
                Instr::Numeric(lm_bytecode::NumericInstr::FloatFromBits)
            }
            lm_abi::INTRINSIC_FLOAT_TO_INT_STATUS => {
                Instr::Numeric(lm_bytecode::NumericInstr::FloatToIntStatus)
            }
            lm_abi::INTRINSIC_FLOAT_TO_INT_VALUE => {
                Instr::Numeric(lm_bytecode::NumericInstr::FloatToIntValue)
            }
            lm_abi::INTRINSIC_FLOAT_FIXED => Instr::Numeric(lm_bytecode::NumericInstr::FloatFixed),
            lm_abi::INTRINSIC_STRING_BUILDER_APPEND_FLOAT => {
                Instr::Numeric(lm_bytecode::NumericInstr::SbAppendFloat)
            }
            lm_abi::INTRINSIC_BYTES_BIT_AND => {
                Instr::Numeric(lm_bytecode::NumericInstr::BytesBitAnd)
            }
            lm_abi::INTRINSIC_BYTES_BIT_OR => Instr::Numeric(lm_bytecode::NumericInstr::BytesBitOr),
            lm_abi::INTRINSIC_BYTES_BIT_XOR => {
                Instr::Numeric(lm_bytecode::NumericInstr::BytesBitXor)
            }
            lm_abi::INTRINSIC_BYTES_BIT_NOT => {
                Instr::Numeric(lm_bytecode::NumericInstr::BytesBitNot)
            }
            lm_abi::INTRINSIC_BOOL_NOT => Instr::Not,
            lm_abi::INTRINSIC_BOOL_EQ => Instr::EqBool,
            lm_abi::INTRINSIC_BOOL_NE => Instr::NeBool,
            lm_abi::INTRINSIC_STRING_BYTE_LEN => {
                Instr::Native(lm_bytecode::NativeInstr::StrByteLen)
            }
            lm_abi::INTRINSIC_STRING_CHAR_COUNT => {
                Instr::Native(lm_bytecode::NativeInstr::StrCharCount)
            }
            lm_abi::INTRINSIC_STRING_CONCAT => Instr::Native(lm_bytecode::NativeInstr::StrConcat),
            lm_abi::INTRINSIC_STRING_STARTS_WITH => {
                Instr::Native(lm_bytecode::NativeInstr::StrStartsWith)
            }
            lm_abi::INTRINSIC_STRING_ENDS_WITH => {
                Instr::Native(lm_bytecode::NativeInstr::StrEndsWith)
            }
            lm_abi::INTRINSIC_STRING_CONTAINS => {
                Instr::Native(lm_bytecode::NativeInstr::StrContains)
            }
            lm_abi::INTRINSIC_STRING_FIND_INDEX => {
                Instr::Native(lm_bytecode::NativeInstr::StrFindIndex)
            }
            lm_abi::INTRINSIC_STRING_EQ => Instr::Native(lm_bytecode::NativeInstr::EqStr),
            lm_abi::INTRINSIC_STRING_NE => Instr::Native(lm_bytecode::NativeInstr::NeStr),
            lm_abi::INTRINSIC_BYTES_LEN => Instr::Native(lm_bytecode::NativeInstr::BytesLen),
            lm_abi::INTRINSIC_BYTES_AT => Instr::Native(lm_bytecode::NativeInstr::BytesAt),
            lm_abi::INTRINSIC_BYTES_GET => Instr::Native(lm_bytecode::NativeInstr::BytesGet),
            lm_abi::INTRINSIC_BYTES_SLICE => Instr::Native(lm_bytecode::NativeInstr::BytesSlice),
            lm_abi::INTRINSIC_BYTES_CONCAT => Instr::Native(lm_bytecode::NativeInstr::BytesConcat),
            lm_abi::INTRINSIC_BYTES_STARTS_WITH => {
                Instr::Native(lm_bytecode::NativeInstr::BytesStartsWith)
            }
            lm_abi::INTRINSIC_BYTES_FIND_INDEX => {
                Instr::Native(lm_bytecode::NativeInstr::BytesFindIndex)
            }
            lm_abi::INTRINSIC_BYTES_HEX => Instr::Native(lm_bytecode::NativeInstr::BytesHex),
            lm_abi::INTRINSIC_BYTES_IS_UTF8 => Instr::Native(lm_bytecode::NativeInstr::BytesIsUtf8),
            lm_abi::INTRINSIC_BYTES_TEXT => Instr::Native(lm_bytecode::NativeInstr::BytesText),
            lm_abi::INTRINSIC_BYTES_EQ => Instr::Native(lm_bytecode::NativeInstr::EqBytes),
            lm_abi::INTRINSIC_BYTES_NE => Instr::Native(lm_bytecode::NativeInstr::NeBytes),
            lm_abi::INTRINSIC_STRING_BUILDER_APPEND => {
                Instr::Native(lm_bytecode::NativeInstr::SbAppendStr)
            }
            lm_abi::INTRINSIC_STRING_BUILDER_APPEND_INT => {
                Instr::Native(lm_bytecode::NativeInstr::SbAppendInt)
            }
            lm_abi::INTRINSIC_STRING_BUILDER_APPEND_BOOL => {
                Instr::Native(lm_bytecode::NativeInstr::SbAppendBool)
            }
            lm_abi::INTRINSIC_STRING_BUILDER_LEN => Instr::Native(lm_bytecode::NativeInstr::SbLen),
            lm_abi::INTRINSIC_STRING_BUILDER_CLEAR => {
                Instr::Native(lm_bytecode::NativeInstr::SbClear)
            }
            lm_abi::INTRINSIC_STRING_BUILDER_BUILD => {
                Instr::Native(lm_bytecode::NativeInstr::SbBuild)
            }
            lm_abi::INTRINSIC_BYTE_BUFFER_APPEND => {
                Instr::Native(lm_bytecode::NativeInstr::BbAppend)
            }
            lm_abi::INTRINSIC_BYTE_BUFFER_EXTEND => {
                Instr::Native(lm_bytecode::NativeInstr::BbExtend)
            }
            lm_abi::INTRINSIC_BYTE_BUFFER_RESERVE => {
                Instr::Native(lm_bytecode::NativeInstr::BbReserve)
            }
            lm_abi::INTRINSIC_BYTE_BUFFER_CLEAR => Instr::Native(lm_bytecode::NativeInstr::BbClear),
            lm_abi::INTRINSIC_BYTE_BUFFER_LEN => Instr::Native(lm_bytecode::NativeInstr::BbLen),
            lm_abi::INTRINSIC_BYTE_BUFFER_BUILD => Instr::Native(lm_bytecode::NativeInstr::BbBuild),
            lm_abi::INTRINSIC_TEXT_AT => Instr::Native(lm_bytecode::NativeInstr::TextAt),
            lm_abi::INTRINSIC_TEXT_SLICE => Instr::Native(lm_bytecode::NativeInstr::TextSlice),
            lm_abi::INTRINSIC_TEXT_IS_BOUNDARY => {
                Instr::Native(lm_bytecode::NativeInstr::TextIsBoundary)
            }
            lm_abi::INTRINSIC_TEXT_SLICE_BYTES => {
                Instr::Native(lm_bytecode::NativeInstr::TextSliceBytes)
            }
            lm_abi::INTRINSIC_TEXT_BYTES => Instr::Native(lm_bytecode::NativeInstr::TextBytes),
            lm_abi::INTRINSIC_TEXT_LT => Instr::Native(lm_bytecode::NativeInstr::TextLt),
            lm_abi::INTRINSIC_TEXT_LE => Instr::Native(lm_bytecode::NativeInstr::TextLe),
            lm_abi::INTRINSIC_TEXT_GT => Instr::Native(lm_bytecode::NativeInstr::TextGt),
            lm_abi::INTRINSIC_TEXT_GE => Instr::Native(lm_bytecode::NativeInstr::TextGe),
            lm_abi::INTRINSIC_TEXT_TO_STRING => {
                Instr::Native(lm_bytecode::NativeInstr::TextToString)
            }
            lm_abi::INTRINSIC_CHAR_CODEPOINT => {
                Instr::Native(lm_bytecode::NativeInstr::CharCodepoint)
            }
            lm_abi::INTRINSIC_CHAR_UTF8_LEN => Instr::Native(lm_bytecode::NativeInstr::CharUtf8Len),
            lm_abi::INTRINSIC_CHAR_EQ => Instr::Native(lm_bytecode::NativeInstr::EqChar),
            lm_abi::INTRINSIC_CHAR_NE => Instr::Native(lm_bytecode::NativeInstr::NeChar),
            lm_abi::INTRINSIC_CHAR_LT => Instr::Native(lm_bytecode::NativeInstr::LtChar),
            lm_abi::INTRINSIC_CHAR_LE => Instr::Native(lm_bytecode::NativeInstr::LeChar),
            lm_abi::INTRINSIC_CHAR_GT => Instr::Native(lm_bytecode::NativeInstr::GtChar),
            lm_abi::INTRINSIC_CHAR_GE => Instr::Native(lm_bytecode::NativeInstr::GeChar),
            lm_abi::INTRINSIC_BYTES_COMPACT => {
                Instr::Native(lm_bytecode::NativeInstr::BytesCompact)
            }
            lm_abi::INTRINSIC_BYTES_TEXT_VIEW => {
                Instr::Native(lm_bytecode::NativeInstr::BytesTextView)
            }
            lm_abi::INTRINSIC_BYTES_LT => Instr::Native(lm_bytecode::NativeInstr::LtBytes),
            lm_abi::INTRINSIC_BYTES_LE => Instr::Native(lm_bytecode::NativeInstr::LeBytes),
            lm_abi::INTRINSIC_BYTES_GT => Instr::Native(lm_bytecode::NativeInstr::GtBytes),
            lm_abi::INTRINSIC_BYTES_GE => Instr::Native(lm_bytecode::NativeInstr::GeBytes),
            lm_abi::INTRINSIC_STRING_BUILDER_PUSH_CHAR => {
                Instr::Native(lm_bytecode::NativeInstr::SbAppendChar)
            }
            lm_abi::INTRINSIC_STRING_BUILDER_BYTE_LEN => {
                Instr::Native(lm_bytecode::NativeInstr::SbByteLen)
            }
            lm_abi::INTRINSIC_STRING_BUILDER_FINISH => {
                Instr::Native(lm_bytecode::NativeInstr::SbFinish)
            }
            lm_abi::INTRINSIC_BYTE_BUFFER_FINISH => {
                Instr::Native(lm_bytecode::NativeInstr::BbFinish)
            }
            lm_abi::INTRINSIC_BYTE_BUFFER_AT => Instr::Native(lm_bytecode::NativeInstr::BbAt),
            lm_abi::INTRINSIC_BYTE_BUFFER_FIND_FROM => {
                Instr::Native(lm_bytecode::NativeInstr::BbFindFrom)
            }
            lm_abi::INTRINSIC_TEXT_FIND_BYTE_INDEX => {
                Instr::Native(lm_bytecode::NativeInstr::TextFindByteIndex)
            }
            lm_abi::INTRINSIC_TEXT_AT_BYTE => Instr::Native(lm_bytecode::NativeInstr::TextAtByte),
            lm_abi::INTRINSIC_TEXT_TRIM => Instr::Native(lm_bytecode::NativeInstr::TextTrim),
            lm_abi::INTRINSIC_TEXT_TRIM_START => {
                Instr::Native(lm_bytecode::NativeInstr::TextTrimStart)
            }
            lm_abi::INTRINSIC_TEXT_TRIM_END => Instr::Native(lm_bytecode::NativeInstr::TextTrimEnd),
            lm_abi::INTRINSIC_TEXT_TO_LOWER_ASCII => {
                Instr::Native(lm_bytecode::NativeInstr::TextToLowerAscii)
            }
            lm_abi::INTRINSIC_TEXT_TO_UPPER_ASCII => {
                Instr::Native(lm_bytecode::NativeInstr::TextToUpperAscii)
            }
            lm_abi::INTRINSIC_TEXT_REPLACE => Instr::Native(lm_bytecode::NativeInstr::TextReplace),
            lm_abi::INTRINSIC_TEXT_PARSE_INT_STATUS => {
                Instr::Native(lm_bytecode::NativeInstr::TextParseIntStatus)
            }
            lm_abi::INTRINSIC_TEXT_PARSE_INT_VALUE => {
                Instr::Native(lm_bytecode::NativeInstr::TextParseIntValue)
            }
            lm_abi::INTRINSIC_TEXT_PAD_START => {
                Instr::Native(lm_bytecode::NativeInstr::TextPadStart)
            }
            lm_abi::INTRINSIC_TEXT_PAD_END => Instr::Native(lm_bytecode::NativeInstr::TextPadEnd),
            lm_abi::INTRINSIC_TEXT_PARSE_FLOAT_STATUS => {
                Instr::Numeric(lm_bytecode::NumericInstr::TextParseFloatStatus)
            }
            lm_abi::INTRINSIC_TEXT_PARSE_FLOAT_VALUE => {
                Instr::Numeric(lm_bytecode::NumericInstr::TextParseFloatValue)
            }
            lm_abi::INTRINSIC_BYTES_ENDS_WITH => {
                Instr::Native(lm_bytecode::NativeInstr::BytesEndsWith)
            }
            lm_abi::INTRINSIC_BYTES_CONTAINS => {
                Instr::Native(lm_bytecode::NativeInstr::BytesContains)
            }
            lm_abi::INTRINSIC_TEXT_HASH => Instr::Native(lm_bytecode::NativeInstr::TextHash),
            lm_abi::INTRINSIC_BYTES_HASH => Instr::Native(lm_bytecode::NativeInstr::BytesHash),
            lm_abi::INTRINSIC_HASH_COMBINE => Instr::Native(lm_bytecode::NativeInstr::HashCombine),
            lm_abi::INTRINSIC_HASH_UNORDERED_COMBINE => {
                Instr::Native(lm_bytecode::NativeInstr::HashUnorderedCombine)
            }
            lm_abi::INTRINSIC_TEXT_SPLIT => Instr::Native(lm_bytecode::NativeInstr::TextSplit),
            lm_abi::INTRINSIC_TEXT_LINES => Instr::Native(lm_bytecode::NativeInstr::TextLines),
            lm_abi::INTRINSIC_LIST_LEN => Instr::ListLen,
            lm_abi::INTRINSIC_LIST_AT => Instr::ListAt,
            lm_abi::INTRINSIC_LIST_GET => extended(ExtendedInstr::ListGet {
                ty: self.m.bc_ty(reply),
            }),
            lm_abi::INTRINSIC_LIST_PUSH => Instr::ListPush,
            lm_abi::INTRINSIC_MAP_LEN => Instr::MapLen,
            lm_abi::INTRINSIC_MAP_HAS => Instr::MapHas,
            lm_abi::INTRINSIC_MAP_AT => Instr::MapAt,
            lm_abi::INTRINSIC_MAP_GET => extended(ExtendedInstr::MapGet {
                ty: self.m.bc_ty(reply),
            }),
            lm_abi::INTRINSIC_MAP_PUT => Instr::MapPut {
                ty: self.m.bc_ty(reply),
                discard: false,
            },
            lm_abi::INTRINSIC_LIST_EPOCH => extended(ExtendedInstr::ListEpoch),
            lm_abi::INTRINSIC_LIST_ITER_LEN => extended(ExtendedInstr::ListIterLen),
            lm_abi::INTRINSIC_MAP_EPOCH => extended(ExtendedInstr::MapEpoch),
            lm_abi::INTRINSIC_MAP_ITER_LEN => extended(ExtendedInstr::MapIterLen),
            lm_abi::INTRINSIC_MAP_NEXT_INDEX => extended(ExtendedInstr::MapNextIndex),
            lm_abi::INTRINSIC_MAP_KEY_AT => extended(ExtendedInstr::MapKeyAt),
            lm_abi::INTRINSIC_MAP_VALUE_AT => extended(ExtendedInstr::MapValueAt),
            lm_abi::INTRINSIC_LIST_CAPACITY => extended(ExtendedInstr::ListCapacity),
            lm_abi::INTRINSIC_LIST_SET => extended(ExtendedInstr::ListSet),
            lm_abi::INTRINSIC_LIST_POP => extended(ExtendedInstr::ListPop {
                ty: self.m.bc_ty(reply),
            }),
            lm_abi::INTRINSIC_LIST_INSERT => extended(ExtendedInstr::ListInsert),
            lm_abi::INTRINSIC_LIST_REMOVE => extended(ExtendedInstr::ListRemove),
            lm_abi::INTRINSIC_LIST_SWAP_REMOVE => extended(ExtendedInstr::ListSwapRemove),
            lm_abi::INTRINSIC_LIST_RESERVE => extended(ExtendedInstr::ListReserve),
            lm_abi::INTRINSIC_LIST_TRUNCATE => extended(ExtendedInstr::ListTruncate),
            lm_abi::INTRINSIC_LIST_CONTAINS => extended(ExtendedInstr::ListContains),
            lm_abi::INTRINSIC_LIST_REORDER => extended(ExtendedInstr::ListReorder),
            lm_abi::INTRINSIC_MAP_REMOVE => extended(ExtendedInstr::MapRemove {
                ty: self.m.bc_ty(reply),
            }),
            lm_abi::INTRINSIC_MAP_CLEAR => extended(ExtendedInstr::MapClear),
            lm_abi::INTRINSIC_MAP_RESERVE => extended(ExtendedInstr::MapReserve),
            lm_abi::INTRINSIC_SYNTAX_TREE_ROOT => extended(ExtendedInstr::SyntaxTreeRoot),
            lm_abi::INTRINSIC_SYNTAX_KIND => extended(ExtendedInstr::SyntaxKind),
            lm_abi::INTRINSIC_SYNTAX_CATEGORY => extended(ExtendedInstr::SyntaxCategory),
            lm_abi::INTRINSIC_SYNTAX_RANGE_START => extended(ExtendedInstr::SyntaxRangeStart),
            lm_abi::INTRINSIC_SYNTAX_RANGE_END => extended(ExtendedInstr::SyntaxRangeEnd),
            lm_abi::INTRINSIC_SYNTAX_TEXT => extended(ExtendedInstr::SyntaxText),
            lm_abi::INTRINSIC_SYNTAX_CHILDREN => extended(ExtendedInstr::SyntaxChildren),
            lm_abi::INTRINSIC_SYNTAX_DETACH => extended(ExtendedInstr::SyntaxDetach),
            lm_abi::INTRINSIC_DYN_RENDER => extended(ExtendedInstr::DynRender),
            lm_abi::INTRINSIC_SYNTAX_BUILD_TOKEN => extended(ExtendedInstr::SyntaxBuildToken),
            lm_abi::INTRINSIC_SYNTAX_BUILD_TRIVIA => extended(ExtendedInstr::SyntaxBuildTrivia),
            lm_abi::INTRINSIC_SYNTAX_BUILD_NODE => extended(ExtendedInstr::SyntaxBuildNode),
            lm_abi::INTRINSIC_SYNTAX_TO_TREE => extended(ExtendedInstr::SyntaxToTree),
            lm_abi::INTRINSIC_PANIC => Instr::RaiseUserPanic,
            lm_abi::INTRINSIC_ASSERT_FAIL => Instr::RaiseAssertionFailed,
            lm_abi::INTRINSIC_RAISE_FAULT => Instr::RaiseFault,
            _ => unreachable!("the checker accepts only manifest intrinsics"),
        };
        self.emit(instr);
    }

    /// Lower one `case` expression. The scrutinee is stored first;
    /// each arm tests the pattern, binds, runs its body, and jumps to
    /// the join with one value. The checker proved exhaustiveness, so
    /// the last arm destructures without tests.
    fn lower_case(&mut self, scrut: &HExpr, scrut_slot: u32, arms: &[HArm], unit_valued: bool) {
        if !self.lower_operand(scrut) {
            return;
        }
        self.emit(Instr::StoreLocal(scrut_slot));
        let join_b = self.new_block();
        // The runtime backstop behind the static exhaustiveness
        // proof: the last arm keeps its tests, and a value no arm
        // accepts reaches an `Unreachable` fault instead of falling
        // through silently.
        let unreach_b = self.new_block();
        let last = arms.len() - 1;
        for (aidx, arm) in arms.iter().enumerate() {
            if aidx == last {
                self.lower_pattern(&arm.pattern, scrut_slot, Some(unreach_b));
                self.lower_branch(&arm.body, unit_valued, join_b);
            } else {
                let next_b = self.new_block();
                self.lower_pattern(&arm.pattern, scrut_slot, Some(next_b));
                self.lower_branch(&arm.body, unit_valued, join_b);
                self.switch_to(next_b);
            }
        }
        self.switch_to(unreach_b);
        self.emit(Instr::Unreachable);
        self.switch_to(join_b);
    }

    /// Lower one pattern over the value in `src`. With `fail` the
    /// tests jump there on a mismatch; without it the pattern only
    /// destructures, because the checker proved it must match.
    fn lower_pattern(&mut self, pattern: &HPattern, src: u32, fail: Option<u32>) {
        match pattern {
            HPattern::Wildcard => {}
            HPattern::Bind(slot) => {
                self.emit(Instr::LoadLocal(src));
                self.emit(Instr::StoreLocal(*slot));
            }
            HPattern::Int(v) => {
                if let Some(fail) = fail {
                    self.emit(Instr::LoadLocal(src));
                    self.emit(Instr::ConstInt(*v));
                    self.emit(Instr::EqInt);
                    self.emit(Instr::JumpIfFalse(fail));
                }
            }
            HPattern::Bool(v) => {
                if let Some(fail) = fail {
                    self.emit(Instr::LoadLocal(src));
                    if *v {
                        self.emit(Instr::JumpIfFalse(fail));
                    } else {
                        self.emit(Instr::JumpIfTrue(fail));
                    }
                }
            }
            HPattern::Char(value) => {
                if let Some(fail) = fail {
                    self.emit(Instr::LoadLocal(src));
                    self.emit(Instr::ConstChar(*value as u32));
                    self.emit(Instr::Native(lm_bytecode::NativeInstr::EqChar));
                    self.emit(Instr::JumpIfFalse(fail));
                }
            }
            HPattern::Str(v) => {
                if let Some(fail) = fail {
                    self.emit(Instr::LoadLocal(src));
                    let idx = self.m.intern_string(v);
                    self.emit(Instr::ConstStr(idx));
                    self.emit(Instr::Native(lm_bytecode::NativeInstr::EqStr));
                    self.emit(Instr::JumpIfFalse(fail));
                }
            }
            HPattern::Project {
                projection,
                ty,
                inner,
            } => {
                let slot = self.scratch_of(*ty);
                self.emit(Instr::LoadLocal(src));
                match projection {
                    Projection::AsCall(op) => {
                        let ty = self.m.bc_ty(*ty);
                        self.emit(Instr::AsCall { op: *op, ty });
                    }
                    Projection::CallArgs => self.emit(Instr::CallArgs),
                }
                self.emit(Instr::StoreLocal(slot));
                self.lower_pattern(inner, slot, fail);
            }
            HPattern::And(subs) => {
                for sub in subs {
                    self.lower_pattern(sub, src, fail);
                }
            }
            HPattern::Tuple { elems, elem_tys } => {
                // The type fixes the arity, so a tuple needs no test.
                for (index, sub) in elems.iter().enumerate() {
                    if matches!(sub, HPattern::Wildcard) {
                        continue;
                    }
                    let slot = self.scratch_of(elem_tys[index]);
                    self.emit(Instr::LoadLocal(src));
                    self.emit(Instr::TupleGet(index as u32));
                    self.emit(Instr::StoreLocal(slot));
                    self.lower_pattern(sub, slot, fail);
                }
            }
            HPattern::Ctor {
                ty,
                args,
                field_tys,
                ..
            } => {
                let bc = self.m.bc_ty(*ty);
                if let Some(fail) = fail {
                    self.emit(Instr::LoadLocal(src));
                    self.emit(Instr::IsType(bc));
                    self.emit(Instr::JumpIfFalse(fail));
                }
                let needs_fields = args.iter().any(|a| !matches!(a, HPattern::Wildcard));
                if needs_fields {
                    let cast_slot = self.scratch(bc);
                    self.emit(Instr::LoadLocal(src));
                    self.emit(Instr::CastType(bc));
                    self.emit(Instr::StoreLocal(cast_slot));
                    for (fidx, sub) in args.iter().enumerate() {
                        if matches!(sub, HPattern::Wildcard) {
                            continue;
                        }
                        let field_slot = self.scratch_of(field_tys[fidx]);
                        self.emit(Instr::LoadLocal(cast_slot));
                        let native_option_payload = fidx == 0
                            && matches!(
                                self.m.store.get(*ty),
                                Type::Inst(class, _) if class.0 == self.m.core.some_class
                            );
                        self.emit(if native_option_payload {
                            extended(ExtendedInstr::OptionPayload { ty: bc })
                        } else {
                            Instr::LoadField(fidx as u32)
                        });
                        self.emit(Instr::StoreLocal(field_slot));
                        self.lower_pattern(sub, field_slot, fail);
                    }
                }
            }
        }
    }

    /// Lower one `if` branch body and jump to the join block with one
    /// value on the stack.
    fn lower_branch(&mut self, body: &[HStmt], unit_valued: bool, join_b: u32) {
        let pushed = if unit_valued {
            let diverged = self.lower_block_stmt(body);
            if !diverged {
                self.emit(Instr::ConstUnit);
            }
            !diverged
        } else {
            self.lower_block_value(body)
        };
        if pushed {
            self.emit(Instr::Jump(join_b));
        }
    }
}

const INLINE_NODE_LIMIT: usize = 8;

/// Select one safe expression body for direct-call inlining.
fn inline_body(func: &HirFunc) -> Option<HExpr> {
    // A core import keeps its checked provider body for compile-time
    // inlining. The emitted import declaration still has no body.
    if (func.imported && !func.core)
        || func.effect_params != 0
        || !func.row.is_empty()
        || !func.captures.is_empty()
        || func.locals.len() != func.params.len()
    {
        return None;
    }
    let [HStmt::Expr(expr)] = func.body.as_slice() else {
        return None;
    };
    Some(expr.clone())
}

/// Replace ordered parameter reads with the caller expressions.
fn instantiate_inline(template: &HExpr, args: &[HExpr], bodies: &[Option<HExpr>]) -> Option<HExpr> {
    let mut next = 0;
    let mut nodes = 0;
    let mut active = Vec::new();
    let expr = instantiate_inline_expr(template, args, &mut next, &mut nodes, bodies, &mut active)?;
    (next == args.len()).then_some(expr)
}

fn instantiate_inline_expr(
    expr: &HExpr,
    args: &[HExpr],
    next: &mut usize,
    nodes: &mut usize,
    bodies: &[Option<HExpr>],
    active: &mut Vec<u32>,
) -> Option<HExpr> {
    *nodes += 1;
    if *nodes > INLINE_NODE_LIMIT {
        return None;
    }
    let mut out = expr.clone();
    match &mut out.kind {
        HExprKind::Local(slot) => {
            let index = *slot as usize;
            if index != *next || index >= args.len() {
                return None;
            }
            *next += 1;
            return Some(args[index].clone());
        }
        HExprKind::Unit
        | HExprKind::Int(_)
        | HExprKind::Float(_)
        | HExprKind::Char(_)
        | HExprKind::Str(_)
        | HExprKind::Bytes(_)
        | HExprKind::Bool(_) => {}
        HExprKind::Not(inner) | HExprKind::Neg(inner) => {
            **inner = instantiate_inline_expr(inner, args, next, nodes, bodies, active)?;
        }
        HExprKind::Binary { left, right, .. }
        | HExprKind::And(left, right)
        | HExprKind::Or(left, right) => {
            **left = instantiate_inline_expr(left, args, next, nodes, bodies, active)?;
            **right = instantiate_inline_expr(right, args, next, nodes, bodies, active)?;
        }
        HExprKind::Native { args: operands, .. } | HExprKind::Intrinsic { args: operands, .. } => {
            for operand in operands {
                *operand = instantiate_inline_expr(operand, args, next, nodes, bodies, active)?;
            }
        }
        HExprKind::Call {
            func,
            targs,
            rowargs,
            args: call_args,
        } => {
            if !targs.is_empty() || !rowargs.is_empty() || active.contains(func) {
                return None;
            }
            for arg in call_args.iter_mut() {
                *arg = instantiate_inline_expr(arg, args, next, nodes, bodies, active)?;
            }
            let template = bodies.get(*func as usize)?.as_ref()?;
            active.push(*func);
            let mut callee_next = 0;
            let expanded = instantiate_inline_expr(
                template,
                call_args,
                &mut callee_next,
                nodes,
                bodies,
                active,
            );
            active.pop();
            let expanded = expanded?;
            if callee_next != call_args.len() {
                return None;
            }
            return Some(expanded);
        }
        _ => return None,
    }
    Some(out)
}

/// Shift every local slot reference in a default expression by
/// `base`, and record one past the highest shifted slot in `max`.
fn shift_locals_expr(expr: &HExpr, base: u32, max: &mut u32) -> HExpr {
    let mut out = expr.clone();
    shift_expr_in_place(&mut out, base, max);
    out
}

fn shift_slot(slot: &mut u32, base: u32, max: &mut u32) {
    *max = (*max).max(*slot + 1);
    *slot += base;
}

fn shift_expr_in_place(expr: &mut HExpr, base: u32, max: &mut u32) {
    match &mut expr.kind {
        HExprKind::Local(slot) => shift_slot(slot, base, max),
        HExprKind::Unit
        | HExprKind::Int(_)
        | HExprKind::Float(_)
        | HExprKind::Char(_)
        | HExprKind::Str(_)
        | HExprKind::Bytes(_)
        | HExprKind::Bool(_)
        | HExprKind::Capture(_)
        | HExprKind::FunctionCode { .. }
        | HExprKind::ClassCode { .. } => {}
        HExprKind::CodeSource { code, .. } | HExprKind::CodeDefinition { code } => {
            shift_expr_in_place(code, base, max)
        }
        HExprKind::Not(inner) | HExprKind::Neg(inner) => shift_expr_in_place(inner, base, max),
        HExprKind::Binary { left, right, .. }
        | HExprKind::And(left, right)
        | HExprKind::Or(left, right) => {
            shift_expr_in_place(left, base, max);
            shift_expr_in_place(right, base, max);
        }
        HExprKind::Call { args, .. } | HExprKind::Construct { args, .. } => {
            for a in args {
                shift_expr_in_place(a, base, max);
            }
        }
        HExprKind::MethodCall { recv, args, .. } | HExprKind::InterfaceCall { recv, args, .. } => {
            shift_expr_in_place(recv, base, max);
            for a in args {
                shift_expr_in_place(a, base, max);
            }
        }
        HExprKind::FieldGet { recv, .. } => shift_expr_in_place(recv, base, max),
        HExprKind::MakeClosure { captures, .. } | HExprKind::MakeCallback { captures, .. } => {
            for c in captures {
                shift_expr_in_place(c, base, max);
            }
        }
        HExprKind::AsCallback(value) => shift_expr_in_place(value, base, max),
        HExprKind::Spawn { args, .. } => {
            for a in args {
                shift_expr_in_place(a, base, max);
            }
        }
        HExprKind::CallValue { callee, args } => {
            shift_expr_in_place(callee, base, max);
            for a in args {
                shift_expr_in_place(a, base, max);
            }
        }
        HExprKind::TupleLit(items) | HExprKind::ListLit(items) => {
            for i in items {
                shift_expr_in_place(i, base, max);
            }
        }
        HExprKind::TupleGet { tuple, .. } => shift_expr_in_place(tuple, base, max),
        HExprKind::IsType { value, .. } | HExprKind::CastType { value, .. } => {
            shift_expr_in_place(value, base, max)
        }
        HExprKind::MapLit(entries) => {
            for (k, v) in entries {
                shift_expr_in_place(k, base, max);
                shift_expr_in_place(v, base, max);
            }
        }
        HExprKind::Native { args, .. } | HExprKind::Intrinsic { args, .. } => {
            for a in args {
                shift_expr_in_place(a, base, max);
            }
        }
        HExprKind::Interp(parts) => {
            for part in parts {
                match part {
                    HInterpPart::Lit(_) => {}
                    HInterpPart::Native { value, .. } => shift_expr_in_place(value, base, max),
                    HInterpPart::Display { value, builder, .. } => {
                        shift_expr_in_place(value, base, max);
                        shift_slot(builder, base, max);
                    }
                }
            }
        }
        HExprKind::Block(body) => {
            for statement in body {
                shift_stmt_in_place(statement, base, max);
            }
        }
        HExprKind::Loop { body, result_slot } => {
            if let Some(slot) = result_slot {
                shift_slot(slot, base, max);
            }
            for statement in body {
                shift_stmt_in_place(statement, base, max);
            }
        }
        HExprKind::If { arms, else_body } => {
            for (cond, body) in arms {
                shift_expr_in_place(cond, base, max);
                for s in body {
                    shift_stmt_in_place(s, base, max);
                }
            }
            if let Some(body) = else_body {
                for s in body {
                    shift_stmt_in_place(s, base, max);
                }
            }
        }
        HExprKind::Case {
            scrut,
            scrut_slot,
            arms,
        } => {
            shift_expr_in_place(scrut, base, max);
            shift_slot(scrut_slot, base, max);
            for arm in arms {
                shift_pattern_in_place(&mut arm.pattern, base, max);
                for s in &mut arm.body {
                    shift_stmt_in_place(s, base, max);
                }
            }
        }
        HExprKind::Perform { args, .. } | HExprKind::PrepareWait { args, .. } => {
            for a in args {
                shift_expr_in_place(a, base, max);
            }
        }
        HExprKind::OpConst(_) => {}
        HExprKind::TableEdit { table, mock, .. } => {
            shift_expr_in_place(table, base, max);
            if let Some(mock) = mock {
                shift_expr_in_place(mock, base, max);
            }
        }
        HExprKind::CallArgs { call } => shift_expr_in_place(call, base, max),
        HExprKind::FaultCodeGet { fault }
        | HExprKind::FaultSiteGet { fault }
        | HExprKind::FaultTraceGet { fault } => shift_expr_in_place(fault, base, max),
        HExprKind::FaultDenied { reason } => shift_expr_in_place(reason, base, max),
        HExprKind::RequestOpName { request } => shift_expr_in_place(request, base, max),
    }
}

fn shift_pattern_in_place(pattern: &mut HPattern, base: u32, max: &mut u32) {
    match pattern {
        HPattern::Bind(slot) => shift_slot(slot, base, max),
        HPattern::Tuple { elems, .. } => {
            for sub in elems.iter_mut() {
                shift_pattern_in_place(sub, base, max);
            }
        }
        HPattern::Project { inner, .. } => shift_pattern_in_place(inner, base, max),
        HPattern::And(subs) => {
            for sub in subs.iter_mut() {
                shift_pattern_in_place(sub, base, max);
            }
        }
        HPattern::Ctor { args, .. } => {
            for a in args {
                shift_pattern_in_place(a, base, max);
            }
        }
        _ => {}
    }
}

fn shift_stmt_in_place(stmt: &mut HStmt, base: u32, max: &mut u32) {
    match stmt {
        HStmt::Assign { slot, value } => {
            shift_slot(slot, base, max);
            shift_expr_in_place(value, base, max);
        }
        HStmt::AssignField { recv, value, .. } => {
            shift_expr_in_place(recv, base, max);
            shift_expr_in_place(value, base, max);
        }
        HStmt::While { cond, body } => {
            shift_expr_in_place(cond, base, max);
            for s in body {
                shift_stmt_in_place(s, base, max);
            }
        }
        HStmt::For {
            source,
            bindings,
            kind,
            body,
        } => {
            shift_expr_in_place(source, base, max);
            for slot in bindings {
                shift_slot(slot, base, max);
            }
            match kind {
                HForKind::List {
                    source_slot,
                    index_slot,
                    epoch_slot,
                    ..
                }
                | HForKind::Map {
                    source_slot,
                    index_slot,
                    epoch_slot,
                    ..
                } => {
                    shift_slot(source_slot, base, max);
                    shift_slot(index_slot, base, max);
                    shift_slot(epoch_slot, base, max);
                }
                HForKind::Text {
                    source_slot,
                    cursor_slot,
                    ..
                } => {
                    shift_slot(source_slot, base, max);
                    shift_slot(cursor_slot, base, max);
                }
                HForKind::Range {
                    source_slot,
                    cursor_slot,
                    stop_slot,
                } => {
                    shift_slot(source_slot, base, max);
                    shift_slot(cursor_slot, base, max);
                    shift_slot(stop_slot, base, max);
                }
                HForKind::Generic {
                    source_slot,
                    iterator_slot,
                    option_slot,
                    item_slot,
                    iterator,
                    next,
                    ..
                } => {
                    shift_slot(source_slot, base, max);
                    shift_slot(iterator_slot, base, max);
                    shift_slot(option_slot, base, max);
                    if let Some(slot) = item_slot {
                        shift_slot(slot, base, max);
                    }
                    shift_expr_in_place(iterator, base, max);
                    shift_expr_in_place(next, base, max);
                }
            }
            for s in body {
                shift_stmt_in_place(s, base, max);
            }
        }
        HStmt::Return { value } => {
            if let Some(v) = value {
                shift_expr_in_place(v, base, max);
            }
        }
        HStmt::Break { value } => {
            if let Some(value) = value {
                shift_expr_in_place(value, base, max);
            }
        }
        HStmt::Continue => {}
        HStmt::Expr(e) => shift_expr_in_place(e, base, max),
    }
}

fn binary_instr(op: BinOp, operand_ty: TypeId) -> Instr {
    match op {
        BinOp::Add => Instr::Add,
        BinOp::Sub => Instr::Sub,
        BinOp::Mul => Instr::Mul,
        BinOp::Div => Instr::Div,
        BinOp::Rem => Instr::Rem,
        BinOp::BitAnd => Instr::Numeric(lm_bytecode::NumericInstr::IntBitAnd),
        BinOp::BitOr => Instr::Numeric(lm_bytecode::NumericInstr::IntBitOr),
        BinOp::BitXor => Instr::Numeric(lm_bytecode::NumericInstr::IntBitXor),
        BinOp::Shl => Instr::Numeric(lm_bytecode::NumericInstr::IntShl),
        BinOp::Shr => Instr::Numeric(lm_bytecode::NumericInstr::IntShr),
        BinOp::Ushr => Instr::Numeric(lm_bytecode::NumericInstr::IntUshr),
        BinOp::Lt => Instr::LtInt,
        BinOp::Le => Instr::LeInt,
        BinOp::Gt => Instr::GtInt,
        BinOp::Ge => Instr::GeInt,
        BinOp::Eq => match operand_ty {
            BOOL => Instr::EqBool,
            STRING => Instr::Native(lm_bytecode::NativeInstr::EqStr),
            DIGEST => Instr::EqDigest,
            INT | NEVER | UNIT => Instr::EqInt,
            _ => Instr::EqRef,
        },
        BinOp::Ne => match operand_ty {
            BOOL => Instr::NeBool,
            STRING => Instr::Native(lm_bytecode::NativeInstr::NeStr),
            DIGEST => Instr::NeDigest,
            INT | NEVER | UNIT => Instr::NeInt,
            _ => Instr::NeRef,
        },
    }
}

fn interp_native_instr(kind: HInterpNative) -> Instr {
    match kind {
        HInterpNative::Text => Instr::Native(lm_bytecode::NativeInstr::SbAppendStr),
        HInterpNative::Int => Instr::Native(lm_bytecode::NativeInstr::SbAppendInt),
        HInterpNative::Float => Instr::Numeric(lm_bytecode::NumericInstr::SbAppendFloat),
        HInterpNative::Bool => Instr::Native(lm_bytecode::NativeInstr::SbAppendBool),
        HInterpNative::Char => Instr::Native(lm_bytecode::NativeInstr::SbAppendChar),
    }
}

fn lower_func(m: &mut ModLowerer<'_>, func: &HirFunc) -> Func {
    let params: Vec<u32> = func.params.iter().map(|t| m.bc_ty(*t)).collect();
    let ret = m.bc_ty(func.ret);
    let row = m.bc_row(&func.row);
    let captures: Vec<u32> = func.captures.iter().map(|t| m.bc_ty(*t)).collect();
    if func.imported {
        // An imported function is a declaration: the signature only.
        // The linker replaces it with the provider definition.
        return Func {
            name: func.name.clone(),
            type_params: func.type_params,
            effect_params: func.effect_params,
            params: params.clone(),
            param_muts: func.param_muts.clone(),
            param_names: func.param_names.clone(),
            ret,
            row,
            captures,
            local_types: params,
            blocks: vec![],
        };
    }
    // The declared checker types of every local slot seed the table;
    // scratch slots append their true types during lowering.
    let base_types: Vec<u32> = func.locals.iter().map(|t| m.bc_ty(*t)).collect();
    let mut lowerer = Lowerer::new(m, base_types);
    let unit_ret = func.ret == UNIT;
    let pushed = if unit_ret {
        let diverged = lowerer.lower_block_stmt(&func.body);
        if !diverged {
            lowerer.emit(Instr::ConstUnit);
        }
        !diverged
    } else {
        lowerer.lower_block_value(&func.body)
    };
    let local_types = lowerer.local_types.clone();
    let blocks = lowerer.finish(pushed);
    Func {
        name: func.name.clone(),
        type_params: func.type_params,
        effect_params: func.effect_params,
        params,
        param_muts: func.param_muts.clone(),
        param_names: func.param_names.clone(),
        ret,
        row,
        captures,
        local_types,
        blocks,
    }
}

/// Synthesize the `<new>` construction function for one class:
/// allocate, evaluate defaults, run `init` or store the case fields,
/// and return the instance.
fn lower_new_func(m: &mut ModLowerer<'_>, class: &HirClass, cidx: u32) -> Func {
    if class.imported {
        if class.kind == ClassKind::EnumParent || class.native_repr == Some(NativeRepr::Text) {
            return Func {
                name: format!("<new {}>", class.name),
                type_params: 0,
                effect_params: 0,
                params: vec![],
                param_muts: vec![],
                param_names: vec![],
                ret: m.intern_type(BcType::Unit),
                row: vec![],
                captures: vec![],
                local_types: vec![],
                blocks: vec![],
            };
        }
        if let Some((type_params, params, ret)) = imported_native_ctor(m, class, cidx) {
            return Func {
                name: format!("<new {}>", class.name),
                type_params,
                effect_params: 0,
                param_muts: vec![false; params.len()],
                param_names: vec![],
                local_types: params.clone(),
                params,
                ret,
                row: vec![],
                captures: vec![],
                blocks: vec![],
            };
        }
        // An imported class declares its construction function and
        // carries no body. The provider evaluates its own defaults.
        let params: Vec<u32> = class.ctor_params.iter().map(|t| m.bc_ty(*t)).collect();
        let self_bc = if class.type_params == 0 {
            m.intern_type(BcType::Class(cidx))
        } else {
            let var_tys: Vec<u32> = (0..class.type_params)
                .map(|i| m.intern_type(BcType::Var(i)))
                .collect();
            m.intern_type(BcType::Inst(cidx, var_tys))
        };
        let row = m.bc_row(&class.ctor_row);
        return Func {
            name: format!("<new {}>", class.name),
            type_params: class.type_params,
            effect_params: 0,
            params: params.clone(),
            param_muts: class.ctor_param_muts.clone(),
            param_names: class.ctor_param_names.clone(),
            ret: self_bc,
            row,
            captures: vec![],
            local_types: params,
            blocks: vec![],
        };
    }
    if class.kind == ClassKind::EnumParent || class.native_repr == Some(NativeRepr::Text) {
        // An abstract enum parent is never constructed. Its `<new>`
        // slot only keeps the index arithmetic dense.
        return Func {
            name: format!("<new {}>", class.name),
            type_params: 0,
            effect_params: 0,
            params: vec![],
            param_muts: vec![],
            param_names: vec![],
            ret: m.intern_type(BcType::Unit),
            row: vec![],
            captures: vec![],
            local_types: vec![],
            blocks: vec![vec![Instr::ConstUnit, Instr::Return]],
        };
    }
    if matches!(
        class.native_repr,
        Some(
            NativeRepr::TcpResource
                | NativeRepr::FileHandle
                | NativeRepr::TcpStream
                | NativeRepr::TcpListener
                | NativeRepr::TlsStream
                | NativeRepr::UdpSocket
                | NativeRepr::Artifact
                | NativeRepr::VerifiedModule
                | NativeRepr::FunctionCode
                | NativeRepr::ClassCode
                | NativeRepr::SlotSpec
                | NativeRepr::CodeInstance
                | NativeRepr::Slot
                | NativeRepr::FunctionDef
                | NativeRepr::ClassDef
                | NativeRepr::FunctionBinding
                | NativeRepr::ClassBinding
                | NativeRepr::DynValue
        )
    ) {
        let ret = if class.type_params == 0 {
            m.intern_type(BcType::Class(cidx))
        } else {
            let args = (0..class.type_params)
                .map(|index| m.intern_type(BcType::Var(index)))
                .collect();
            m.intern_type(BcType::Inst(cidx, args))
        };
        return Func {
            name: format!("<new {}>", class.name),
            type_params: class.type_params,
            effect_params: 0,
            params: vec![],
            param_muts: vec![],
            param_names: vec![],
            ret,
            row: vec![],
            captures: vec![],
            local_types: vec![],
            blocks: vec![vec![Instr::Unreachable]],
        };
    }
    if class.native_repr == Some(NativeRepr::Unit) {
        return Func {
            name: format!("<new {}>", class.name),
            type_params: 0,
            effect_params: 0,
            params: vec![],
            param_muts: vec![],
            param_names: vec![],
            ret: m.intern_type(BcType::Unit),
            row: vec![],
            captures: vec![],
            local_types: vec![],
            blocks: vec![vec![Instr::ConstUnit, Instr::Return]],
        };
    }
    if let Some(NativeRepr::Tuple(arity)) = class.native_repr {
        let params: Vec<u32> = (0..arity)
            .map(|index| m.intern_type(BcType::Var(index as u32)))
            .collect();
        let tuple = m.intern_type(BcType::Tuple(params.clone()));
        let mut block: Vec<Instr> = (0..arity)
            .map(|index| Instr::LoadLocal(index as u32))
            .collect();
        block.push(Instr::TupleNew {
            ty: tuple,
            count: arity as u32,
        });
        block.push(Instr::Return);
        return Func {
            name: format!("<new {}>", class.name),
            type_params: arity as u32,
            effect_params: 0,
            params: params.clone(),
            param_muts: vec![false; arity as usize],
            param_names: vec![],
            ret: tuple,
            row: vec![],
            captures: vec![],
            local_types: params,
            blocks: vec![block],
        };
    }
    if class.native_repr == Some(NativeRepr::Int) {
        let int = m.intern_type(BcType::Int);
        return Func {
            name: format!("<new {}>", class.name),
            type_params: 0,
            effect_params: 0,
            params: vec![],
            param_muts: vec![],
            param_names: vec![],
            ret: int,
            row: vec![],
            captures: vec![],
            local_types: vec![],
            blocks: vec![vec![Instr::ConstInt(0), Instr::Return]],
        };
    }
    if class.native_repr == Some(NativeRepr::Float) {
        let float = m.intern_type(BcType::Float);
        return Func {
            name: format!("<new {}>", class.name),
            type_params: 0,
            effect_params: 0,
            params: vec![],
            param_muts: vec![],
            param_names: vec![],
            ret: float,
            row: vec![],
            captures: vec![],
            local_types: vec![],
            blocks: vec![vec![Instr::ConstFloat(0), Instr::Return]],
        };
    }
    if class.native_repr == Some(NativeRepr::Bool) {
        let bool_ty = m.intern_type(BcType::Bool);
        return Func {
            name: format!("<new {}>", class.name),
            type_params: 0,
            effect_params: 0,
            params: vec![],
            param_muts: vec![],
            param_names: vec![],
            ret: bool_ty,
            row: vec![],
            captures: vec![],
            local_types: vec![],
            blocks: vec![vec![Instr::ConstBool(false), Instr::Return]],
        };
    }
    if class.native_repr == Some(NativeRepr::String) {
        let string_ty = m.intern_type(BcType::Str);
        let empty = m.intern_string("");
        return Func {
            name: format!("<new {}>", class.name),
            type_params: 0,
            effect_params: 0,
            params: vec![],
            param_muts: vec![],
            param_names: vec![],
            ret: string_ty,
            row: vec![],
            captures: vec![],
            local_types: vec![],
            blocks: vec![vec![Instr::ConstStr(empty), Instr::Return]],
        };
    }
    if class.native_repr == Some(NativeRepr::Substring) {
        let substring_ty = m.intern_type(BcType::Class(cidx));
        let empty = m.intern_string("");
        return Func {
            name: format!("<new {}>", class.name),
            type_params: 0,
            effect_params: 0,
            params: vec![],
            param_muts: vec![],
            param_names: vec![],
            ret: substring_ty,
            row: vec![],
            captures: vec![],
            local_types: vec![],
            blocks: vec![vec![
                Instr::ConstStr(empty),
                Instr::ConstInt(0),
                Instr::ConstInt(0),
                Instr::Native(lm_bytecode::NativeInstr::TextSlice),
                Instr::Return,
            ]],
        };
    }
    if class.native_repr == Some(NativeRepr::Char) {
        let char_ty = m.intern_type(BcType::Class(cidx));
        let space = m.intern_string(" ");
        return Func {
            name: format!("<new {}>", class.name),
            type_params: 0,
            effect_params: 0,
            params: vec![],
            param_muts: vec![],
            param_names: vec![],
            ret: char_ty,
            row: vec![],
            captures: vec![],
            local_types: vec![],
            blocks: vec![vec![
                Instr::ConstStr(space),
                Instr::ConstInt(0),
                Instr::Native(lm_bytecode::NativeInstr::TextAt),
                Instr::Return,
            ]],
        };
    }
    if class.native_repr == Some(NativeRepr::Bytes) {
        let bytes_ty = m.intern_type(BcType::Bytes);
        let empty = m.intern_string("");
        return Func {
            name: format!("<new {}>", class.name),
            type_params: 0,
            effect_params: 0,
            params: vec![],
            param_muts: vec![],
            param_names: vec![],
            ret: bytes_ty,
            row: vec![],
            captures: vec![],
            local_types: vec![],
            blocks: vec![vec![
                Instr::ConstStr(empty),
                Instr::Native(lm_bytecode::NativeInstr::BytesNew),
                Instr::Return,
            ]],
        };
    }
    if class.native_repr == Some(NativeRepr::StringBuilder) {
        let builder = m.intern_type(BcType::Class(cidx));
        return Func {
            name: format!("<new {}>", class.name),
            type_params: 0,
            effect_params: 0,
            params: vec![],
            param_muts: vec![],
            param_names: vec![],
            ret: builder,
            row: vec![],
            captures: vec![],
            local_types: vec![],
            blocks: vec![vec![
                Instr::Native(lm_bytecode::NativeInstr::SbNew),
                Instr::Return,
            ]],
        };
    }
    if class.native_repr == Some(NativeRepr::ByteBuffer) {
        let buffer = m.intern_type(BcType::Class(cidx));
        return Func {
            name: format!("<new {}>", class.name),
            type_params: 0,
            effect_params: 0,
            params: vec![],
            param_muts: vec![],
            param_names: vec![],
            ret: buffer,
            row: vec![],
            captures: vec![],
            local_types: vec![],
            blocks: vec![vec![
                Instr::Native(lm_bytecode::NativeInstr::BbNew),
                Instr::Return,
            ]],
        };
    }
    if class.native_repr == Some(NativeRepr::List) {
        let element = m.intern_type(BcType::Var(0));
        let list = m.intern_type(BcType::List(element));
        return Func {
            name: format!("<new {}>", class.name),
            type_params: 1,
            effect_params: 0,
            params: vec![],
            param_muts: vec![],
            param_names: vec![],
            ret: list,
            row: vec![],
            captures: vec![],
            local_types: vec![],
            blocks: vec![vec![Instr::ListNew { ty: list, count: 0 }, Instr::Return]],
        };
    }
    if class.native_repr == Some(NativeRepr::Map) {
        let key = m.intern_type(BcType::Var(0));
        let value = m.intern_type(BcType::Var(1));
        let map = m.intern_type(BcType::Map(key, value));
        return Func {
            name: format!("<new {}>", class.name),
            type_params: 2,
            effect_params: 0,
            params: vec![],
            param_muts: vec![],
            param_names: vec![],
            ret: map,
            row: vec![],
            captures: vec![],
            local_types: vec![],
            blocks: vec![vec![Instr::MapNew { ty: map, count: 0 }, Instr::Return]],
        };
    }
    let params: Vec<u32> = class.ctor_params.iter().map(|t| m.bc_ty(*t)).collect();
    let type_params = class.type_params;
    let vars: Vec<TypeId> = Vec::new();
    let _ = vars;
    let (self_bc, app) = if type_params == 0 {
        (m.intern_type(BcType::Class(cidx)), None)
    } else {
        let var_tys: Vec<u32> = (0..type_params)
            .map(|i| m.intern_type(BcType::Var(i)))
            .collect();
        let inst = m.intern_type(BcType::Inst(cidx, var_tys.clone()));
        let app = m.intern_app(var_tys, vec![]);
        (inst, Some(app))
    };
    let row = m.bc_row(&class.ctor_row);
    if cidx == m.core.some_class {
        return Func {
            name: format!("<new {}>", class.name),
            type_params,
            effect_params: 0,
            params: params.clone(),
            param_muts: class.ctor_param_muts.clone(),
            param_names: class.ctor_param_names.clone(),
            ret: self_bc,
            row,
            captures: vec![],
            local_types: params,
            blocks: vec![vec![
                Instr::LoadLocal(0),
                extended(ExtendedInstr::OptionSome { ty: self_bc }),
                Instr::Return,
            ]],
        };
    }
    if cidx == m.core.none_class {
        return Func {
            name: format!("<new {}>", class.name),
            type_params,
            effect_params: 0,
            params: params.clone(),
            param_muts: class.ctor_param_muts.clone(),
            param_names: class.ctor_param_names.clone(),
            ret: self_bc,
            row,
            captures: vec![],
            local_types: params,
            blocks: vec![vec![
                extended(ExtendedInstr::OptionNone { ty: self_bc }),
                Instr::Return,
            ]],
        };
    }
    let self_slot = params.len() as u32;
    // The slot table starts with the constructor parameters and the
    // `self` scratch slot.
    let mut base_types = params.clone();
    base_types.push(self_bc);
    let mut lowerer = Lowerer::new(m, base_types);
    match app {
        None => lowerer.emit(Instr::New(cidx)),
        Some(app) => lowerer.emit(Instr::NewG { class: cidx, app }),
    }
    lowerer.emit(Instr::StoreLocal(self_slot));
    if class.ctor_kind == CtorKind::CaseFields {
        for fidx in 0..class.ctor_params.len() {
            lowerer.emit(Instr::LoadLocal(self_slot));
            lowerer.emit(Instr::LoadLocal(fidx as u32));
            lowerer.emit(Instr::StoreField(fidx as u32));
        }
    } else {
        for (fidx, default) in class.defaults.iter().enumerate() {
            if let Some(expr) = default {
                // A default was checked in its own local space. Move
                // its temporary slots into fresh scratch slots of the
                // `<new>` function, with their checker-declared types.
                let base = lowerer.local_types.len() as u32;
                let mut max_slot = 0;
                let shifted = shift_locals_expr(expr, base, &mut max_slot);
                // The shifted temporaries occupy `base .. base + max_slot`,
                // because `max_slot` counts in the pre-shift space.
                let default_types = &class.default_locals[fidx];
                for ty in default_types.iter().take(max_slot as usize) {
                    lowerer.scratch_of(*ty);
                }
                lowerer.emit(Instr::LoadLocal(self_slot));
                lowerer.lower_expr(&shifted);
                lowerer.emit(Instr::StoreField(fidx as u32));
            }
        }
        if let Some(init) = class.init {
            lowerer.emit(Instr::LoadLocal(self_slot));
            for i in 0..self_slot {
                lowerer.emit(Instr::LoadLocal(i));
            }
            match app {
                None => lowerer.emit(Instr::Call(init)),
                Some(app) => lowerer.emit(Instr::CallG { func: init, app }),
            }
            lowerer.emit(Instr::Pop);
        }
    }
    lowerer.emit(Instr::LoadLocal(self_slot));
    if class.is_frozen {
        lowerer.emit(extended(ExtendedInstr::SealInstance));
    }
    let local_types = lowerer.local_types.clone();
    let blocks = lowerer.finish(true);
    Func {
        name: format!("<new {}>", class.name),
        type_params,
        effect_params: 0,
        params,
        param_muts: class.ctor_param_muts.clone(),
        param_names: class.ctor_param_names.clone(),
        ret: self_bc,
        row,
        captures: vec![],
        local_types,
        blocks,
    }
}

/// Return the constructor contract of one native value class.
fn imported_native_ctor(
    m: &mut ModLowerer<'_>,
    class: &HirClass,
    cidx: u32,
) -> Option<(u32, Vec<u32>, u32)> {
    let contract = match class.native_repr? {
        NativeRepr::Unit => (0, Vec::new(), m.intern_type(BcType::Unit)),
        NativeRepr::Int => (0, Vec::new(), m.intern_type(BcType::Int)),
        NativeRepr::Float => (0, Vec::new(), m.intern_type(BcType::Float)),
        NativeRepr::Bool => (0, Vec::new(), m.intern_type(BcType::Bool)),
        NativeRepr::String => (0, Vec::new(), m.intern_type(BcType::Str)),
        NativeRepr::Bytes => (0, Vec::new(), m.intern_type(BcType::Bytes)),
        NativeRepr::Tuple(arity) => {
            let params: Vec<u32> = (0..arity)
                .map(|index| m.intern_type(BcType::Var(index as u32)))
                .collect();
            let ty = m.intern_type(BcType::Tuple(params.clone()));
            (arity as u32, params, ty)
        }
        NativeRepr::List => {
            let element = m.intern_type(BcType::Var(0));
            (1, Vec::new(), m.intern_type(BcType::List(element)))
        }
        NativeRepr::Map => {
            let key = m.intern_type(BcType::Var(0));
            let value = m.intern_type(BcType::Var(1));
            (2, Vec::new(), m.intern_type(BcType::Map(key, value)))
        }
        NativeRepr::Text => return None,
        _ => {
            let ty = if class.type_params == 0 {
                m.intern_type(BcType::Class(cidx))
            } else {
                let args = (0..class.type_params)
                    .map(|index| m.intern_type(BcType::Var(index)))
                    .collect();
                m.intern_type(BcType::Inst(cidx, args))
            };
            (class.type_params, Vec::new(), ty)
        }
    };
    Some(contract)
}

/// Synthesize one closure-compatible dispatcher for a late class.
/// The target constructor remains version-specific in the class slot.
fn lower_new_dispatch_func(m: &mut ModLowerer<'_>, class: &HirClass, cidx: u32) -> Func {
    let params: Vec<u32> = class.ctor_params.iter().map(|ty| m.bc_ty(*ty)).collect();
    let ret = if class.type_params == 0 {
        m.intern_type(BcType::Class(cidx))
    } else {
        let variables: Vec<u32> = (0..class.type_params)
            .map(|index| m.intern_type(BcType::Var(index)))
            .collect();
        m.intern_type(BcType::Inst(cidx, variables))
    };
    let app = if class.type_params == 0 {
        NO_APP
    } else {
        let variables = (0..class.type_params)
            .map(|index| m.intern_type(BcType::Var(index)))
            .collect();
        m.intern_app(variables, vec![])
    };
    let slot = m.class_slots[&cidx];
    let mut body = Vec::with_capacity(params.len() + 2);
    for index in 0..params.len() as u32 {
        body.push(Instr::LoadLocal(index));
    }
    body.push(extended(ExtendedInstr::NewSlot { slot, app }));
    body.push(Instr::Return);
    Func {
        name: format!("<late new {}>", class.name),
        type_params: class.type_params,
        effect_params: 0,
        params: params.clone(),
        param_muts: class.ctor_param_muts.clone(),
        param_names: class.ctor_param_names.clone(),
        ret,
        row: m.bc_row(&class.ctor_row),
        captures: vec![],
        local_types: params,
        blocks: vec![body],
    }
}

/// Tables needed to calculate one instruction's operand effect.
trait StackEffectTables {
    fn function_param_count(&self, function: u32) -> usize;
    fn interface_param_count(&self, interface: u32, method: u32) -> usize;
    fn slot_param_count(&self, slot: u32) -> usize;
}

impl StackEffectTables for ModLowerer<'_> {
    fn function_param_count(&self, function: u32) -> usize {
        self.func_param_counts
            .get(function as usize)
            .copied()
            .unwrap_or(0)
    }

    fn interface_param_count(&self, interface: u32, method: u32) -> usize {
        self.interfaces
            .get(interface as usize)
            .and_then(|interface| interface.methods.get(method as usize))
            .map(|method| method.params.len())
            .unwrap_or(0)
    }

    fn slot_param_count(&self, slot: u32) -> usize {
        self.slot_param_counts
            .get(slot as usize)
            .copied()
            .unwrap_or(0)
    }
}

impl StackEffectTables for Module {
    fn function_param_count(&self, function: u32) -> usize {
        self.funcs
            .get(function as usize)
            .map(|function| function.params.len())
            .unwrap_or(0)
    }

    fn interface_param_count(&self, interface: u32, method: u32) -> usize {
        self.interfaces
            .get(interface as usize)
            .and_then(|interface| interface.methods.get(method as usize))
            .map(|method| method.params.len())
            .unwrap_or(0)
    }

    fn slot_param_count(&self, slot: u32) -> usize {
        self.slots
            .get(slot as usize)
            .map(|slot| match &slot.contract {
                lm_bytecode::SlotContract::Function(contract)
                | lm_bytecode::SlotContract::Method(contract) => contract.params.len(),
                lm_bytecode::SlotContract::Class { constructor, .. } => constructor.params.len(),
                lm_bytecode::SlotContract::Value { .. }
                | lm_bytecode::SlotContract::Process { .. } => 0,
            })
            .unwrap_or(0)
    }
}

/// Count the values an instruction pops and pushes.
fn stack_effect(tables: &impl StackEffectTables, instr: &Instr) -> (usize, usize) {
    match instr {
        Instr::ConstUnit
        | Instr::ConstBool(_)
        | Instr::ConstInt(_)
        | Instr::ConstFloat(_)
        | Instr::ConstChar(_)
        | Instr::ConstStr(_)
        | Instr::ConstBytes(_)
        | Instr::LoadLocal(_)
        | Instr::LoadCapture(_)
        | Instr::New(_)
        | Instr::NewG { .. }
        | Instr::Native(lm_bytecode::NativeInstr::SbNew)
        | Instr::Native(lm_bytecode::NativeInstr::BbNew) => (0, 1),
        Instr::StoreLocal(_) | Instr::Pop => (1, 0),
        Instr::Add
        | Instr::Sub
        | Instr::Mul
        | Instr::Div
        | Instr::Rem
        | Instr::LtInt
        | Instr::LeInt
        | Instr::GtInt
        | Instr::GeInt
        | Instr::EqInt
        | Instr::NeInt
        | Instr::EqBool
        | Instr::NeBool
        | Instr::Native(lm_bytecode::NativeInstr::EqStr)
        | Instr::Native(lm_bytecode::NativeInstr::NeStr)
        | Instr::EqRef
        | Instr::EqValue
        | Instr::NeValue
        | Instr::NeRef
        | Instr::Native(lm_bytecode::NativeInstr::StrConcat)
        | Instr::Native(lm_bytecode::NativeInstr::StrStartsWith)
        | Instr::Native(lm_bytecode::NativeInstr::StrEndsWith)
        | Instr::Native(lm_bytecode::NativeInstr::StrContains)
        | Instr::Native(lm_bytecode::NativeInstr::StrFindIndex)
        | Instr::Native(lm_bytecode::NativeInstr::TextFindByteIndex)
        | Instr::Native(lm_bytecode::NativeInstr::TextAtByte)
        | Instr::Native(lm_bytecode::NativeInstr::TextParseIntStatus)
        | Instr::Native(lm_bytecode::NativeInstr::TextParseIntValue)
        | Instr::Native(lm_bytecode::NativeInstr::TextPadStart)
        | Instr::Native(lm_bytecode::NativeInstr::TextPadEnd)
        | Instr::Native(lm_bytecode::NativeInstr::BytesEndsWith)
        | Instr::Native(lm_bytecode::NativeInstr::BytesContains)
        | Instr::Native(lm_bytecode::NativeInstr::TextSplit)
        | Instr::Native(lm_bytecode::NativeInstr::BytesAt)
        | Instr::Native(lm_bytecode::NativeInstr::BytesGet)
        | Instr::Native(lm_bytecode::NativeInstr::BytesConcat)
        | Instr::Native(lm_bytecode::NativeInstr::BytesStartsWith)
        | Instr::Native(lm_bytecode::NativeInstr::BytesFindIndex)
        | Instr::Native(lm_bytecode::NativeInstr::EqBytes)
        | Instr::Native(lm_bytecode::NativeInstr::NeBytes)
        | Instr::Native(lm_bytecode::NativeInstr::BbExtend)
        | Instr::Native(lm_bytecode::NativeInstr::BbReserve)
        | Instr::Native(lm_bytecode::NativeInstr::BbAt)
        | Instr::Native(lm_bytecode::NativeInstr::TextAt)
        | Instr::Native(lm_bytecode::NativeInstr::TextIsBoundary)
        | Instr::Native(lm_bytecode::NativeInstr::TextLt)
        | Instr::Native(lm_bytecode::NativeInstr::TextLe)
        | Instr::Native(lm_bytecode::NativeInstr::TextGt)
        | Instr::Native(lm_bytecode::NativeInstr::TextGe)
        | Instr::Native(lm_bytecode::NativeInstr::EqChar)
        | Instr::Native(lm_bytecode::NativeInstr::NeChar)
        | Instr::Native(lm_bytecode::NativeInstr::LtChar)
        | Instr::Native(lm_bytecode::NativeInstr::LeChar)
        | Instr::Native(lm_bytecode::NativeInstr::GtChar)
        | Instr::Native(lm_bytecode::NativeInstr::GeChar)
        | Instr::Native(lm_bytecode::NativeInstr::LtBytes)
        | Instr::Native(lm_bytecode::NativeInstr::LeBytes)
        | Instr::Native(lm_bytecode::NativeInstr::GtBytes)
        | Instr::Native(lm_bytecode::NativeInstr::GeBytes)
        | Instr::Native(lm_bytecode::NativeInstr::HashCombine)
        | Instr::Native(lm_bytecode::NativeInstr::HashUnorderedCombine)
        | Instr::Native(lm_bytecode::NativeInstr::SbAppendChar) => (2, 1),
        Instr::Neg
        | Instr::Not
        | Instr::LoadField(_)
        | Instr::TupleGet(_)
        | Instr::IsType(_)
        | Instr::CastType(_)
        | Instr::ListLen
        | Instr::MapLen
        | Instr::Native(lm_bytecode::NativeInstr::SbBuild)
        | Instr::Native(lm_bytecode::NativeInstr::SbLen)
        | Instr::Native(lm_bytecode::NativeInstr::SbClear)
        | Instr::Native(lm_bytecode::NativeInstr::BbLen)
        | Instr::Native(lm_bytecode::NativeInstr::BbBuild)
        | Instr::Native(lm_bytecode::NativeInstr::BbClear)
        | Instr::Native(lm_bytecode::NativeInstr::StrByteLen)
        | Instr::Native(lm_bytecode::NativeInstr::StrCharCount)
        | Instr::Native(lm_bytecode::NativeInstr::BytesNew)
        | Instr::Native(lm_bytecode::NativeInstr::BytesLen)
        | Instr::Native(lm_bytecode::NativeInstr::BytesText)
        | Instr::Native(lm_bytecode::NativeInstr::BytesHex)
        | Instr::Native(lm_bytecode::NativeInstr::BytesIsUtf8)
        | Instr::Native(lm_bytecode::NativeInstr::TextBytes)
        | Instr::Native(lm_bytecode::NativeInstr::TextTrim)
        | Instr::Native(lm_bytecode::NativeInstr::TextTrimStart)
        | Instr::Native(lm_bytecode::NativeInstr::TextTrimEnd)
        | Instr::Native(lm_bytecode::NativeInstr::TextToLowerAscii)
        | Instr::Native(lm_bytecode::NativeInstr::TextToUpperAscii)
        | Instr::Native(lm_bytecode::NativeInstr::TextLines)
        | Instr::Native(lm_bytecode::NativeInstr::TextToString)
        | Instr::Native(lm_bytecode::NativeInstr::CharCodepoint)
        | Instr::Native(lm_bytecode::NativeInstr::CharUtf8Len)
        | Instr::Native(lm_bytecode::NativeInstr::BytesCompact)
        | Instr::Native(lm_bytecode::NativeInstr::BytesTextView)
        | Instr::Native(lm_bytecode::NativeInstr::TextHash)
        | Instr::Native(lm_bytecode::NativeInstr::BytesHash)
        | Instr::Native(lm_bytecode::NativeInstr::SbByteLen)
        | Instr::Native(lm_bytecode::NativeInstr::SbFinish)
        | Instr::Native(lm_bytecode::NativeInstr::BbFinish)
        | Instr::Freeze
        | Instr::Digest { .. } => (1, 1),
        Instr::EqDigest | Instr::NeDigest => (2, 1),
        Instr::StoreField(_) => (2, 0),
        Instr::ListAt
        | Instr::ListPush
        | Instr::MapHas
        | Instr::MapAt
        | Instr::Native(lm_bytecode::NativeInstr::SbAppendStr)
        | Instr::Native(lm_bytecode::NativeInstr::SbAppendInt)
        | Instr::Native(lm_bytecode::NativeInstr::SbAppendBool)
        | Instr::Native(lm_bytecode::NativeInstr::BbAppend) => (2, 1),
        Instr::MapPut { discard: false, .. }
        | Instr::Native(lm_bytecode::NativeInstr::BytesSlice)
        | Instr::Native(lm_bytecode::NativeInstr::TextSlice)
        | Instr::Native(lm_bytecode::NativeInstr::TextSliceBytes)
        | Instr::Native(lm_bytecode::NativeInstr::TextReplace)
        | Instr::Native(lm_bytecode::NativeInstr::BbFindFrom) => (3, 1),
        Instr::MapPut { discard: true, .. } => (3, 0),
        Instr::ListNew { count, .. } | Instr::TupleNew { count, .. } => (*count as usize, 1),
        Instr::MapNew { count, .. } => (2 * *count as usize, 1),
        Instr::MakeClosure { captures, .. } => (*captures as usize, 1),
        Instr::Call(idx) | Instr::CallG { func: idx, .. } => (tables.function_param_count(*idx), 1),
        Instr::CallVirtual { argc, .. } | Instr::CallVirtualG { argc, .. } => {
            (*argc as usize + 1, 1)
        }
        Instr::CallValue { argc } => (*argc as usize + 1, 1),
        Instr::Jump(_) => (0, 0),
        Instr::JumpIfFalse(_) | Instr::JumpIfTrue(_) => (1, 0),
        Instr::Return => (1, 0),
        Instr::Perform { argc, .. } => (*argc as usize, 1),
        Instr::PerformValue { argc, .. } => (*argc as usize + 1, 1),
        Instr::OpConst(_) => (0, 1),
        Instr::TableEdit { action, .. } => {
            // A mock edit also pops the handler closure.
            if *action == 2 {
                (2, 1)
            } else {
                (1, 1)
            }
        }
        Instr::AsCall { .. } => (1, 1),
        Instr::CallArgs => (1, 1),
        Instr::FaultCode => (1, 1),
        Instr::FaultDenied => (1, 1),
        Instr::RequestOp => (1, 1),
        Instr::RaiseUserPanic | Instr::RaiseAssertionFailed | Instr::RaiseFault => (1, 0),
        Instr::Unreachable => (0, 0),
        Instr::CallInterface { site, .. } => {
            let (interface, method) = lm_bytecode::unpack_interface_call_site(*site);
            let argc = tables.interface_param_count(interface, method);
            (argc + 1, 1)
        }
        Instr::Numeric(instr) => numeric_stack_effect(*instr),
        Instr::Extended(instr) => extended_stack_effect(tables, instr),
    }
}

fn numeric_stack_effect(instr: lm_bytecode::NumericInstr) -> (usize, usize) {
    use lm_bytecode::NumericInstr;
    match instr {
        NumericInstr::IntBitNot
        | NumericInstr::IntToFloat
        | NumericInstr::FloatNeg
        | NumericInstr::FloatIsNan
        | NumericInstr::FloatHash
        | NumericInstr::FloatBits
        | NumericInstr::FloatFromBits
        | NumericInstr::FloatToIntStatus
        | NumericInstr::FloatToIntValue
        | NumericInstr::TextParseFloatStatus
        | NumericInstr::TextParseFloatValue
        | NumericInstr::BytesBitNot => (1, 1),
        NumericInstr::IntBitAnd
        | NumericInstr::IntBitOr
        | NumericInstr::IntBitXor
        | NumericInstr::IntShl
        | NumericInstr::IntShr
        | NumericInstr::IntUshr
        | NumericInstr::IntWrappingAdd
        | NumericInstr::IntWrappingSub
        | NumericInstr::IntWrappingMul
        | NumericInstr::IntRotateLeft
        | NumericInstr::IntRotateRight
        | NumericInstr::FloatAdd
        | NumericInstr::FloatSub
        | NumericInstr::FloatMul
        | NumericInstr::FloatDiv
        | NumericInstr::FloatEq
        | NumericInstr::FloatNe
        | NumericInstr::FloatLt
        | NumericInstr::FloatLe
        | NumericInstr::FloatGt
        | NumericInstr::FloatGe
        | NumericInstr::FloatFixed
        | NumericInstr::SbAppendFloat
        | NumericInstr::BytesBitAnd
        | NumericInstr::BytesBitOr
        | NumericInstr::BytesBitXor => (2, 1),
    }
}

fn extended_stack_effect(tables: &impl StackEffectTables, instr: &ExtendedInstr) -> (usize, usize) {
    match instr {
        ExtendedInstr::OptionNone { .. } => (0, 1),
        ExtendedInstr::OptionSome { .. }
        | ExtendedInstr::OptionPayload { .. }
        | ExtendedInstr::ListEpoch
        | ExtendedInstr::MapEpoch
        | ExtendedInstr::ListCapacity
        | ExtendedInstr::ListPop { .. }
        | ExtendedInstr::ListReorder
        | ExtendedInstr::MapClear
        | ExtendedInstr::SealInstance
        | ExtendedInstr::AsCallback => (1, 1),
        ExtendedInstr::ListGet { .. }
        | ExtendedInstr::MapGet { .. }
        | ExtendedInstr::ListIterLen
        | ExtendedInstr::MapIterLen
        | ExtendedInstr::MapKeyAt
        | ExtendedInstr::MapValueAt
        | ExtendedInstr::ListRemove
        | ExtendedInstr::ListSwapRemove
        | ExtendedInstr::ListReserve
        | ExtendedInstr::ListTruncate
        | ExtendedInstr::ListContains
        | ExtendedInstr::MapRemove { .. }
        | ExtendedInstr::MapReserve => (2, 1),
        ExtendedInstr::MapNextIndex => (3, 1),
        ExtendedInstr::ListSet | ExtendedInstr::ListInsert => (3, 1),
        ExtendedInstr::MakeCallback { captures, .. } => (*captures as usize, 1),
        ExtendedInstr::FunctionCode { .. } | ExtendedInstr::ClassCode { .. } => (0, 1),
        ExtendedInstr::CodeSource { .. }
        | ExtendedInstr::CodeDefinition
        | ExtendedInstr::FaultSite { .. }
        | ExtendedInstr::FaultTrace { .. } => (1, 1),
        ExtendedInstr::CallSlot { slot, .. } | ExtendedInstr::NewSlot { slot, .. } => {
            (tables.slot_param_count(*slot), 1)
        }
        ExtendedInstr::LoadSlot { .. } => (0, 1),
        ExtendedInstr::SendSlot { .. }
        | ExtendedInstr::SyntaxTreeRoot
        | ExtendedInstr::SyntaxKind
        | ExtendedInstr::SyntaxCategory
        | ExtendedInstr::SyntaxRangeStart
        | ExtendedInstr::SyntaxRangeEnd
        | ExtendedInstr::SyntaxText
        | ExtendedInstr::SyntaxChildren
        | ExtendedInstr::SyntaxDetach
        | ExtendedInstr::DynPack { .. }
        | ExtendedInstr::DynRender
        | ExtendedInstr::SyntaxToTree => (1, 1),
        ExtendedInstr::SyntaxBuildToken | ExtendedInstr::SyntaxBuildTrivia => (3, 1),
        ExtendedInstr::SyntaxBuildNode => (3, 1),
        ExtendedInstr::MapProbe => (3, 1),
        ExtendedInstr::MapProbeFound => (1, 1),
        ExtendedInstr::MapProbeKey | ExtendedInstr::MapProbeValue => (2, 1),
        ExtendedInstr::MapProbeSetValue => (3, 1),
        ExtendedInstr::MapProbeRemove => (2, 1),
        ExtendedInstr::MapInsertHashed => (5, 1),
        ExtendedInstr::MapWriteGuard => (1, 1),
        ExtendedInstr::PrepareWait { op_argc, .. } => {
            let (_, argc) = ExtendedInstr::wait_parts(*op_argc);
            (argc as usize, 1)
        }
    }
}

/// The display name of one operation slot, safe for out-of-range
/// slots in hand-built modules.
fn op_text(slot: u32) -> String {
    if slot < lm_abi::OP_COUNT {
        lm_abi::op_name(slot)
    } else {
        format!("op{slot}")
    }
}

fn instr_text(instr: &Instr) -> String {
    match instr {
        Instr::ConstUnit => "ConstUnit".to_string(),
        Instr::ConstBool(v) => format!("ConstBool {v}"),
        Instr::ConstInt(v) => format!("ConstInt {v}"),
        Instr::ConstFloat(bits) => format!("ConstFloat {bits:#018x}"),
        Instr::ConstChar(value) => format!("ConstChar U+{value:04X}"),
        Instr::ConstStr(idx) => format!("ConstStr s{idx}"),
        Instr::ConstBytes(idx) => format!("ConstBytes b{idx}"),
        Instr::Numeric(instr) => format!("Numeric {instr:?}"),
        Instr::LoadLocal(slot) => format!("LoadLocal {slot}"),
        Instr::StoreLocal(slot) => format!("StoreLocal {slot}"),
        Instr::Pop => "Pop".to_string(),
        Instr::Add => "Add".to_string(),
        Instr::Sub => "Sub".to_string(),
        Instr::Mul => "Mul".to_string(),
        Instr::Div => "Div".to_string(),
        Instr::Rem => "Rem".to_string(),
        Instr::Neg => "Neg".to_string(),
        Instr::Not => "Not".to_string(),
        Instr::LtInt => "LtInt".to_string(),
        Instr::LeInt => "LeInt".to_string(),
        Instr::GtInt => "GtInt".to_string(),
        Instr::GeInt => "GeInt".to_string(),
        Instr::EqInt => "EqInt".to_string(),
        Instr::NeInt => "NeInt".to_string(),
        Instr::EqBool => "EqBool".to_string(),
        Instr::NeBool => "NeBool".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::EqStr) => "EqStr".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::NeStr) => "NeStr".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::StrByteLen) => "StrByteLen".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::StrCharCount) => "StrCharCount".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::StrConcat) => "StrConcat".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::StrStartsWith) => "StrStartsWith".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::StrEndsWith) => "StrEndsWith".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::StrContains) => "StrContains".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::StrFindIndex) => "StrFindIndex".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::TextFindByteIndex) => {
            "TextFindByteIndex".to_string()
        }
        Instr::Native(lm_bytecode::NativeInstr::TextAtByte) => "TextAtByte".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::TextTrim) => "TextTrim".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::TextTrimStart) => "TextTrimStart".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::TextTrimEnd) => "TextTrimEnd".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::TextToLowerAscii) => "TextToLowerAscii".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::TextToUpperAscii) => "TextToUpperAscii".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::TextReplace) => "TextReplace".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::TextParseIntStatus) => {
            "TextParseIntStatus".to_string()
        }
        Instr::Native(lm_bytecode::NativeInstr::TextParseIntValue) => {
            "TextParseIntValue".to_string()
        }
        Instr::Native(lm_bytecode::NativeInstr::TextPadStart) => "TextPadStart".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::TextPadEnd) => "TextPadEnd".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::BytesEndsWith) => "BytesEndsWith".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::BytesContains) => "BytesContains".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::TextSplit) => "TextSplit".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::TextLines) => "TextLines".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::TextAt) => "TextAt".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::TextSlice) => "TextSlice".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::TextIsBoundary) => "TextIsBoundary".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::TextSliceBytes) => "TextSliceBytes".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::TextBytes) => "TextBytes".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::TextLt) => "TextLt".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::TextLe) => "TextLe".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::TextGt) => "TextGt".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::TextGe) => "TextGe".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::TextToString) => "TextToString".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::CharCodepoint) => "CharCodepoint".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::CharUtf8Len) => "CharUtf8Len".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::EqChar) => "EqChar".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::NeChar) => "NeChar".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::LtChar) => "LtChar".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::LeChar) => "LeChar".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::GtChar) => "GtChar".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::GeChar) => "GeChar".to_string(),
        Instr::EqRef => "EqRef".to_string(),
        Instr::EqValue => "EqValue".to_string(),
        Instr::NeValue => "NeValue".to_string(),
        Instr::NeRef => "NeRef".to_string(),
        Instr::Call(idx) => format!("Call fn{idx}"),
        Instr::CallG { func, app } => format!("CallG fn{func} app{app}"),
        Instr::CallVirtual { selector, argc } => {
            format!("CallVirtual sel{selector} argc {argc}")
        }
        Instr::CallVirtualG {
            selector,
            argc,
            app,
        } => format!("CallVirtualG sel{selector} argc {argc} app{app}"),
        Instr::CallValue { argc } => format!("CallValue argc {argc}"),
        Instr::MakeClosure { func, captures } => {
            format!("MakeClosure fn{func} captures {captures}")
        }
        Instr::LoadCapture(idx) => format!("LoadCapture {idx}"),
        Instr::New(class) => format!("New class{class}"),
        Instr::NewG { class, app } => format!("NewG class{class} app{app}"),
        Instr::LoadField(field) => format!("LoadField {field}"),
        Instr::StoreField(field) => format!("StoreField {field}"),
        Instr::TupleNew { ty, count } => format!("TupleNew ty{ty} count {count}"),
        Instr::TupleGet(index) => format!("TupleGet {index}"),
        Instr::IsType(ty) => format!("IsType ty{ty}"),
        Instr::CastType(ty) => format!("CastType ty{ty}"),
        Instr::ListNew { ty, count } => format!("ListNew ty{ty} count {count}"),
        Instr::ListLen => "ListLen".to_string(),
        Instr::ListAt => "ListAt".to_string(),
        Instr::ListPush => "ListPush".to_string(),
        Instr::MapNew { ty, count } => format!("MapNew ty{ty} count {count}"),
        Instr::MapLen => "MapLen".to_string(),
        Instr::MapHas => "MapHas".to_string(),
        Instr::MapAt => "MapAt".to_string(),
        Instr::MapPut { ty, discard } => format!("MapPut ty{ty} discard {discard}"),
        Instr::Native(lm_bytecode::NativeInstr::SbNew) => "SbNew".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::SbAppendStr) => "SbAppendStr".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::SbAppendInt) => "SbAppendInt".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::SbAppendBool) => "SbAppendBool".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::SbBuild) => "SbBuild".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::SbLen) => "SbLen".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::SbClear) => "SbClear".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::BbNew) => "BbNew".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::BbAppend) => "BbAppend".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::BbLen) => "BbLen".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::BbBuild) => "BbBuild".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::BbExtend) => "BbExtend".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::BbReserve) => "BbReserve".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::BbClear) => "BbClear".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::BytesNew) => "BytesNew".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::BytesLen) => "BytesLen".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::BytesText) => "BytesText".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::BytesAt) => "BytesAt".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::BytesGet) => "BytesGet".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::BytesSlice) => "BytesSlice".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::BytesConcat) => "BytesConcat".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::BytesStartsWith) => "BytesStartsWith".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::BytesFindIndex) => "BytesFindIndex".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::BytesHex) => "BytesHex".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::BytesIsUtf8) => "BytesIsUtf8".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::EqBytes) => "EqBytes".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::NeBytes) => "NeBytes".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::LtBytes) => "LtBytes".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::LeBytes) => "LeBytes".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::GtBytes) => "GtBytes".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::GeBytes) => "GeBytes".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::BytesCompact) => "BytesCompact".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::BytesTextView) => "BytesTextView".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::TextHash) => "TextHash".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::BytesHash) => "BytesHash".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::HashCombine) => "HashCombine".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::HashUnorderedCombine) => {
            "HashUnorderedCombine".to_string()
        }
        Instr::Native(lm_bytecode::NativeInstr::SbAppendChar) => "SbAppendChar".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::SbByteLen) => "SbByteLen".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::SbFinish) => "SbFinish".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::BbFinish) => "BbFinish".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::BbAt) => "BbAt".to_string(),
        Instr::Native(lm_bytecode::NativeInstr::BbFindFrom) => "BbFindFrom".to_string(),
        Instr::Freeze => "Freeze".to_string(),
        Instr::Digest { ty } => format!("Digest {ty}"),
        Instr::EqDigest => "EqDigest".to_string(),
        Instr::NeDigest => "NeDigest".to_string(),
        Instr::Jump(b) => format!("Jump -> b{b}"),
        Instr::JumpIfFalse(b) => format!("JumpIfFalse -> b{b}"),
        Instr::JumpIfTrue(b) => format!("JumpIfTrue -> b{b}"),
        Instr::Return => "Return".to_string(),
        Instr::Perform { op, argc, .. } => {
            format!("Perform {} argc {argc}", op_text(*op))
        }
        Instr::PerformValue { argc, .. } => format!("PerformValue argc {argc}"),
        Instr::OpConst(op) => format!("OpConst {}", op_text(*op)),
        Instr::TableEdit { action, kind, slot } => {
            let action_text = match action {
                0 => "pass",
                1 => "block",
                2 => "mock",
                _ => "clear",
            };
            let target = match kind {
                0 => op_text(*slot),
                _ => lm_abi::GROUPS
                    .get(*slot as usize)
                    .map(|g| g.to_string())
                    .unwrap_or_else(|| format!("group{slot}")),
            };
            format!("TableEdit {action_text} {target}")
        }
        Instr::AsCall { op, ty } => format!("AsCall {} type{ty}", op_text(*op)),
        Instr::CallArgs => "CallArgs".to_string(),
        Instr::FaultCode => "FaultCode".to_string(),
        Instr::FaultDenied => "FaultDenied".to_string(),
        Instr::RaiseUserPanic => "RaiseUserPanic".to_string(),
        Instr::RaiseAssertionFailed => "RaiseAssertionFailed".to_string(),
        Instr::RaiseFault => "RaiseFault".to_string(),
        Instr::RequestOp => "RequestOp".to_string(),
        Instr::Unreachable => "Unreachable".to_string(),
        Instr::CallInterface { site, recv_ty, app } => {
            let (interface, method) = lm_bytecode::unpack_interface_call_site(*site);
            format!("CallInterface interface{interface} method{method} type{recv_ty} app{app}")
        }
        Instr::Extended(instr) => extended_instr_text(instr),
    }
}

fn extended_instr_text(instr: &ExtendedInstr) -> String {
    match instr {
        ExtendedInstr::MakeCallback { func, captures } => {
            format!("MakeCallback fn{func} captures {captures}")
        }
        ExtendedInstr::FunctionCode { func } => format!("FunctionCode fn{func}"),
        ExtendedInstr::ClassCode { class } => format!("ClassCode class{class}"),
        ExtendedInstr::CodeSource { ty } => format!("CodeSource ty{ty}"),
        ExtendedInstr::CodeDefinition => "CodeDefinition".to_string(),
        ExtendedInstr::FaultSite { ty } => format!("FaultSite ty{ty}"),
        ExtendedInstr::FaultTrace { ty } => format!("FaultTrace ty{ty}"),
        ExtendedInstr::AsCallback => "AsCallback".to_string(),
        ExtendedInstr::OptionSome { ty } => format!("OptionSome ty{ty}"),
        ExtendedInstr::OptionNone { ty } => format!("OptionNone ty{ty}"),
        ExtendedInstr::OptionPayload { ty } => format!("OptionPayload ty{ty}"),
        ExtendedInstr::ListGet { ty } => format!("ListGet ty{ty}"),
        ExtendedInstr::MapGet { ty } => format!("MapGet ty{ty}"),
        ExtendedInstr::ListEpoch => "ListEpoch".to_string(),
        ExtendedInstr::ListIterLen => "ListIterLen".to_string(),
        ExtendedInstr::MapEpoch => "MapEpoch".to_string(),
        ExtendedInstr::MapIterLen => "MapIterLen".to_string(),
        ExtendedInstr::MapNextIndex => "MapNextIndex".to_string(),
        ExtendedInstr::SealInstance => "SealInstance".to_string(),
        ExtendedInstr::MapKeyAt => "MapKeyAt".to_string(),
        ExtendedInstr::MapValueAt => "MapValueAt".to_string(),
        ExtendedInstr::ListCapacity => "ListCapacity".to_string(),
        ExtendedInstr::ListSet => "ListSet".to_string(),
        ExtendedInstr::ListPop { ty } => format!("ListPop ty{ty}"),
        ExtendedInstr::ListInsert => "ListInsert".to_string(),
        ExtendedInstr::ListRemove => "ListRemove".to_string(),
        ExtendedInstr::ListSwapRemove => "ListSwapRemove".to_string(),
        ExtendedInstr::ListReserve => "ListReserve".to_string(),
        ExtendedInstr::ListTruncate => "ListTruncate".to_string(),
        ExtendedInstr::ListContains => "ListContains".to_string(),
        ExtendedInstr::ListReorder => "ListReorder".to_string(),
        ExtendedInstr::MapRemove { ty } => format!("MapRemove ty{ty}"),
        ExtendedInstr::MapClear => "MapClear".to_string(),
        ExtendedInstr::MapReserve => "MapReserve".to_string(),
        ExtendedInstr::CallSlot { slot, app } => {
            format!("CallSlot slot{slot} {}", optional_app_text(*app))
        }
        ExtendedInstr::NewSlot { slot, app } => {
            format!("NewSlot slot{slot} {}", optional_app_text(*app))
        }
        ExtendedInstr::LoadSlot { slot } => format!("LoadSlot slot{slot}"),
        ExtendedInstr::SendSlot { slot } => format!("SendSlot slot{slot}"),
        ExtendedInstr::SyntaxTreeRoot => "SyntaxTreeRoot".to_string(),
        ExtendedInstr::SyntaxKind => "SyntaxKind".to_string(),
        ExtendedInstr::SyntaxCategory => "SyntaxCategory".to_string(),
        ExtendedInstr::SyntaxRangeStart => "SyntaxRangeStart".to_string(),
        ExtendedInstr::SyntaxRangeEnd => "SyntaxRangeEnd".to_string(),
        ExtendedInstr::SyntaxText => "SyntaxText".to_string(),
        ExtendedInstr::SyntaxChildren => "SyntaxChildren".to_string(),
        ExtendedInstr::SyntaxDetach => "SyntaxDetach".to_string(),
        ExtendedInstr::DynPack { ty } => format!("DynPack ty{ty}"),
        ExtendedInstr::DynRender => "DynRender".to_string(),
        ExtendedInstr::SyntaxBuildToken => "SyntaxBuildToken".to_string(),
        ExtendedInstr::SyntaxBuildTrivia => "SyntaxBuildTrivia".to_string(),
        ExtendedInstr::SyntaxBuildNode => "SyntaxBuildNode".to_string(),
        ExtendedInstr::SyntaxToTree => "SyntaxToTree".to_string(),
        ExtendedInstr::MapProbe => "MapProbe".to_string(),
        ExtendedInstr::MapProbeFound => "MapProbeFound".to_string(),
        ExtendedInstr::MapProbeKey => "MapProbeKey".to_string(),
        ExtendedInstr::MapProbeValue => "MapProbeValue".to_string(),
        ExtendedInstr::MapProbeSetValue => "MapProbeSetValue".to_string(),
        ExtendedInstr::MapProbeRemove => "MapProbeRemove".to_string(),
        ExtendedInstr::MapInsertHashed => "MapInsertHashed".to_string(),
        ExtendedInstr::MapWriteGuard => "MapWriteGuard".to_string(),
        ExtendedInstr::PrepareWait { op_argc, .. } => {
            let (op, argc) = ExtendedInstr::wait_parts(*op_argc);
            format!("PrepareWait {} argc {argc}", op_text(op))
        }
    }
}

fn optional_app_text(app: u32) -> String {
    if app == lm_bytecode::NO_APP {
        "plain".to_string()
    } else {
        format!("app{app}")
    }
}

fn row_text(_module: &Module, row: &[BcRow]) -> String {
    let bundle = lm_abi::standard_bundle();
    let parts: Vec<String> = row
        .iter()
        .map(|elem| match elem {
            BcRow::Op(idx) => bundle
                .op_name(*idx)
                .map(str::to_string)
                .unwrap_or_else(|| format!("op{idx}")),
            BcRow::Group(idx) => bundle
                .group_name(*idx)
                .map(str::to_string)
                .unwrap_or_else(|| format!("group{idx}")),
            BcRow::Var(v) => format!("e{v}"),
        })
        .collect();
    parts.join(", ")
}

fn type_text(module: &Module, idx: u32) -> String {
    match &module.types[idx as usize] {
        BcType::Unit => "()".to_string(),
        BcType::Never => "Never".to_string(),
        BcType::Bool => "Bool".to_string(),
        BcType::Int => "Int".to_string(),
        BcType::Float => "Float".to_string(),
        BcType::Str => "String".to_string(),
        BcType::Bytes => "Bytes".to_string(),
        BcType::FileHandle => "FileHandle".to_string(),
        BcType::ResourceHandle => "ResourceHandle".to_string(),
        BcType::HostResource => "HostResource".to_string(),
        BcType::Digest => "Digest".to_string(),
        BcType::Class(c) => module
            .classes
            .get(*c as usize)
            .map(|cl| cl.name.clone())
            .unwrap_or_else(|| format!("class{c}")),
        BcType::Inst(c, args) => {
            let name = module
                .classes
                .get(*c as usize)
                .map(|cl| cl.name.clone())
                .unwrap_or_else(|| format!("class{c}"));
            let parts: Vec<String> = args.iter().map(|a| type_text(module, *a)).collect();
            format!("{}[{}]", name, parts.join(", "))
        }
        BcType::List(e) => format!("[{}]", type_text(module, *e)),
        BcType::Map(k, v) => {
            format!("{{{}: {}}}", type_text(module, *k), type_text(module, *v))
        }
        BcType::Tuple(elems) => {
            let parts: Vec<String> = elems.iter().map(|e| type_text(module, *e)).collect();
            if parts.len() == 1 {
                format!("({},)", parts[0])
            } else {
                format!("({})", parts.join(", "))
            }
        }
        BcType::Fn(params, muts, ret, row) => {
            let parts: Vec<String> = params
                .iter()
                .zip(muts.iter())
                .map(|(p, m)| {
                    if *m {
                        format!("mut {}", type_text(module, *p))
                    } else {
                        type_text(module, *p)
                    }
                })
                .collect();
            let mut out = format!("({}) -> {}", parts.join(", "), type_text(module, *ret));
            if !row.is_empty() {
                out.push_str(" with ");
                out.push_str(&row_text(module, row));
            }
            out
        }
        BcType::Callback(params, muts, ret, row) => {
            let parts: Vec<String> = params
                .iter()
                .zip(muts.iter())
                .map(|(p, m)| {
                    if *m {
                        format!("mut {}", type_text(module, *p))
                    } else {
                        type_text(module, *p)
                    }
                })
                .collect();
            let mut out = format!(
                "nonescaping ({}) -> {}",
                parts.join(", "),
                type_text(module, *ret)
            );
            if !row.is_empty() {
                out.push_str(" with ");
                out.push_str(&row_text(module, row));
            }
            out
        }
        BcType::Var(i) => format!("${i}"),
        BcType::Projection {
            base,
            interface,
            assoc,
        } => format!(
            "{}.interface{interface}.assoc{assoc}",
            type_text(module, *base)
        ),
        BcType::Fault => "Fault".to_string(),
        BcType::Request => "Request".to_string(),
        BcType::PolicyTable => "PolicyTable".to_string(),
        BcType::Vm => "Vm".to_string(),
        BcType::VmSnapshot => "VmSnapshot".to_string(),
        BcType::Run(t) => format!("Run[{}]", type_text(module, *t)),
        BcType::Wait(t) => format!("Wait[{}]", type_text(module, *t)),
        BcType::RunSnapshot(t) => format!("RunSnapshot[{}]", type_text(module, *t)),
        BcType::PendingCall(a, r) => format!(
            "PendingCall[{}, {}]",
            type_text(module, *a),
            type_text(module, *r)
        ),
        BcType::Handle(m, r) => format!(
            "Handle[{}, {}]",
            type_text(module, *m),
            type_text(module, *r)
        ),
        BcType::Op(op, f) => format!("Op[{}, {}]", op_text(*op), type_text(module, *f)),
    }
}

/// Render a module as a readable control-flow listing with tables,
/// function signatures, block boundaries, stack effects, and resolved
/// jump targets.
pub fn dump_cfg(module: &Module) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "entry fn{}", module.entry);
    for (sidx, s) in module.strings.iter().enumerate() {
        let _ = writeln!(out, "string s{sidx} = {s:?}");
    }
    for (tidx, _) in module.types.iter().enumerate() {
        let _ = writeln!(out, "type ty{tidx} = {}", type_text(module, tidx as u32));
    }
    for (sidx, s) in module.selectors.iter().enumerate() {
        let _ = writeln!(out, "selector sel{sidx} = {s}");
    }
    for (aidx, app) in module.apps.iter().enumerate() {
        let types: Vec<String> = app.types.iter().map(|t| type_text(module, *t)).collect();
        let rows: Vec<String> = app
            .rows
            .iter()
            .map(|r| format!("{{{}}}", row_text(module, r)))
            .collect();
        let _ = writeln!(
            out,
            "app app{aidx} = [{}] rows [{}]",
            types.join(", "),
            rows.join(", ")
        );
    }
    for (cidx, class) in module.classes.iter().enumerate() {
        // A generic parent carries its type arguments, so the listing
        // shows the instantiation the class table records.
        let parent = class
            .parent()
            .map(|p| {
                let args = if class.parent_args.is_empty() {
                    String::new()
                } else {
                    let parts: Vec<String> = class
                        .parent_args
                        .iter()
                        .map(|t| type_text(module, *t))
                        .collect();
                    format!("[{}]", parts.join(", "))
                };
                format!(" < {}{args}", module.classes[p as usize].name)
            })
            .unwrap_or_default();
        let kind = match class.kind {
            BcClassKind::Normal => "",
            BcClassKind::Abstract => " abstract",
            BcClassKind::Case => " case",
        };
        let final_mark = if class.is_final { " final" } else { "" };
        let frozen_mark = if class.is_frozen { " frozen" } else { "" };
        let generics = if class.type_params > 0 {
            format!(" params {}", class.type_params)
        } else {
            String::new()
        };
        let _ = writeln!(
            out,
            "class class{cidx} {}{final_mark}{frozen_mark}{kind}{generics}{parent}",
            class.name
        );
        for (fidx, (name, ty)) in class.fields.iter().enumerate() {
            let _ = writeln!(out, "  field {fidx} {name}: {}", type_text(module, *ty));
        }
        for (sel, func) in &class.methods {
            let _ = writeln!(out, "  method sel{sel} -> fn{func}");
        }
    }
    // One function-to-names index for the whole dump.
    let mut binding_index: std::collections::HashMap<u32, Vec<&str>> =
        std::collections::HashMap::new();
    for binding in &module.bindings {
        binding_index
            .entry(binding.func)
            .or_default()
            .push(binding.key.as_str());
    }
    for (fidx, func) in module.funcs.iter().enumerate() {
        let params: Vec<String> = func.params.iter().map(|p| type_text(module, *p)).collect();
        let generics = if func.type_params > 0 || func.effect_params > 0 {
            format!(" generics {}+{}", func.type_params, func.effect_params)
        } else {
            String::new()
        };
        let row = if func.row.is_empty() {
            String::new()
        } else {
            format!(" with {}", row_text(module, &func.row))
        };
        let _ = writeln!(
            out,
            "\nfn{} {}({}) -> {}{}{}",
            fidx,
            func.name,
            params.join(", "),
            type_text(module, func.ret),
            row,
            generics
        );
        // Every name that points at this function value. Two modules
        // with equal bodies share one code object and keep two names,
        // so the listing must print them all. The index is built once,
        // because a scan per function makes the dump quadratic.
        if let Some(keys) = binding_index.get(&(fidx as u32)) {
            for key in keys {
                let _ = writeln!(out, "  binding {key}");
            }
        }
        if !func.captures.is_empty() {
            let caps: Vec<String> = func
                .captures
                .iter()
                .map(|c| type_text(module, *c))
                .collect();
            let _ = writeln!(out, "  captures {}", caps.join(", "));
        }
        let _ = writeln!(out, "  locals {}", func.local_count());
        for (bidx, block) in func.blocks.iter().enumerate() {
            let _ = writeln!(out, "  b{bidx}:");
            for instr in block {
                let (pops, pushes) = stack_effect(module, instr);
                let _ = writeln!(
                    out,
                    "    {:<24} ; pop {pops} push {pushes}",
                    instr_text(instr)
                );
            }
        }
    }
    out
}
