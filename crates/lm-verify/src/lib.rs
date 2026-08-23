//! Independent bytecode verifier.
//!
//! The verifier receives a decoded module and rejects it unless every
//! table and every function is well formed. It validates the type,
//! selector, type-application, and class tables first. It then
//! reconstructs the operand-stack types and the local-slot types at
//! each block entry with a worklist, and it checks jumps, calls,
//! generic substitution, claimed effect rows, field access, closure
//! creation, tuples, casts, and collection operations. A generic
//! function body is verified once with its type variables opaque;
//! call sites substitute the callee signature through the type
//! application. The verifier shares no code with the source checker.

use lm_bytecode::corepin::CoreLayout;
use lm_bytecode::{
    BcCallableContract, BcClassKind, BcInterfaceUse, BcRow, BcType, ExtendedInstr, Func, Instr,
    Module, SlotContract, SlotTarget,
};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;

/// The largest operand-stack depth the verifier accepts for one function.
const MAX_STATIC_STACK: usize = 4096;

/// The largest portable tuple arity.
const MAX_TUPLE_ARITY: usize = 16;

/// The largest local slot count of one function. The bound rejects a
/// forged `local_count` before any allocation is sized from it.
const MAX_LOCAL_SLOTS: u32 = 65_536;

/// The deepest a type may nest.
///
/// A type child names an earlier table entry, so a table of N entries
/// can nest N deep. Every walk over a type costs at least its depth. A
/// crafted artifact must not make that work unbounded.
///
/// The bound makes a deep type unrepresentable. It also keeps a
/// recursive walk safe, so a later walk needs no iterative form to stay
/// inside the Rust stack.
///
/// Real code nests far below this limit. `lm-bytecode` bounds an
/// interface type at 32 for the same reason.
const MAX_TYPE_DEPTH: u32 = 128;

/// The largest dataflow footprint of one function: block count times
/// local slots. The bound keeps hostile inputs from demanding an
/// unbounded state table.
const MAX_DATAFLOW_CELLS: u64 = 1 << 24;

/// Canonical type-table indices for the primitive types. Every module
/// must begin its type table with these entries in this order.
pub const TY_UNIT: u32 = 0;
pub const TY_BOOL: u32 = 1;
pub const TY_INT: u32 = 2;
pub const TY_STR: u32 = 3;

/// A verification failure. The message names the exact position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyError {
    pub func: Option<u32>,
    pub message: String,
}

impl fmt::Display for VerifyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.func {
            Some(func) => write!(f, "function {func}: {}", self.message),
            None => f.write_str(&self.message),
        }
    }
}

fn err(func: u32, message: impl Into<String>) -> VerifyError {
    VerifyError {
        func: Some(func),
        message: message.into(),
    }
}

/// One join step: a finished type, or the element lists of two tuples
/// whose elements the walk still joins.
enum Flat {
    Type(u32),
    Tuple(Vec<u32>, Vec<u32>),
}

/// The abstract state at one program point. Types are indices into
/// the extended type universe. `None` marks a local slot without a
/// known value.
#[derive(Debug, Clone, PartialEq, Eq)]
struct State {
    locals: Vec<Option<u32>>,
    stack: Vec<u32>,
}

/// The extended type universe: the module table plus the types
/// created by substitution during verification.
struct Universe {
    types: Vec<BcType>,
    index: HashMap<BcType, u32>,
    facts: Vec<TypeFacts>,
}

#[derive(Clone, Copy, Default)]
struct TypeFacts {
    stores_callback: bool,
    contains_projection: bool,
    max_type_var: Option<u32>,
    max_effect_var: Option<u32>,
}

impl TypeFacts {
    fn include(&mut self, other: Self) {
        self.stores_callback |= other.stores_callback;
        self.contains_projection |= other.contains_projection;
        self.max_type_var = self.max_type_var.max(other.max_type_var);
        self.max_effect_var = self.max_effect_var.max(other.max_effect_var);
    }
}

impl Universe {
    fn intern(&mut self, ty: BcType) -> u32 {
        if let Some(idx) = self.index.get(&ty) {
            return *idx;
        }
        let idx = self.types.len() as u32;
        let facts = type_facts(&ty, &self.facts);
        self.types.push(ty.clone());
        self.facts.push(facts);
        self.index.insert(ty, idx);
        idx
    }
}

fn type_facts(ty: &BcType, known: &[TypeFacts]) -> TypeFacts {
    let mut facts = TypeFacts::default();
    match ty {
        BcType::Callback(params, _, ret, row) => {
            facts.stores_callback = true;
            for child in params {
                include_type_facts(&mut facts, known, *child);
            }
            include_type_facts(&mut facts, known, *ret);
            for element in row {
                if let BcRow::Var(var) = element {
                    facts.max_effect_var = facts.max_effect_var.max(Some(*var));
                }
            }
        }
        BcType::Fn(params, _, ret, row) => {
            for child in params {
                include_parameter_type_facts(&mut facts, known, *child);
            }
            include_type_facts(&mut facts, known, *ret);
            for element in row {
                if let BcRow::Var(var) = element {
                    facts.max_effect_var = facts.max_effect_var.max(Some(*var));
                }
            }
        }
        BcType::Inst(_, args) | BcType::Tuple(args) => {
            for child in args {
                include_type_facts(&mut facts, known, *child);
            }
        }
        BcType::List(child)
        | BcType::Run(child)
        | BcType::Wait(child)
        | BcType::RunSnapshot(child) => include_type_facts(&mut facts, known, *child),
        BcType::Projection { base, .. } => {
            facts.contains_projection = true;
            include_type_facts(&mut facts, known, *base);
        }
        BcType::Map(left, right)
        | BcType::PendingCall(left, right)
        | BcType::Handle(left, right) => {
            include_type_facts(&mut facts, known, *left);
            include_type_facts(&mut facts, known, *right);
        }
        BcType::Op(_, function) => include_type_facts(&mut facts, known, *function),
        BcType::Var(var) => facts.max_type_var = Some(*var),
        _ => {}
    }
    facts
}

fn include_type_facts(facts: &mut TypeFacts, known: &[TypeFacts], child: u32) {
    facts.include(known.get(child as usize).copied().unwrap_or_default());
}

fn include_parameter_type_facts(facts: &mut TypeFacts, known: &[TypeFacts], child: u32) {
    let Some(child) = known.get(child as usize).copied() else {
        return;
    };
    facts.contains_projection |= child.contains_projection;
    facts.max_type_var = facts.max_type_var.max(child.max_type_var);
    facts.max_effect_var = facts.max_effect_var.max(child.max_effect_var);
}

/// Shared lookup context for one module.
struct Ctx<'m> {
    module: &'m Module,
    bundle: std::sync::Arc<lm_abi::AbiBundle>,
    /// Class index to the type index of its `Class` entry, when the
    /// module contains one.
    class_ty: Vec<Option<u32>>,
    /// The first conformance for each class and interface pair.
    conformance_index: HashMap<(u32, u32), usize>,
    /// The class built by each function, when it is a constructor.
    constructor_classes: Vec<Option<u32>>,
    /// Constructor functions grouped by their class.
    class_constructors: Vec<Vec<u32>>,
    uni: RefCell<Universe>,
    /// The resolved pinned core definitions of this module.
    core: CoreLayout,
}

mod ctx;
mod func;
mod roles;
mod step;
mod tables;

use func::verify_func;
use roles::*;
use step::step;
use tables::verify_tables;

/// The verifier version. It takes part in the verified-code cache
/// key: a rule change invalidates every cached admission.
///
/// Version 9 adds byte types, resource types, and their operations.
/// Version 10 adds final class rules. Version 11 adds the `Int` role.
/// Version 12 adds the `Bool` role. Version 13 adds the `String` role
/// and String instructions. Version 14 adds Bytes and builder roles.
/// Version 15 adds the sealed Text family and immediate Char rules.
/// Version 16 adds the text extraction rules and structural enum
/// equality. Version 16 also named native TLS resources and their
/// service control on a separate branch, so version 17 is the first
/// that accepts both.
/// Version 19 verifies interfaces, callbacks, native `Option`, and collection operations.
/// Version 20 verifies the declared receiver type of each digest.
/// Version 21 separates persistent VMs from typed runs.
/// Version 23 verifies late-bound slot contracts and instructions.
/// Version 24 verifies reified code controls.
/// Version 25 verifies runtime compiler input classes. Version 26
/// verifies `ClassDef` and complete VM image controls. Version 27
/// verifies fallible activation and stable slot discovery.
/// Version 28 verifies versioned constructors in class slots.
/// Version 29 verifies portable definition source lookup.
/// Version 30 verifies fault source lookup instructions.
/// Version 31 verifies native and interface-backed map paths.
/// Version 32 verifies conditional conformance premises.
/// Version 33 verifies ordered and unordered hash mixing.
/// Version 34 verifies Float and bitwise instructions.
/// Version 35 verifies text padding and Float text conversions.
pub const VERIFIER_VERSION: u32 = 35;

/// Verify a full module. Every table and every function must pass.
///
/// The core layout comes from the core role table the artifact
/// carries. The verifier proves the shape of every filled slot, so it
/// reads no definition hash and no source name.
pub fn verify_module(module: &Module) -> Result<(), VerifyError> {
    let bundle = lm_abi::standard_bundle();
    verify_module_with_bundle(module, &bundle)
}

/// Verify a full module against one immutable ABI bundle.
pub fn verify_module_with_bundle(
    module: &Module,
    bundle: &std::sync::Arc<lm_abi::AbiBundle>,
) -> Result<(), VerifyError> {
    let ctx = verify_structure(module, bundle.clone())?;
    let imported = module.extern_funcs();
    for (idx, func) in module.funcs.iter().enumerate() {
        // An imported function has no body to check. The structural
        // pass already proved it carries a signature only.
        if imported[idx] {
            continue;
        }
        verify_func(&ctx, func, idx as u32)?;
    }
    Ok(())
}

/// Validate every module-level rule without the per-function
/// dataflow: the tables and the entry shape. The verified-code cache
/// may skip only the dataflow, never this pass, so a hash-equal
/// byte stream with a non-canonical table is rejected on every load.
pub fn verify_structure_only(module: &Module) -> Result<(), VerifyError> {
    let bundle = lm_abi::standard_bundle();
    verify_structure_only_with_bundle(module, &bundle)
}

/// Validate module-level rules against one immutable ABI bundle.
pub fn verify_structure_only_with_bundle(
    module: &Module,
    bundle: &std::sync::Arc<lm_abi::AbiBundle>,
) -> Result<(), VerifyError> {
    verify_structure(module, bundle.clone()).map(|_| ())
}

fn verify_structure(
    module: &Module,
    bundle: std::sync::Arc<lm_abi::AbiBundle>,
) -> Result<Ctx<'_>, VerifyError> {
    let core = lm_bytecode::corepin::declared_layout(module);
    let ctx = verify_tables(module, core, bundle)?;
    let entry = module.entry as usize;
    if entry >= module.funcs.len() {
        return Err(err(
            module.entry,
            format!(
                "entry index {} is not inside the function table of length {}",
                module.entry,
                module.funcs.len()
            ),
        ));
    }
    let entry_func = &module.funcs[entry];
    if !entry_func.params.is_empty() {
        return Err(err(
            module.entry,
            "the entry function must not have parameters",
        ));
    }
    if !entry_func.captures.is_empty() {
        return Err(err(
            module.entry,
            "the entry function must not have captures",
        ));
    }
    if entry_func.type_params != 0 || entry_func.effect_params != 0 {
        return Err(err(module.entry, "the entry function must not be generic"));
    }
    Ok(ctx)
}

// ----------------------------------------------------------------
// The core role slots.
// ----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use lm_bytecode::{BcClass, Func, Instr::*, Module, NumericInstr, TypeApp, NO_PARENT};

    /// Add empty interface bounds required by one test fixture.
    fn complete_bounds(module: &mut Module) {
        module.class_bounds = module
            .classes
            .iter()
            .map(|class| vec![Vec::new(); class.type_params as usize])
            .collect();
        module.func_bounds = module
            .funcs
            .iter()
            .map(|func| vec![Vec::new(); func.type_params as usize])
            .collect();
    }

    fn verify_module(module: &Module) -> Result<(), VerifyError> {
        let mut module = module.clone();
        complete_bounds(&mut module);
        super::verify_module(&module)
    }

    fn base_types() -> Vec<BcType> {
        vec![BcType::Unit, BcType::Bool, BcType::Int, BcType::Str]
    }

    fn plain_func(name: &str, params: Vec<u32>, ret: u32, blocks: Vec<Vec<Instr>>) -> Func {
        Func {
            name: name.to_string(),
            type_params: 0,
            effect_params: 0,
            param_muts: vec![false; params.len()],
            local_types: {
                let mut locals = params.clone();
                locals.resize(2, TY_INT);
                locals
            },
            params,
            ret,
            row: vec![],
            captures: vec![],
            blocks,
        }
    }

    fn module_with(blocks: Vec<Vec<Instr>>) -> Module {
        Module {
            strings: vec!["s".to_string()],
            bytes: vec![],
            types: base_types(),
            selectors: vec![],
            apps: vec![],
            interfaces: vec![],
            conformances: vec![],
            class_bounds: vec![],
            func_bounds: vec![vec![]],
            classes: vec![],
            funcs: vec![plain_func("main", vec![], TY_INT, blocks)],
            imports: vec![],
            slots: vec![],
            core_roles: [lm_bytecode::NO_ROLE; lm_bytecode::CORE_ROLE_COUNT],
            entry: 0,
            exports: vec![],
            bindings: vec![],
            debug: Vec::new(),
        }
    }

    #[test]
    fn rejects_a_missing_function_bound_table() {
        let mut module = module_with(vec![vec![ConstInt(0), Return]]);
        module.func_bounds.clear();
        let error = super::verify_module(&module).expect_err("the missing table rejects");
        assert!(error.message.contains("function-bound table"));
    }

    /// A module with one class `Counter { value: Int }` and one method
    /// `bump(self): Int` on selector 0, plus an entry function.
    fn class_module(entry_blocks: Vec<Vec<Instr>>) -> Module {
        let mut types = base_types();
        types.push(BcType::Class(0)); // type 4
        Module {
            strings: vec![],
            bytes: vec![],
            types,
            selectors: vec!["bump".to_string()],
            apps: vec![],
            interfaces: vec![],
            conformances: vec![],
            class_bounds: vec![vec![]],
            func_bounds: vec![vec![], vec![]],
            classes: vec![BcClass {
                name: "Counter".to_string(),
                parent_args: Vec::new(),
                key: "Counter".to_string(),
                is_final: false,
                is_frozen: false,
                parent: NO_PARENT,
                type_params: 0,
                kind: BcClassKind::Normal,
                fields: vec![("value".to_string(), TY_INT)],
                methods: vec![(0, 1)],
            }],
            funcs: vec![
                plain_func("main", vec![], TY_INT, entry_blocks),
                plain_func(
                    "bump",
                    vec![4],
                    TY_INT,
                    vec![vec![LoadLocal(0), LoadField(0), Return]],
                ),
            ],
            imports: vec![],
            slots: vec![],
            core_roles: [lm_bytecode::NO_ROLE; lm_bytecode::CORE_ROLE_COUNT],
            entry: 0,
            exports: vec![],
            bindings: vec![],
            debug: Vec::new(),
        }
    }

    /// A generic module: `Box[T] { value: T }`, its `<new>` function,
    /// and an entry that builds `Box[Int]` and reads the field.
    fn generic_module(entry_blocks: Vec<Vec<Instr>>) -> Module {
        let mut types = base_types();
        types.push(BcType::Var(0)); // 4
        types.push(BcType::Inst(0, vec![4])); // 5 Box[$0]
        types.push(BcType::Inst(0, vec![TY_INT])); // 6 Box[Int]
        Module {
            strings: vec![],
            bytes: vec![],
            types,
            selectors: vec![],
            apps: vec![
                TypeApp {
                    types: vec![TY_INT],
                    rows: vec![],
                },
                TypeApp {
                    types: vec![4],
                    rows: vec![],
                },
            ],
            interfaces: vec![],
            conformances: vec![],
            class_bounds: vec![vec![]],
            func_bounds: vec![vec![], vec![]],
            classes: vec![BcClass {
                name: "Box".to_string(),
                parent_args: Vec::new(),
                key: "Box".to_string(),
                is_final: false,
                is_frozen: false,
                parent: NO_PARENT,
                type_params: 1,
                kind: BcClassKind::Normal,
                fields: vec![("value".to_string(), 4)],
                methods: vec![],
            }],
            funcs: vec![
                plain_func("main", vec![], TY_INT, entry_blocks),
                Func {
                    name: "<new Box>".to_string(),
                    type_params: 1,
                    effect_params: 0,
                    params: vec![4],
                    param_muts: vec![false],
                    ret: 5,
                    row: vec![],
                    captures: vec![],
                    local_types: vec![4, 5],
                    blocks: vec![vec![
                        NewG { class: 0, app: 1 },
                        StoreLocal(1),
                        LoadLocal(1),
                        LoadLocal(0),
                        StoreField(0),
                        LoadLocal(1),
                        Return,
                    ]],
                },
            ],
            imports: vec![],
            slots: vec![],
            core_roles: [lm_bytecode::NO_ROLE; lm_bytecode::CORE_ROLE_COUNT],
            entry: 0,
            exports: vec![],
            bindings: vec![],
            debug: Vec::new(),
        }
    }

    #[test]
    fn accepts_simple_function() {
        let m = module_with(vec![vec![ConstInt(1), ConstInt(2), Add, Return]]);
        assert!(verify_module(&m).is_ok());
    }

    #[test]
    fn accepts_float_and_byte_instructions() {
        let mut module = module_with(vec![vec![
            ConstFloat(1.5f64.to_bits()),
            Instr::Numeric(NumericInstr::FloatBits),
            ConstBytes(0),
            Instr::Numeric(NumericInstr::BytesBitNot),
            Pop,
            Return,
        ]]);
        module.bytes.push(vec![0, 255]);
        assert!(verify_module(&module).is_ok());
    }

    #[test]
    fn rejects_a_noncanonical_float_constant() {
        let module = module_with(vec![vec![
            ConstFloat(0x7ff0_0000_0000_0001),
            Instr::Numeric(NumericInstr::FloatBits),
            Return,
        ]]);
        let error = verify_module(&module).expect_err("the NaN must reject");
        assert!(error.message.contains("noncanonical NaN"), "{error}");
    }

    #[test]
    fn rejects_a_byte_literal_outside_its_pool() {
        let module = module_with(vec![vec![ConstBytes(0), Pop, ConstInt(0), Return]]);
        let error = verify_module(&module).expect_err("the byte index must reject");
        assert!(error.message.contains("byte literal index"), "{error}");
    }

    #[test]
    fn rejects_a_numeric_instruction_type_mismatch() {
        let module = module_with(vec![vec![
            ConstBool(true),
            Instr::Numeric(NumericInstr::IntBitNot),
            Return,
        ]]);
        let error = verify_module(&module).expect_err("the operand type must reject");
        assert!(error.message.contains("expected type"), "{error}");
    }

    #[test]
    fn module_errors_do_not_print_a_function_sentinel() {
        let error = VerifyError {
            func: None,
            message: "the module is invalid".to_string(),
        };
        assert_eq!(error.to_string(), "the module is invalid");
    }

    #[test]
    fn accepts_class_construction_and_virtual_call() {
        let mut m = class_module(vec![vec![
            New(0),
            StoreLocal(0),
            LoadLocal(0),
            ConstInt(7),
            StoreField(0),
            LoadLocal(0),
            CallVirtual {
                selector: 0,
                argc: 0,
            },
            Return,
        ]]);
        // The entry stores a Counter into local 0, so the declared
        // slot type must accept it.
        m.funcs[0].local_types = vec![4, TY_INT];
        assert!(verify_module(&m).is_ok(), "{:?}", verify_module(&m));
    }

    #[test]
    fn accepts_generic_construction_and_field_read() {
        let m = generic_module(vec![vec![
            ConstInt(41),
            CallG { func: 1, app: 0 },
            LoadField(0),
            Return,
        ]]);
        assert!(verify_module(&m).is_ok(), "{:?}", verify_module(&m));
    }

    #[test]
    fn rejects_generic_call_with_wrong_result_use() {
        // Box[Int].value is Int; using it as Bool must fail.
        let m = generic_module(vec![vec![
            ConstInt(41),
            CallG { func: 1, app: 0 },
            LoadField(0),
            Not,
            Pop,
            ConstInt(0),
            Return,
        ]]);
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("expected type"), "{e}");
    }

    #[test]
    fn rejects_generic_call_without_application() {
        let m = generic_module(vec![vec![ConstInt(41), Call(1), LoadField(0), Return]]);
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("type application"), "{e}");
    }

    #[test]
    fn rejects_application_arity_mismatch() {
        let mut m = generic_module(vec![vec![
            ConstInt(41),
            CallG { func: 1, app: 0 },
            LoadField(0),
            Return,
        ]]);
        m.apps[0].types.push(TY_INT);
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("arity"), "{e}");
    }

    #[test]
    fn rejects_bad_jump_target() {
        let m = module_with(vec![vec![Jump(7)]]);
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("jump target"), "{e}");
    }

    #[test]
    fn rejects_wrong_stack_shape() {
        let m = module_with(vec![vec![ConstInt(1), Add, Return]]);
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("empty stack"), "{e}");
    }

    #[test]
    fn verifies_every_part_of_a_prepared_wait() {
        let mut module = module_with(vec![vec![
            ConstInt(1),
            Instr::Extended(
                ExtendedInstr::prepare_wait(lm_abi::OP_CLOCK_SLEEP, 1, TY_UNIT)
                    .expect("the wait instruction fits"),
            ),
            Return,
        ]]);
        let wait_ty = module.types.len() as u32;
        module.types.push(BcType::Wait(TY_UNIT));
        module.funcs[0].ret = wait_ty;
        let sleep_name = module.strings.len() as u32;
        module.strings.push("Clock.Sleep".to_string());
        module.funcs[0].row = vec![lm_bytecode::BcRow::Op(sleep_name)];
        verify_module(&module).expect("the prepared wait verifies");

        let mut nonwaitable = module.clone();
        nonwaitable.funcs[0].blocks[0][1] = Instr::Extended(
            ExtendedInstr::prepare_wait(lm_abi::OP_IO_WRITE, 1, TY_UNIT)
                .expect("the wait instruction fits"),
        );
        let error = verify_module(&nonwaitable).expect_err("the forged operation rejects");
        assert!(error.message.contains("not a wait source"), "{error}");

        let mut wrong_reply = module.clone();
        wrong_reply.funcs[0].blocks[0][1] = Instr::Extended(
            ExtendedInstr::prepare_wait(lm_abi::OP_CLOCK_SLEEP, 1, TY_INT)
                .expect("the wait instruction fits"),
        );
        let error = verify_module(&wrong_reply).expect_err("the forged reply type rejects");
        assert!(error.message.contains("another reply type"), "{error}");

        let mut missing_effect = module;
        missing_effect.funcs[0].row.clear();
        let error = verify_module(&missing_effect).expect_err("the missing effect rejects");
        assert!(error.message.contains("claimed row"), "{error}");
    }

    #[test]
    fn accepts_a_call_through_an_exact_function_slot() {
        let mut module = module_with(vec![vec![
            Instr::Extended(ExtendedInstr::CallSlot {
                slot: 0,
                app: lm_bytecode::NO_APP,
            }),
            Instr::Return,
        ]]);
        module.funcs.push(plain_func(
            "slot_target",
            vec![],
            TY_INT,
            vec![vec![Instr::ConstInt(1), Instr::Return]],
        ));
        module.func_bounds.push(vec![]);
        module.slots.push(lm_bytecode::SlotSpec {
            key: [1; 32],
            contract_hash: [0; 32],
            contract: SlotContract::Function(BcCallableContract {
                type_params: 0,
                effect_params: 0,
                type_bounds: vec![],
                params: vec![],
                param_muts: vec![],
                ret: TY_INT,
                row: vec![],
            }),
            initial: Some(SlotTarget::Function(1)),
        });
        verify_module(&module).expect("the slot call verifies");
    }

    #[test]
    fn rejects_an_incompatible_initial_slot_target() {
        let mut module = module_with(vec![vec![Instr::ConstInt(0), Instr::Return]]);
        module.funcs.push(plain_func(
            "wrong",
            vec![],
            TY_BOOL,
            vec![vec![Instr::ConstBool(true), Instr::Return]],
        ));
        module.func_bounds.push(vec![]);
        module.slots.push(lm_bytecode::SlotSpec {
            key: [2; 32],
            contract_hash: [0; 32],
            contract: SlotContract::Function(BcCallableContract {
                type_params: 0,
                effect_params: 0,
                type_bounds: vec![],
                params: vec![],
                param_muts: vec![],
                ret: TY_INT,
                row: vec![],
            }),
            initial: Some(SlotTarget::Function(1)),
        });
        let error = verify_module(&module).unwrap_err();
        assert!(error.message.contains("does not match the slot contract"));
    }

    #[test]
    fn rejects_a_slot_instruction_with_no_contract() {
        let module = module_with(vec![vec![
            Instr::Extended(ExtendedInstr::CallSlot {
                slot: 0,
                app: lm_bytecode::NO_APP,
            }),
            Instr::Return,
        ]]);
        let error = verify_module(&module).unwrap_err();
        assert!(error.message.contains("slot index out of range"));
    }

    #[test]
    fn rejects_type_confusion() {
        let m = module_with(vec![vec![ConstBool(true), ConstInt(2), Add, Return]]);
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("expected type"), "{e}");
    }

    #[test]
    fn rejects_missing_terminator() {
        let m = module_with(vec![vec![ConstInt(1)]]);
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("terminator"), "{e}");
    }

    #[test]
    fn rejects_load_before_store() {
        let m = module_with(vec![vec![LoadLocal(0), Return]]);
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("without a value"), "{e}");
    }

    #[test]
    fn rejects_missing_primitive_prefix() {
        let mut m = module_with(vec![vec![ConstInt(1), Return]]);
        m.types = vec![BcType::Int, BcType::Unit, BcType::Bool, BcType::Str];
        m.funcs[0].ret = 0;
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("does not start with"), "{e}");
    }

    #[test]
    fn rejects_duplicate_type_entry() {
        let mut m = module_with(vec![vec![ConstInt(1), Return]]);
        m.types.push(BcType::Int);
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("duplicates"), "{e}");
    }

    #[test]
    fn rejects_forward_type_reference() {
        let mut m = module_with(vec![vec![ConstInt(1), Return]]);
        m.types.push(BcType::List(9));
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("earlier entry"), "{e}");
    }

    #[test]
    fn rejects_invalid_map_key_type() {
        let mut m = module_with(vec![vec![ConstInt(1), Return]]);
        m.types.push(BcType::List(TY_INT)); // 4
        m.types.push(BcType::Map(4, TY_INT)); // key is a list
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("key type"), "{e}");
    }

    #[test]
    fn rejects_overlong_tuple_type() {
        let mut m = module_with(vec![vec![ConstInt(1), Return]]);
        m.types.push(BcType::Tuple(vec![TY_INT; 17]));
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("arity"), "{e}");
    }

    #[test]
    fn rejects_tuple_get_out_of_range() {
        let mut m = module_with(vec![vec![
            ConstInt(1),
            ConstInt(2),
            TupleNew { ty: 4, count: 2 },
            TupleGet(5),
            Return,
        ]]);
        m.types.push(BcType::Tuple(vec![TY_INT, TY_INT]));
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("tuple index"), "{e}");
    }

    #[test]
    fn rejects_tuple_new_count_mismatch() {
        let mut m = module_with(vec![vec![
            ConstInt(1),
            TupleNew { ty: 4, count: 1 },
            TupleGet(0),
            Return,
        ]]);
        m.types.push(BcType::Tuple(vec![TY_INT, TY_INT]));
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("arity"), "{e}");
    }

    #[test]
    fn rejects_new_on_abstract_class() {
        let mut m = class_module(vec![vec![New(1), Pop, ConstInt(0), Return]]);
        m.classes.push(BcClass {
            name: "Opt".to_string(),
            parent_args: Vec::new(),
            key: "Opt".to_string(),
            is_final: false,
            is_frozen: false,
            parent: NO_PARENT,
            type_params: 0,
            kind: BcClassKind::Abstract,
            fields: vec![],
            methods: vec![],
        });
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("abstract"), "{e}");
    }

    #[test]
    fn rejects_subclass_of_case_class() {
        let mut m = class_module(vec![vec![ConstInt(0), Return]]);
        m.classes.push(BcClass {
            name: "Opt".to_string(),
            parent_args: Vec::new(),
            key: "Opt".to_string(),
            is_final: false,
            is_frozen: false,
            parent: NO_PARENT,
            type_params: 0,
            kind: BcClassKind::Abstract,
            fields: vec![],
            methods: vec![],
        });
        m.classes.push(BcClass {
            name: "Opt.None".to_string(),
            parent_args: Vec::new(),
            key: "Opt.None".to_string(),
            is_final: false,
            is_frozen: false,
            parent: 1,
            type_params: 0,
            kind: BcClassKind::Case,
            fields: vec![],
            methods: vec![],
        });
        m.classes.push(BcClass {
            name: "Bad".to_string(),
            parent_args: Vec::new(),
            key: "Bad".to_string(),
            is_final: false,
            is_frozen: false,
            parent: 2,
            type_params: 0,
            kind: BcClassKind::Normal,
            fields: vec![],
            methods: vec![],
        });
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("case"), "{e}");
    }

    #[test]
    fn rejects_type_test_between_unrelated_classes() {
        let mut m = class_module(vec![vec![New(0), IsType(5), Pop, ConstInt(0), Return]]);
        m.types.push(BcType::Class(1)); // 5
        m.classes.push(BcClass {
            name: "Other".to_string(),
            parent_args: Vec::new(),
            key: "Other".to_string(),
            is_final: false,
            is_frozen: false,
            parent: NO_PARENT,
            type_params: 0,
            kind: BcClassKind::Normal,
            fields: vec![],
            methods: vec![],
        });
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("unrelated"), "{e}");
    }

    #[test]
    fn rejects_row_not_inside_caller() {
        // Callee claims Io.Print; caller declares the empty row.
        let mut m = module_with(vec![vec![Call(1), Return]]);
        m.strings = vec!["Io.Print".to_string()];
        m.funcs.push(Func {
            name: "printer".to_string(),
            type_params: 0,
            effect_params: 0,
            params: vec![],
            param_muts: vec![],
            ret: TY_INT,
            row: vec![BcRow::Op(0)],
            captures: vec![],
            local_types: vec![],
            blocks: vec![vec![ConstInt(1), Return]],
        });
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("row"), "{e}");
    }

    #[test]
    fn accepts_row_inside_caller_with_group() {
        // Caller declares Io; callee claims Io.Print.
        let mut m = module_with(vec![vec![Call(1), Return]]);
        m.strings = vec!["Io.Print".to_string(), "Io".to_string()];
        m.funcs[0].row = vec![BcRow::Op(1)];
        m.funcs.push(Func {
            name: "printer".to_string(),
            type_params: 0,
            effect_params: 0,
            params: vec![],
            param_muts: vec![],
            ret: TY_INT,
            row: vec![BcRow::Op(0)],
            captures: vec![],
            local_types: vec![],
            blocks: vec![vec![ConstInt(1), Return]],
        });
        assert!(verify_module(&m).is_ok(), "{:?}", verify_module(&m));
    }

    #[test]
    fn rejects_non_canonical_declared_row() {
        let mut m = module_with(vec![vec![ConstInt(1), Return]]);
        m.strings = vec!["Io".to_string(), "Fs".to_string()];
        m.funcs[0].row = vec![BcRow::Op(0), BcRow::Op(1)]; // Io before Fs
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("canonical"), "{e}");
    }

    #[test]
    fn rejects_row_var_outside_arity() {
        let mut m = module_with(vec![vec![ConstInt(1), Return]]);
        m.funcs[0].row = vec![BcRow::Var(0)];
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("effect variable"), "{e}");
    }

    #[test]
    fn rejects_field_index_out_of_range() {
        let m = class_module(vec![vec![New(0), LoadField(9), Return]]);
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("field index"), "{e}");
    }

    #[test]
    fn rejects_wrong_field_store_type() {
        let m = class_module(vec![vec![
            New(0),
            ConstBool(true),
            StoreField(0),
            ConstInt(0),
            Return,
        ]]);
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("field store"), "{e}");
    }

    #[test]
    fn rejects_unknown_selector_on_class() {
        let mut m = class_module(vec![vec![
            New(0),
            CallVirtual {
                selector: 1,
                argc: 0,
            },
            Return,
        ]]);
        m.selectors.push("other".to_string());
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("not a class method"), "{e}");
    }

    #[test]
    fn rejects_virtual_argc_mismatch() {
        let m = class_module(vec![vec![
            New(0),
            ConstInt(1),
            CallVirtual {
                selector: 0,
                argc: 1,
            },
            Return,
        ]]);
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("argument count"), "{e}");
    }

    #[test]
    fn rejects_method_with_wrong_self_type() {
        let mut m = class_module(vec![vec![ConstInt(0), Return]]);
        m.funcs[1].params = vec![TY_INT];
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("`self`"), "{e}");
    }

    #[test]
    fn rejects_override_that_changes_parameters() {
        let mut m = class_module(vec![vec![ConstInt(0), Return]]);
        // A subclass whose bump takes an extra Int.
        m.types.push(BcType::Class(1)); // 5
        m.classes.push(BcClass {
            name: "Fast".to_string(),
            parent_args: Vec::new(),
            key: "Fast".to_string(),
            is_final: false,
            is_frozen: false,
            parent: 0,
            type_params: 0,
            kind: BcClassKind::Normal,
            fields: vec![("value".to_string(), TY_INT)],
            methods: vec![(0, 2)],
        });
        m.funcs.push(plain_func(
            "bump2",
            vec![5, TY_INT],
            TY_INT,
            vec![vec![ConstInt(1), Return]],
        ));
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("parameter types"), "{e}");
    }

    #[test]
    fn rejects_override_that_widens_the_row() {
        let mut m = class_module(vec![vec![ConstInt(0), Return]]);
        m.strings = vec!["Io.Print".to_string()];
        m.types.push(BcType::Class(1)); // 5
        m.classes.push(BcClass {
            name: "Loud".to_string(),
            parent_args: Vec::new(),
            key: "Loud".to_string(),
            is_final: false,
            is_frozen: false,
            parent: 0,
            type_params: 0,
            kind: BcClassKind::Normal,
            fields: vec![("value".to_string(), TY_INT)],
            methods: vec![(0, 2)],
        });
        m.funcs.push(Func {
            name: "bump2".to_string(),
            type_params: 0,
            effect_params: 0,
            params: vec![5],
            param_muts: vec![false],
            ret: TY_INT,
            row: vec![BcRow::Op(0)],
            captures: vec![],
            local_types: vec![5],
            blocks: vec![vec![ConstInt(1), Return]],
        });
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("widens the effect row"), "{e}");
    }

    #[test]
    fn rejects_layout_that_breaks_parent_prefix() {
        let mut m = class_module(vec![vec![ConstInt(0), Return]]);
        m.types.push(BcType::Class(1));
        m.classes.push(BcClass {
            name: "Bad".to_string(),
            parent_args: Vec::new(),
            key: "Bad".to_string(),
            is_final: false,
            is_frozen: false,
            parent: 0,
            type_params: 0,
            kind: BcClassKind::Normal,
            fields: vec![("other".to_string(), TY_BOOL)],
            methods: vec![],
        });
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("parent layout"), "{e}");
    }

    #[test]
    fn rejects_direct_call_to_captured_function() {
        let mut m = module_with(vec![vec![Call(1), Return]]);
        m.funcs.push(Func {
            name: "closure".to_string(),
            type_params: 0,
            effect_params: 0,
            params: vec![],
            param_muts: vec![],
            ret: TY_INT,
            row: vec![],
            captures: vec![TY_INT],
            local_types: vec![],
            blocks: vec![vec![LoadCapture(0), Return]],
        });
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("captures"), "{e}");
    }

    #[test]
    fn rejects_closure_without_fn_type_entry() {
        let mut m = module_with(vec![vec![
            ConstInt(1),
            MakeClosure {
                func: 1,
                captures: 1,
            },
            Pop,
            ConstInt(0),
            Return,
        ]]);
        m.funcs.push(Func {
            name: "closure".to_string(),
            type_params: 0,
            effect_params: 0,
            params: vec![],
            param_muts: vec![],
            ret: TY_INT,
            row: vec![],
            captures: vec![TY_INT],
            local_types: vec![],
            blocks: vec![vec![LoadCapture(0), Return]],
        });
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("type table"), "{e}");
    }

    #[test]
    fn accepts_closure_create_and_call() {
        let mut m = module_with(vec![vec![
            ConstInt(41),
            MakeClosure {
                func: 1,
                captures: 1,
            },
            ConstInt(1),
            CallValue { argc: 1 },
            Return,
        ]]);
        m.types
            .push(BcType::Fn(vec![TY_INT], vec![false], TY_INT, vec![]));
        m.funcs.push(Func {
            name: "closure".to_string(),
            type_params: 0,
            effect_params: 0,
            params: vec![TY_INT],
            param_muts: vec![false],
            ret: TY_INT,
            row: vec![],
            captures: vec![TY_INT],
            local_types: vec![TY_INT],
            blocks: vec![vec![LoadCapture(0), LoadLocal(0), Add, Return]],
        });
        assert!(verify_module(&m).is_ok(), "{:?}", verify_module(&m));
    }

    #[test]
    fn rejects_call_value_row_outside_caller() {
        let mut m = module_with(vec![vec![
            MakeClosure {
                func: 1,
                captures: 0,
            },
            CallValue { argc: 0 },
            Return,
        ]]);
        m.strings = vec!["Io.Print".to_string()];
        m.types
            .push(BcType::Fn(vec![], vec![], TY_INT, vec![BcRow::Op(0)]));
        m.funcs.push(Func {
            name: "printer".to_string(),
            type_params: 0,
            effect_params: 0,
            params: vec![],
            param_muts: vec![],
            ret: TY_INT,
            row: vec![BcRow::Op(0)],
            captures: vec![],
            local_types: vec![],
            blocks: vec![vec![ConstInt(1), Return]],
        });
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("row"), "{e}");
    }

    #[test]
    fn rejects_call_value_on_non_function() {
        let m = module_with(vec![vec![ConstInt(1), CallValue { argc: 0 }, Return]]);
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("not a function type"), "{e}");
    }

    #[test]
    fn rejects_capture_index_out_of_range() {
        let m = module_with(vec![vec![LoadCapture(0), Return]]);
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("capture index"), "{e}");
    }

    #[test]
    fn rejects_list_element_type_mismatch() {
        let mut m = module_with(vec![vec![
            ConstBool(true),
            ListNew { ty: 4, count: 1 },
            Pop,
            ConstInt(0),
            Return,
        ]]);
        m.types.push(BcType::List(TY_INT));
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("expected type"), "{e}");
    }

    #[test]
    fn rejects_freeze_on_scalar() {
        let m = module_with(vec![vec![ConstInt(1), Freeze, Return]]);
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("freeze"), "{e}");
    }

    /// The digest reads a heap graph. A scalar has no graph, so the
    /// verifier rejects the instruction instead of letting the VM meet
    /// a value that is not an object.
    #[test]
    fn rejects_digest_on_a_scalar() {
        let mut m = module_with(vec![vec![ConstInt(1), Digest { ty: TY_INT }, Return]]);
        m.types.push(BcType::Digest);
        m.funcs[0].ret = 4;
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("digest on non-object type"), "{e}");
    }

    /// A digest comparison reads two digests. Any other operand type
    /// rejects, so the value comparison in the VM cannot meet a shape
    /// that carries no digest.
    #[test]
    fn rejects_digest_comparison_on_other_types() {
        let m = module_with(vec![vec![ConstInt(1), ConstInt(2), EqDigest, Return]]);
        let e = verify_module(&m).unwrap_err();
        assert!(
            e.message.contains("digest comparison on non-digest types"),
            "{e}"
        );
        let m = module_with(vec![vec![ConstInt(1), ConstInt(2), NeDigest, Return]]);
        let e = verify_module(&m).unwrap_err();
        assert!(
            e.message.contains("digest comparison on non-digest types"),
            "{e}"
        );
        // A string is a heap value and still not a digest.
        let m = module_with(vec![vec![ConstStr(0), ConstStr(0), EqDigest, Return]]);
        let e = verify_module(&m).unwrap_err();
        assert!(
            e.message.contains("digest comparison on non-digest types"),
            "{e}"
        );
    }

    /// The digest result type must exist in the module type table.
    /// A module that omits it rejects instead of resolving to a
    /// neighbouring type.
    #[test]
    fn rejects_digest_without_the_result_type() {
        let m = module_with(vec![vec![ConstStr(0), Digest { ty: TY_STR }, Return]]);
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("Digest is not in the type table"), "{e}");
    }

    #[test]
    fn rejects_entry_with_parameters() {
        let mut m = module_with(vec![vec![ConstInt(1), Return]]);
        m.funcs[0].params = vec![TY_INT];
        m.funcs[0].param_muts = vec![false];
        m.funcs[0].local_types = vec![TY_INT, TY_INT];
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("entry function"), "{e}");
    }

    #[test]
    fn rejects_generic_entry() {
        let mut m = module_with(vec![vec![ConstInt(1), Return]]);
        m.funcs[0].type_params = 1;
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("generic"), "{e}");
    }

    #[test]
    fn joins_subclass_stacks_at_merge_points() {
        // Both branches push a different subclass of Animal. The join
        // must settle at the common ancestor.
        let mut types = base_types();
        types.push(BcType::Class(0)); // 4 Animal
        types.push(BcType::Class(1)); // 5 Dog
        types.push(BcType::Class(2)); // 6 Cat
        let class = |name: &str, parent: u32| BcClass {
            name: name.to_string(),
            parent_args: Vec::new(),
            key: name.to_string(),
            is_final: false,
            is_frozen: false,
            parent,
            type_params: 0,
            kind: BcClassKind::Normal,
            fields: vec![],
            methods: vec![],
        };
        let m = Module {
            strings: vec![],
            bytes: vec![],
            types,
            selectors: vec![],
            apps: vec![],
            interfaces: vec![],
            conformances: vec![],
            class_bounds: vec![vec![], vec![], vec![]],
            func_bounds: vec![vec![]],
            classes: vec![class("Animal", NO_PARENT), class("Dog", 0), class("Cat", 0)],
            funcs: vec![Func {
                name: "main".to_string(),
                type_params: 0,
                effect_params: 0,
                params: vec![],
                param_muts: vec![],
                ret: TY_UNIT,
                row: vec![],
                captures: vec![],
                local_types: vec![],
                blocks: vec![
                    vec![ConstBool(true), JumpIfFalse(1), New(1), Jump(2)],
                    vec![New(2), Jump(2)],
                    vec![Pop, ConstUnit, Return],
                ],
            }],
            imports: vec![],
            slots: vec![],
            core_roles: [lm_bytecode::NO_ROLE; lm_bytecode::CORE_ROLE_COUNT],
            entry: 0,
            exports: vec![],
            bindings: vec![],
            debug: Vec::new(),
        };
        assert!(verify_module(&m).is_ok(), "{:?}", verify_module(&m));
    }

    /// A class that inherits an instantiated generic parent fits that
    /// exact application and no other.
    ///
    /// `IntBox` inherits `Box[Int]` and takes no type parameter of its
    /// own. A rule that compared class names alone accepted it at a
    /// `Box[String]` position. The subtype rule walks the arguments,
    /// so the plain class position and the application position both
    /// answer the same relation.
    #[test]
    fn an_inherited_generic_parent_fits_one_application() {
        let class = |name: &str, parent: u32, args: Vec<u32>, params: u32| BcClass {
            name: name.to_string(),
            key: name.to_string(),
            is_final: false,
            is_frozen: false,
            parent,
            parent_args: args,
            type_params: params,
            kind: lm_bytecode::BcClassKind::Normal,
            fields: vec![],
            methods: vec![],
        };
        let mut m = module_with(vec![vec![ConstInt(0), Return]]);
        m.types = base_types();
        m.types.push(BcType::Var(0)); // 4
        m.types.push(BcType::Inst(0, vec![TY_INT])); // 5 Box[Int]
        m.types.push(BcType::Inst(0, vec![TY_STR])); // 6 Box[String]
        m.types.push(BcType::Class(1)); // 7 IntBox
        m.classes = vec![
            class("Box", NO_PARENT, vec![], 1),
            class("IntBox", 0, vec![TY_INT], 0),
        ];
        complete_bounds(&mut m);
        let core = lm_bytecode::corepin::declared_layout(&m);
        let ctx = verify_tables(&m, core, lm_abi::standard_bundle()).expect("the tables verify");
        assert!(ctx.is_subtype(7, 5), "an IntBox fits Box[Int]");
        assert!(!ctx.is_subtype(7, 6), "an IntBox fits no Box[String]");
    }

    /// Sibling classes join through the full application of a generic
    /// parent. A class slot alone would lose the parent's argument.
    #[test]
    fn sibling_classes_join_at_one_generic_parent_application() {
        let class = |name: &str, parent: u32, args: Vec<u32>, params: u32| BcClass {
            name: name.to_string(),
            key: name.to_string(),
            is_final: false,
            is_frozen: false,
            parent,
            parent_args: args,
            type_params: params,
            kind: lm_bytecode::BcClassKind::Normal,
            fields: vec![],
            methods: vec![],
        };
        let mut m = module_with(vec![vec![ConstInt(0), Return]]);
        m.types = base_types();
        m.types.push(BcType::Class(1)); // 4 IntLeft
        m.types.push(BcType::Class(2)); // 5 IntRight
        m.types.push(BcType::Class(3)); // 6 StringChild
        m.classes = vec![
            class("Box", NO_PARENT, vec![], 1),
            class("IntLeft", 0, vec![TY_INT], 0),
            class("IntRight", 0, vec![TY_INT], 0),
            class("StringChild", 0, vec![TY_STR], 0),
        ];
        complete_bounds(&mut m);
        let core = lm_bytecode::corepin::declared_layout(&m);
        let ctx = verify_tables(&m, core, lm_abi::standard_bundle()).expect("the tables verify");

        let joined = ctx.join(4, 5).expect("the siblings join");
        assert_eq!(ctx.ty(joined), BcType::Inst(0, vec![TY_INT]));
        assert_eq!(ctx.join(4, 6), None, "different applications do not join");
    }

    /// Shared type children form a DAG, not a tree. Each verifier walk
    /// must visit one node or pair once.
    #[test]
    fn shared_type_dags_do_not_duplicate_verifier_work() {
        const DEPTH: usize = 40;
        let class = |name: &str, parent: u32| BcClass {
            name: name.to_string(),
            key: name.to_string(),
            is_final: false,
            is_frozen: false,
            parent,
            parent_args: vec![],
            type_params: 0,
            kind: lm_bytecode::BcClassKind::Normal,
            fields: vec![],
            methods: vec![],
        };
        let mut types = base_types();
        types.push(BcType::Var(0));
        let mut bounded = (types.len() - 1) as u32;
        for _ in 0..DEPTH {
            types.push(BcType::Tuple(vec![bounded, bounded]));
            bounded = (types.len() - 1) as u32;
        }
        types.push(BcType::Class(1));
        let mut left = (types.len() - 1) as u32;
        types.push(BcType::Class(2));
        let mut right = (types.len() - 1) as u32;
        for _ in 0..DEPTH {
            types.push(BcType::Tuple(vec![left, left]));
            left = (types.len() - 1) as u32;
            types.push(BcType::Tuple(vec![right, right]));
            right = (types.len() - 1) as u32;
        }
        let mut m = module_with(vec![vec![ConstInt(0), Return]]);
        m.types = types;
        m.classes = vec![
            class("Parent", NO_PARENT),
            class("Left", 0),
            class("Right", 0),
        ];
        complete_bounds(&mut m);
        let core = lm_bytecode::corepin::declared_layout(&m);
        let ctx = verify_tables(&m, core, lm_abi::standard_bundle()).expect("the tables verify");

        assert!(ctx.vars_bounded(bounded, 1, 0));
        assert!(!ctx.is_subtype(left, right));
        assert!(ctx.join(left, right).is_some());
    }

    /// A type table nests no deeper than `MAX_TYPE_DEPTH`.
    ///
    /// An artifact states its own type table, and a hand-built one can
    /// nest a type as deeply as the table holds entries. Every walk
    /// over a type costs at least its depth, and `join` costs the
    /// square of it. The bound therefore makes a deep type
    /// unrepresentable instead of hardening each walk against one.
    #[test]
    fn a_type_table_past_the_depth_bound_rejects() {
        let mut types = base_types();
        let mut deep = TY_INT;
        for _ in 0..MAX_TYPE_DEPTH {
            types.push(BcType::List(deep));
            deep = (types.len() - 1) as u32;
        }
        let mut m = module_with(vec![vec![ConstInt(0), Return]]);
        m.types = types;
        let error = verify_module(&m).expect_err("a type past the bound rejects");
        assert!(
            format!("{error:?}").contains("nests"),
            "the diagnostic names the depth rule: {error:?}"
        );
    }

    /// Every type walk answers at the bound on a small stack.
    #[test]
    fn a_type_table_at_the_depth_bound_walks_on_a_small_stack() {
        std::thread::Builder::new()
            .stack_size(256 * 1024)
            .spawn(|| {
                // `Var(0)` is depth 1, so this many list levels reach
                // the bound exactly.
                const DEPTH: u32 = MAX_TYPE_DEPTH - 1;
                // `join` tests the subtype relation at each level, so
                // its cost grows with the square of the depth. The
                // bound keeps that cost small.
                const JOIN_DEPTH: u32 = MAX_TYPE_DEPTH - 2;
                let class = |name: &str, parent: u32| BcClass {
                    name: name.to_string(),
                    key: name.to_string(),
                    is_final: false,
                    is_frozen: false,
                    parent,
                    parent_args: vec![],
                    type_params: 0,
                    kind: lm_bytecode::BcClassKind::Normal,
                    fields: vec![],
                    methods: vec![],
                };
                let mut types = base_types();
                // `[[[ ... [T] ... ]]]`, nested `DEPTH` deep over the
                // type variable of a generic function.
                types.push(BcType::Var(0));
                let mut deep = (types.len() - 1) as u32;
                for _ in 0..DEPTH {
                    types.push(BcType::List(deep));
                    deep = (types.len() - 1) as u32;
                }
                // Two tuple chains of the same depth, one over each
                // child class. Their join walks to the common parent
                // at the innermost position.
                types.push(BcType::Class(1));
                let mut left = (types.len() - 1) as u32;
                types.push(BcType::Class(2));
                let mut right = (types.len() - 1) as u32;
                for _ in 0..JOIN_DEPTH {
                    types.push(BcType::Tuple(vec![left]));
                    left = (types.len() - 1) as u32;
                    types.push(BcType::Tuple(vec![right]));
                    right = (types.len() - 1) as u32;
                }
                let mut callee = plain_func("deep", vec![deep], TY_INT, vec![]);
                callee.type_params = 1;
                callee.local_types = vec![deep];
                callee.blocks = vec![vec![ConstInt(0), Return]];
                let mut m = module_with(vec![vec![ConstInt(0), Return]]);
                m.types = types;
                m.classes = vec![class("P", NO_PARENT), class("A", 0), class("B", 0)];
                m.apps = vec![TypeApp {
                    types: vec![TY_INT],
                    rows: vec![],
                }];
                m.funcs.push(callee);
                complete_bounds(&mut m);
                // The whole pass runs first: the table rules read every
                // entry, and `vars_bounded` walks the deep parameter.
                verify_module(&m).expect("the module verifies");
                // The three remaining walks run directly, because no
                // small program reaches a type this deep.
                let core = lm_bytecode::corepin::declared_layout(&m);
                let ctx =
                    verify_tables(&m, core, lm_abi::standard_bundle()).expect("the tables verify");
                assert!(ctx.vars_bounded(deep, 1, 0));
                let closed = ctx.subst(deep, &[TY_INT], &[]);
                assert_ne!(closed, deep);
                assert!(ctx.is_subtype(closed, closed));
                assert!(!ctx.is_subtype(left, right));
                assert!(ctx.join(left, right).is_some());
            })
            .expect("thread starts")
            .join()
            .expect("no Rust stack overflow");
    }
}
