//! Compile one source module against a frozen compile environment.
//!
//! The result is the artifact container, the interface, and the
//! hashes. The step is pure: it reads no file and writes no file.

use crate::env::FrozenCompileEnv;
use lm_bytecode::interface::{IfaceItem, IfaceSlotKind, IfaceSlotSpec, Interface};
use lm_bytecode::{BcType, ExtendedInstr, Instr, Module};
use lm_hir::hir::{
    HExpr, HExprKind, HForKind, HInterpPart, HStmt, HirClass, HirInterfaceUse, HirModule,
};
use lm_hir::{LateCallable, LateCallableKind, LowerLinkage};
use lm_source::SourceFile;
use lm_types::{RowElem, Type, TypeId};
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// Linkage choices for one compiler invocation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompileOptions {
    /// Package the main entry result with its closed static type.
    pub dynamic_result: bool,
    /// Give every named source definition late linkage.
    pub late_definitions: bool,
    /// Qualified or module-local function binding names.
    pub late_functions: BTreeSet<String>,
    /// Qualified or module-local class binding names.
    pub late_classes: BTreeSet<String>,
}

impl CompileOptions {
    pub fn new() -> CompileOptions {
        CompileOptions::default()
    }

    pub fn late_definitions(mut self) -> CompileOptions {
        self.late_definitions = true;
        self
    }

    pub fn dynamic_result(mut self) -> CompileOptions {
        self.dynamic_result = true;
        self
    }

    pub fn late_function(mut self, name: impl Into<String>) -> CompileOptions {
        self.late_functions.insert(name.into());
        self
    }

    pub fn late_class(mut self, name: impl Into<String>) -> CompileOptions {
        self.late_classes.insert(name.into());
        self
    }
}

/// One compiled module.
#[derive(Debug, Clone)]
pub struct CompiledModule {
    /// The full module path, for example `mathlib.matrix`.
    pub path: String,
    pub module: Module,
    pub artifact: Vec<u8>,
    pub interface: Interface,
    pub interface_bytes: Vec<u8>,
    pub semantic_hash: [u8; 32],
    pub container_hash: [u8; 32],
}

/// Compile one module. `Err` carries fully rendered diagnostic text.
///
/// `is_main` marks the module that holds the program entry. Every
/// other module must end without a trailing expression, because a
/// library module has no entry to run.
pub fn compile_module(
    path: &str,
    source: &SourceFile,
    env: &FrozenCompileEnv,
    is_main: bool,
) -> Result<CompiledModule, String> {
    compile_module_with_options(path, source, env, is_main, &CompileOptions::default())
}

/// Compile one module with explicit linkage choices.
pub fn compile_module_with_options(
    path: &str,
    source: &SourceFile,
    env: &FrozenCompileEnv,
    is_main: bool,
    options: &CompileOptions,
) -> Result<CompiledModule, String> {
    if options.dynamic_result && !is_main {
        return Err("error: a dynamic result needs a main module\n".to_string());
    }
    let (ast, syntax) =
        lm_source::syntax::parse_complete(&source.text).map_err(|d| d.render(source))?;
    if !is_main && !ast.entry.is_empty() {
        let span = ast.entry[0].span;
        let diagnostic = lm_source::diag::Diagnostic::new(
            "E1053",
            format!(
                "the module `{path}` ends with an expression, and only \
                 `src/main.lm` holds the program entry"
            ),
            span,
        );
        return Err(diagnostic.render(source));
    }
    let hir = lm_hir::check_module_with(
        &ast,
        lm_hir::CheckOptions {
            prelude: true,
            module_path: path.to_string(),
            imports: env.imports().clone(),
        },
    )
    .map_err(|d| d.render(source))?;
    let (linkage, interface_slots) = select_linkage(path, &hir, env, options)?;
    let mut module = lm_hir::lower_module_with_linkage(&hir, &linkage)
        .map_err(|error| format!("error: `{path}`: {error}\n"))?;
    if options.dynamic_result {
        package_dynamic_entry(&mut module, path)?;
    }
    lm_verify::verify_module(&module)
        .map_err(|e| format!("error: the verifier rejected `{path}`: {e}\n"))?;
    let identity = lm_bytecode::identity::module_identity(&module)
        .map_err(|e| format!("error: `{path}`: {e}\n"))?;
    attach_source_debug(&mut module, source, syntax, &ast, &hir, &linkage)?;
    let items: Vec<IfaceItem> = hir.exports.iter().map(|e| e.item.clone()).collect();
    let mut interface = lm_bytecode::interface::build_interface(&module, &identity, path, &items)
        .map_err(|e| format!("error: `{path}`: {e}\n"))?;
    interface.slots = interface_slots;
    let interface_bytes = lm_bytecode::interface::encode_interface(&interface);
    let artifact = lm_bytecode::encode(&module);
    let container_hash = lm_bytecode::identity::container_hash(&artifact);
    Ok(CompiledModule {
        path: path.to_string(),
        module,
        artifact,
        interface,
        interface_bytes,
        semantic_hash: identity.semantic_hash,
        container_hash,
    })
}

pub(crate) fn attach_source_debug(
    module: &mut Module,
    source: &SourceFile,
    parsed: lm_source::syntax::PublicSyntax,
    ast: &lm_source::ast::Module,
    hir: &lm_hir::HirModule,
    linkage: &LowerLinkage,
) -> Result<(), String> {
    use lm_bytecode::debug::{
        DebugCodeOrigin, DebugDefinition, DebugFunction, DebugInfo, DebugSource, DefinitionKind,
    };
    use lm_source::Span;

    let view =
        lm_abi::syntax::SyntaxView::new(&parsed.records, source.text.len()).map_err(|error| {
            format!(
                "error: `{}` has invalid public syntax: {error}\n",
                source.name
            )
        })?;
    let root = view.record(view.root()).map_err(|error| {
        format!(
            "error: `{}` has invalid public syntax: {error}\n",
            source.name
        )
    })?;
    let mut syntax_records = HashMap::with_capacity(root.child_len as usize);
    for offset in 0..root.child_len {
        let index = view.child(root, offset).map_err(|error| {
            format!(
                "error: `{}` has invalid public syntax: {error}\n",
                source.name
            )
        })?;
        let record = view.record(index).map_err(|error| {
            format!(
                "error: `{}` has invalid public syntax: {error}\n",
                source.name
            )
        })?;
        syntax_records
            .entry((record.kind, record.lo, record.hi))
            .or_insert(index);
    }
    let syntax_index = |kind: u16, span: Span| {
        syntax_records
            .get(&(kind, span.lo, span.hi))
            .copied()
            .ok_or_else(|| {
                format!(
                    "error: `{}` has no syntax record for a definition\n",
                    source.name
                )
            })
    };

    let function_spans: HashMap<&str, Span> = ast
        .funcs
        .iter()
        .map(|item| (item.name.as_str(), item.span))
        .collect();
    let class_spans: HashMap<&str, Span> = ast
        .classes
        .iter()
        .map(|item| (item.name.as_str(), item.span))
        .collect();
    let enum_spans: HashMap<&str, Span> = ast
        .enums
        .iter()
        .map(|item| (item.name.as_str(), item.span))
        .collect();
    let mut origins = HashMap::new();

    let mut definitions = Vec::new();
    for export in &module.exports {
        let (kind, span, syntax_kind) = match export.kind {
            lm_bytecode::ExportKind::Function => {
                let span = function_spans.get(export.name.as_str()).ok_or_else(|| {
                    format!("error: `{}` has no exported function source\n", source.name)
                })?;
                (
                    DefinitionKind::Function,
                    *span,
                    lm_abi::syntax::KIND_FUNCTION,
                )
            }
            lm_bytecode::ExportKind::Class => {
                let span = class_spans.get(export.name.as_str()).ok_or_else(|| {
                    format!("error: `{}` has no exported class source\n", source.name)
                })?;
                (DefinitionKind::Class, *span, lm_abi::syntax::KIND_CLASS)
            }
            lm_bytecode::ExportKind::Enum | lm_bytecode::ExportKind::EnumCase => {
                let family = export.name.split('.').next().unwrap_or(&export.name);
                let span = enum_spans.get(family).ok_or_else(|| {
                    format!("error: `{}` has no exported enum source\n", source.name)
                })?;
                (DefinitionKind::Class, *span, lm_abi::syntax::KIND_ENUM)
            }
            lm_bytecode::ExportKind::Interface => continue,
        };
        let origin_key = (kind, span.lo, span.hi);
        let origin = match origins.get(&origin_key) {
            Some(origin) => *origin,
            None => {
                let origin = lm_bytecode::debug::definition_origin(
                    &source.name,
                    &source.text,
                    kind,
                    span.lo,
                    span.hi,
                )
                .map_err(|error| {
                    format!(
                        "error: `{}` has invalid source origin: {error}\n",
                        source.name
                    )
                })?;
                origins.insert(origin_key, origin);
                origin
            }
        };
        definitions.push(DebugDefinition {
            kind,
            target: export.def,
            source: 0,
            lo: span.lo,
            hi: span.hi,
            syntax: syntax_index(syntax_kind, span)?,
            origin,
        });
    }

    let mut functions = Vec::new();
    for (index, function) in hir.funcs.iter().enumerate() {
        if let Some(span) = function.source_span {
            functions.push(DebugFunction {
                function: index as u32,
                source: 0,
                lo: span.lo,
                hi: span.hi,
            });
        }
    }
    let constructor_base = hir.funcs.len() as u32;
    for (index, class) in hir.classes.iter().enumerate() {
        if let Some(span) = class.source_span {
            functions.push(DebugFunction {
                function: constructor_base + index as u32,
                source: 0,
                lo: span.lo,
                hi: span.hi,
            });
        }
    }
    let dispatch_base = constructor_base + hir.classes.len() as u32;
    for (offset, class) in linkage.dynamic_classes.iter().enumerate() {
        if let Some(span) = hir.classes[*class as usize].source_span {
            functions.push(DebugFunction {
                function: dispatch_base + offset as u32,
                source: 0,
                lo: span.lo,
                hi: span.hi,
            });
        }
    }
    let definition_origins: HashMap<(DefinitionKind, u32), [u8; 32]> = definitions
        .iter()
        .map(|definition| ((definition.kind, definition.target), definition.origin))
        .collect();
    let mut code_origins = Vec::new();
    for (function, body) in module.funcs.iter().enumerate() {
        for (block, instructions) in body.blocks.iter().enumerate() {
            for (instruction, item) in instructions.iter().enumerate() {
                let origin = match item {
                    Instr::Extended(ExtendedInstr::FunctionCode { func }) => {
                        definition_origins.get(&(DefinitionKind::Function, *func))
                    }
                    Instr::Extended(ExtendedInstr::ClassCode { class }) => {
                        definition_origins.get(&(DefinitionKind::Class, *class))
                    }
                    _ => None,
                };
                let Some(origin) = origin else { continue };
                code_origins.push(DebugCodeOrigin {
                    function: u32::try_from(function).map_err(|_| {
                        format!("error: `{}` has too many functions\n", source.name)
                    })?,
                    block: u32::try_from(block)
                        .map_err(|_| format!("error: `{}` has too many blocks\n", source.name))?,
                    instruction: u32::try_from(instruction).map_err(|_| {
                        format!("error: `{}` has too many instructions\n", source.name)
                    })?,
                    origin: *origin,
                });
            }
        }
    }
    let info = DebugInfo {
        sources: vec![DebugSource {
            path: source.name.clone(),
            text: source.text.clone(),
            syntax: parsed.records,
        }],
        definitions,
        functions,
        code_origins,
    };
    lm_bytecode::debug::validate(&info, module)
        .map_err(|error| format!("error: `{}` has invalid debug data: {error}\n", source.name))?;
    module.debug = lm_bytecode::debug::encode(&info);
    Ok(())
}

fn package_dynamic_entry(module: &mut Module, path: &str) -> Result<(), String> {
    let entry = module.entry as usize;
    let result = module
        .funcs
        .get(entry)
        .map(|function| function.ret)
        .ok_or_else(|| format!("error: `{path}` has no entry function\n"))?;
    let class = module.core_roles[lm_bytecode::corepin::ROLE_DYN_VALUE];
    if class == lm_bytecode::NO_ROLE {
        return Err(format!("error: `{path}` has no DynValue core role\n"));
    }
    let package = module
        .types
        .iter()
        .position(|ty| *ty == BcType::Class(class))
        .map(|index| index as u32)
        .unwrap_or_else(|| {
            let index = module.types.len() as u32;
            module.types.push(BcType::Class(class));
            index
        });
    let function = &mut module.funcs[entry];
    let mut packed = false;
    for block in &mut function.blocks {
        if matches!(block.last(), Some(Instr::Return)) {
            block.insert(
                block.len() - 1,
                Instr::Extended(ExtendedInstr::DynPack { ty: result }),
            );
            packed = true;
        }
    }
    if !packed {
        return Err(format!("error: `{path}` has no returning entry path\n"));
    }
    function.ret = package;
    for slot in &mut module.slots {
        if slot.initial != Some(lm_bytecode::SlotTarget::Function(module.entry)) {
            continue;
        }
        match &mut slot.contract {
            lm_bytecode::SlotContract::Function(contract) => contract.ret = package,
            _ => {
                return Err(format!(
                    "error: `{path}` has an invalid entry binding contract\n"
                ));
            }
        }
    }
    Ok(())
}

fn contract_u32(out: &mut Vec<u8>, value: usize) {
    out.extend_from_slice(&(value as u32).to_le_bytes());
}

fn contract_text(out: &mut Vec<u8>, value: &str) {
    contract_u32(out, value.len());
    out.extend_from_slice(value.as_bytes());
}

fn encode_contract_row(out: &mut Vec<u8>, hir: &lm_hir::HirModule, row: &[RowElem]) {
    contract_u32(out, row.len());
    for item in row {
        match item {
            RowElem::Op(name) => {
                out.push(0);
                contract_text(out, hir.store.row_name(*name));
            }
            RowElem::Var(index) => {
                out.push(1);
                out.extend_from_slice(&index.to_le_bytes());
            }
        }
    }
}

fn encode_contract_type(
    out: &mut Vec<u8>,
    hir: &lm_hir::HirModule,
    ty: TypeId,
) -> Result<(), String> {
    match hir.store.get(ty) {
        Type::Unit => out.push(0),
        Type::Bool => out.push(1),
        Type::Int => out.push(2),
        Type::String => out.push(3),
        Type::Never => out.push(4),
        Type::Bytes => out.push(5),
        Type::Digest => out.push(6),
        Type::Class(class) => {
            out.push(7);
            let class = hir
                .classes
                .get(class.0 as usize)
                .ok_or_else(|| "a slot contract names no class".to_string())?;
            contract_text(out, &class.key);
        }
        Type::Inst(class, args) => {
            out.push(8);
            let class = hir
                .classes
                .get(class.0 as usize)
                .ok_or_else(|| "a slot contract names no class".to_string())?;
            contract_text(out, &class.key);
            contract_u32(out, args.len());
            for arg in args {
                encode_contract_type(out, hir, *arg)?;
            }
        }
        Type::List(element) => {
            out.push(9);
            encode_contract_type(out, hir, *element)?;
        }
        Type::Map(key, value) => {
            out.push(10);
            encode_contract_type(out, hir, *key)?;
            encode_contract_type(out, hir, *value)?;
        }
        Type::Tuple(elements) => {
            out.push(11);
            contract_u32(out, elements.len());
            for element in elements {
                encode_contract_type(out, hir, *element)?;
            }
        }
        Type::Fn(params, muts, ret, row) | Type::Callback(params, muts, ret, row) => {
            out.push(if matches!(hir.store.get(ty), Type::Fn(..)) {
                12
            } else {
                13
            });
            contract_u32(out, params.len());
            for (param, mutable) in params.iter().zip(muts) {
                out.push(u8::from(*mutable));
                encode_contract_type(out, hir, *param)?;
            }
            encode_contract_type(out, hir, *ret)?;
            encode_contract_row(out, hir, row);
        }
        Type::Var(index) => {
            out.push(14);
            out.extend_from_slice(&index.to_le_bytes());
        }
        Type::Projection {
            base,
            interface,
            assoc,
        } => {
            out.push(15);
            encode_contract_type(out, hir, *base)?;
            let interface = hir
                .interfaces
                .get(interface.0 as usize)
                .ok_or_else(|| "a slot contract names no interface".to_string())?;
            contract_text(out, &interface.key);
            let associated = interface
                .associated
                .get(*assoc as usize)
                .ok_or_else(|| "a slot contract names no associated type".to_string())?;
            contract_text(out, &associated.name);
        }
        Type::Fault => out.push(16),
        Type::Request => out.push(17),
        Type::PolicyTable => out.push(18),
        Type::Vm => out.push(19),
        Type::Run(result) => {
            out.push(20);
            encode_contract_type(out, hir, *result)?;
        }
        Type::Wait(result) => {
            out.push(21);
            encode_contract_type(out, hir, *result)?;
        }
        Type::PendingCall(args, reply) => {
            out.push(22);
            encode_contract_type(out, hir, *args)?;
            encode_contract_type(out, hir, *reply)?;
        }
        Type::Handle(message, result) => {
            out.push(23);
            encode_contract_type(out, hir, *message)?;
            encode_contract_type(out, hir, *result)?;
        }
        Type::VmSnapshot => out.push(24),
        Type::RunSnapshot(result) => {
            out.push(25);
            encode_contract_type(out, hir, *result)?;
        }
        Type::FileHandle => out.push(26),
        Type::ResourceHandle => out.push(27),
        Type::Op(op, function) => {
            out.push(28);
            out.extend_from_slice(&lm_abi::op_identity(*op));
            encode_contract_type(out, hir, *function)?;
        }
    }
    Ok(())
}

fn encode_contract_use(
    out: &mut Vec<u8>,
    hir: &lm_hir::HirModule,
    application: &HirInterfaceUse,
) -> Result<(), String> {
    let interface = hir
        .interfaces
        .get(application.interface as usize)
        .ok_or_else(|| "a slot contract names no interface".to_string())?;
    contract_text(out, &interface.key);
    contract_u32(out, application.types.len());
    for ty in &application.types {
        encode_contract_type(out, hir, *ty)?;
    }
    contract_u32(out, application.rows.len());
    for row in &application.rows {
        encode_contract_row(out, hir, row);
    }
    Ok(())
}

fn encode_callable_contract(
    out: &mut Vec<u8>,
    hir: &lm_hir::HirModule,
    function: u32,
) -> Result<(), String> {
    let function = hir
        .funcs
        .get(function as usize)
        .ok_or_else(|| "a slot contract names no function".to_string())?;
    out.extend_from_slice(&function.type_params.to_le_bytes());
    out.extend_from_slice(&function.effect_params.to_le_bytes());
    contract_u32(out, function.type_bounds.len());
    for bounds in &function.type_bounds {
        contract_u32(out, bounds.len());
        for bound in bounds {
            encode_contract_use(out, hir, bound)?;
        }
    }
    contract_u32(out, function.params.len());
    for (param, mutable) in function.params.iter().zip(&function.param_muts) {
        out.push(u8::from(*mutable));
        encode_contract_type(out, hir, *param)?;
    }
    encode_contract_type(out, hir, function.ret)?;
    encode_contract_row(out, hir, &function.row);
    Ok(())
}

fn callable_contract_hash(
    hir: &lm_hir::HirModule,
    function: u32,
    kind: IfaceSlotKind,
) -> Result<[u8; 32], String> {
    let mut bytes = b"lm-slot-contract-v1\0".to_vec();
    bytes.push(match kind {
        IfaceSlotKind::Function => 0,
        IfaceSlotKind::Method => 1,
        IfaceSlotKind::Class => return Err("a callable slot names a class".to_string()),
    });
    encode_callable_contract(&mut bytes, hir, function)?;
    Ok(lm_bytecode::hash::sha256(&bytes))
}

fn class_contract_hash(hir: &lm_hir::HirModule, class_index: u32) -> Result<[u8; 32], String> {
    let class = hir
        .classes
        .get(class_index as usize)
        .ok_or_else(|| "a slot contract names no class".to_string())?;
    let mut bytes = b"lm-slot-contract-v1\0".to_vec();
    bytes.push(2);
    out_class_contract(&mut bytes, hir, class_index, class)?;
    Ok(lm_bytecode::hash::sha256(&bytes))
}

fn out_class_contract(
    out: &mut Vec<u8>,
    hir: &lm_hir::HirModule,
    class_index: u32,
    class: &HirClass,
) -> Result<(), String> {
    out.push(u8::from(class.is_final));
    out.push(match class.kind {
        lm_types::ClassKind::Normal => 0,
        lm_types::ClassKind::EnumParent => 1,
        lm_types::ClassKind::EnumCase => 2,
    });
    out.extend_from_slice(&class.type_params.to_le_bytes());
    contract_u32(out, class.type_bounds.len());
    for bounds in &class.type_bounds {
        contract_u32(out, bounds.len());
        for bound in bounds {
            encode_contract_use(out, hir, bound)?;
        }
    }
    match class.parent {
        Some(parent) => {
            out.push(1);
            let parent = hir
                .classes
                .get(parent as usize)
                .ok_or_else(|| "a slot class names no parent".to_string())?;
            contract_text(out, &parent.key);
            contract_u32(out, class.parent_args.len());
            for arg in &class.parent_args {
                encode_contract_type(out, hir, *arg)?;
            }
        }
        None => out.push(0),
    }
    contract_u32(out, class.field_tys.len());
    for ((name, ty), default) in class
        .field_names
        .iter()
        .zip(&class.field_tys)
        .zip(&class.defaults)
    {
        contract_text(out, name);
        encode_contract_type(out, hir, *ty)?;
        out.push(u8::from(default.is_some()));
    }
    contract_u32(out, class.methods.len());
    for (name, function) in &class.methods {
        contract_text(out, name);
        encode_callable_contract(out, hir, *function)?;
    }
    contract_u32(out, class.ctor_params.len());
    for (param, mutable) in class.ctor_params.iter().zip(&class.ctor_param_muts) {
        out.push(u8::from(*mutable));
        encode_contract_type(out, hir, *param)?;
    }
    encode_contract_row(out, hir, &class.ctor_row);
    let conformances: Vec<_> = hir
        .conformances
        .iter()
        .filter(|item| item.class == class_index)
        .collect();
    contract_u32(out, conformances.len());
    for conformance in conformances {
        encode_contract_use(out, hir, &conformance.application)?;
        contract_u32(out, conformance.associated.len());
        for associated in &conformance.associated {
            encode_contract_type(out, hir, *associated)?;
        }
    }
    Ok(())
}

#[derive(Default)]
struct PortableDependencies {
    functions: BTreeSet<u32>,
    classes: BTreeSet<u32>,
}

/// References found while one portable definition body is scanned.
#[derive(Default)]
struct DependencyReferences {
    functions: BTreeSet<u32>,
    classes: BTreeSet<u32>,
    nested_functions: BTreeSet<u32>,
}

/// Find each named local definition reachable through direct code references.
///
/// Anonymous nested bodies join the scan. They do not publish their own slots.
fn portable_dependency_closure(
    hir: &HirModule,
    functions: &BTreeMap<String, (u32, IfaceSlotKind)>,
    classes: &BTreeMap<String, u32>,
) -> PortableDependencies {
    let bindable_functions: BTreeSet<u32> = functions
        .iter()
        .filter_map(|(binding, (function, _))| (!binding.starts_with("core.")).then_some(*function))
        .collect();
    let bindable_classes: BTreeSet<u32> = classes
        .iter()
        .filter_map(|(binding, class)| (!binding.starts_with("core.")).then_some(*class))
        .collect();
    let mut out = PortableDependencies {
        functions: hir.reified_functions.clone(),
        classes: hir.reified_classes.clone(),
    };
    let mut pending_functions = out.functions.clone();
    let mut pending_classes = out.classes.clone();
    let mut scanned_functions = BTreeSet::new();
    let mut scanned_classes = BTreeSet::new();

    while !pending_functions.is_empty() || !pending_classes.is_empty() {
        while let Some(class) = pending_classes.pop_first() {
            if !scanned_classes.insert(class) {
                continue;
            }
            let Some(definition) = hir.classes.get(class as usize) else {
                continue;
            };
            if definition.imported {
                continue;
            }
            pending_functions.extend(definition.methods.iter().map(|(_, function)| *function));
            pending_functions.extend(definition.init);
            let mut references = DependencyReferences::default();
            for value in definition.defaults.iter().flatten() {
                collect_dependency_expr(value, &mut references);
            }
            add_dependency_references(
                references,
                &bindable_functions,
                &bindable_classes,
                &mut out,
                &mut pending_functions,
                &mut pending_classes,
            );
        }

        while let Some(function) = pending_functions.pop_first() {
            if !scanned_functions.insert(function) {
                continue;
            }
            let Some(definition) = hir.funcs.get(function as usize) else {
                continue;
            };
            if definition.imported {
                continue;
            }
            let mut references = DependencyReferences::default();
            collect_dependency_block(&definition.body, &mut references);
            add_dependency_references(
                references,
                &bindable_functions,
                &bindable_classes,
                &mut out,
                &mut pending_functions,
                &mut pending_classes,
            );
        }
    }
    out
}

fn add_dependency_references(
    references: DependencyReferences,
    bindable_functions: &BTreeSet<u32>,
    bindable_classes: &BTreeSet<u32>,
    out: &mut PortableDependencies,
    pending_functions: &mut BTreeSet<u32>,
    pending_classes: &mut BTreeSet<u32>,
) {
    for function in references.functions {
        pending_functions.insert(function);
        if bindable_functions.contains(&function) {
            out.functions.insert(function);
        }
    }
    pending_functions.extend(references.nested_functions);
    for class in references.classes {
        pending_classes.insert(class);
        if bindable_classes.contains(&class) {
            out.classes.insert(class);
        }
    }
}

fn collect_dependency_block(body: &[HStmt], out: &mut DependencyReferences) {
    for statement in body {
        collect_dependency_stmt(statement, out);
    }
}

fn collect_dependency_stmt(statement: &HStmt, out: &mut DependencyReferences) {
    match statement {
        HStmt::Assign { value, .. } => collect_dependency_expr(value, out),
        HStmt::AssignField { recv, value, .. } => {
            collect_dependency_expr(recv, out);
            collect_dependency_expr(value, out);
        }
        HStmt::While { cond, body } => {
            collect_dependency_expr(cond, out);
            collect_dependency_block(body, out);
        }
        HStmt::For {
            source, kind, body, ..
        } => {
            collect_dependency_expr(source, out);
            if let HForKind::Generic { iterator, next, .. } = kind {
                collect_dependency_expr(iterator, out);
                collect_dependency_expr(next, out);
            }
            collect_dependency_block(body, out);
        }
        HStmt::Return { value } => {
            if let Some(value) = value {
                collect_dependency_expr(value, out);
            }
        }
        HStmt::Expr(value) => collect_dependency_expr(value, out),
        HStmt::Break | HStmt::Continue => {}
    }
}

fn collect_dependency_expr(expr: &HExpr, out: &mut DependencyReferences) {
    match &expr.kind {
        HExprKind::Unit
        | HExprKind::Int(_)
        | HExprKind::Str(_)
        | HExprKind::Bool(_)
        | HExprKind::Local(_)
        | HExprKind::Capture(_)
        | HExprKind::OpConst(_) => {}
        HExprKind::Not(value)
        | HExprKind::Neg(value)
        | HExprKind::AsCallback(value)
        | HExprKind::TupleGet { tuple: value, .. }
        | HExprKind::IsType { value, .. }
        | HExprKind::CastType { value, .. }
        | HExprKind::CodeSource { code: value, .. }
        | HExprKind::CallArgs { call: value }
        | HExprKind::FaultCodeGet { fault: value }
        | HExprKind::FaultSiteGet { fault: value }
        | HExprKind::FaultTraceGet { fault: value }
        | HExprKind::RequestOpName { request: value }
        | HExprKind::FaultDenied { reason: value } => collect_dependency_expr(value, out),
        HExprKind::Binary { left, right, .. }
        | HExprKind::And(left, right)
        | HExprKind::Or(left, right) => {
            collect_dependency_expr(left, out);
            collect_dependency_expr(right, out);
        }
        HExprKind::Call { func, args, .. } => {
            out.functions.insert(*func);
            collect_dependency_exprs(args, out);
        }
        HExprKind::Construct { class, args, .. } => {
            out.classes.insert(*class);
            collect_dependency_exprs(args, out);
        }
        HExprKind::MethodCall { recv, args, .. } | HExprKind::InterfaceCall { recv, args, .. } => {
            collect_dependency_expr(recv, out);
            collect_dependency_exprs(args, out);
        }
        HExprKind::FieldGet { recv, .. } => collect_dependency_expr(recv, out),
        HExprKind::Spawn {
            class, body, args, ..
        } => {
            out.classes.insert(*class);
            out.nested_functions.insert(*body);
            collect_dependency_exprs(args, out);
        }
        HExprKind::MakeClosure { func, captures } | HExprKind::MakeCallback { func, captures } => {
            out.nested_functions.insert(*func);
            collect_dependency_exprs(captures, out);
        }
        HExprKind::FunctionCode { func } => {
            out.functions.insert(*func);
        }
        HExprKind::ClassCode { class } => {
            out.classes.insert(*class);
        }
        HExprKind::CallValue { callee, args } => {
            collect_dependency_expr(callee, out);
            collect_dependency_exprs(args, out);
        }
        HExprKind::TupleLit(items) | HExprKind::ListLit(items) => {
            collect_dependency_exprs(items, out);
        }
        HExprKind::MapLit(entries) => {
            for (key, value) in entries {
                collect_dependency_expr(key, out);
                collect_dependency_expr(value, out);
            }
        }
        HExprKind::Native { args, .. }
        | HExprKind::Intrinsic { args, .. }
        | HExprKind::Perform { args, .. } => collect_dependency_exprs(args, out),
        HExprKind::Interp(parts) => {
            for part in parts {
                if let HInterpPart::Expr(value) = part {
                    collect_dependency_expr(value, out);
                }
            }
        }
        HExprKind::If { arms, else_body } => {
            for (condition, body) in arms {
                collect_dependency_expr(condition, out);
                collect_dependency_block(body, out);
            }
            if let Some(body) = else_body {
                collect_dependency_block(body, out);
            }
        }
        HExprKind::Case { scrut, arms, .. } => {
            collect_dependency_expr(scrut, out);
            for arm in arms {
                collect_dependency_block(&arm.body, out);
            }
        }
        HExprKind::TableEdit { table, mock, .. } => {
            collect_dependency_expr(table, out);
            if let Some(mock) = mock {
                collect_dependency_expr(mock, out);
            }
        }
    }
}

fn collect_dependency_exprs(values: &[HExpr], out: &mut DependencyReferences) {
    for value in values {
        collect_dependency_expr(value, out);
    }
}

pub(crate) fn select_linkage(
    path: &str,
    hir: &lm_hir::HirModule,
    env: &FrozenCompileEnv,
    options: &CompileOptions,
) -> Result<(LowerLinkage, Vec<IfaceSlotSpec>), String> {
    let mut functions: BTreeMap<String, (u32, IfaceSlotKind)> = BTreeMap::new();
    let method_functions: BTreeSet<u32> = hir
        .classes
        .iter()
        .flat_map(|class| class.methods.iter().map(|(_, function)| *function))
        .collect();
    for binding in &hir.bindings {
        let kind = if method_functions.contains(&binding.func) {
            IfaceSlotKind::Method
        } else {
            IfaceSlotKind::Function
        };
        functions.insert(binding.key.clone(), (binding.func, kind));
    }
    for import in &hir.imports {
        let lm_hir::HirImportDef::Func(function) = import.def else {
            continue;
        };
        let kind = if import.kind == lm_bytecode::ImportKind::Method {
            IfaceSlotKind::Method
        } else {
            IfaceSlotKind::Function
        };
        let binding = lm_bytecode::qualified_key(&import.module, &import.name);
        functions.insert(binding, (function, kind));
    }
    let classes: BTreeMap<String, u32> = hir
        .classes
        .iter()
        .enumerate()
        .map(|(index, class)| (class.key.clone(), index as u32))
        .collect();
    let portable = portable_dependency_closure(hir, &functions, &classes);

    let mut selected: BTreeMap<String, IfaceSlotSpec> = BTreeMap::new();
    let entry_binding = lm_bytecode::qualified_key(path, "<entry>");
    selected.insert(
        entry_binding.clone(),
        IfaceSlotSpec {
            binding: entry_binding.clone(),
            contract_hash: [0; 32],
            key: [0; 32],
            kind: IfaceSlotKind::Function,
            late: false,
        },
    );
    functions.insert(
        entry_binding.clone(),
        (hir.entry as u32, IfaceSlotKind::Function),
    );

    for export in &hir.exports {
        match export.kind {
            lm_bytecode::ExportKind::Function => {
                let binding = lm_bytecode::qualified_key(path, &export.name);
                selected.entry(binding.clone()).or_insert(IfaceSlotSpec {
                    binding,
                    contract_hash: [0; 32],
                    key: [0; 32],
                    kind: IfaceSlotKind::Function,
                    late: false,
                });
            }
            kind if kind.is_class()
                && hir.classes[export.def as usize].native_repr.is_none()
                && hir.classes[export.def as usize].kind != lm_types::ClassKind::EnumParent =>
            {
                let binding = hir.classes[export.def as usize].key.clone();
                selected.entry(binding.clone()).or_insert(IfaceSlotSpec {
                    binding,
                    contract_hash: [0; 32],
                    key: [0; 32],
                    kind: IfaceSlotKind::Class,
                    late: false,
                });
            }
            _ => {}
        }
    }

    for spec in env.late_bindings().values() {
        if functions.contains_key(&spec.binding) || classes.contains_key(&spec.binding) {
            selected
                .entry(spec.binding.clone())
                .and_modify(|selected| {
                    selected.contract_hash = spec.contract_hash;
                    selected.key = spec.key;
                    selected.kind = spec.kind;
                    selected.late = true;
                })
                .or_insert_with(|| spec.clone());
        }
    }
    for function in &portable.functions {
        if let Some((binding, (_, kind))) = functions
            .iter()
            .find(|(_, (candidate, _))| candidate == function)
        {
            let mut published = env
                .published_binding(binding)
                .cloned()
                .unwrap_or(IfaceSlotSpec {
                    binding: binding.clone(),
                    contract_hash: [0; 32],
                    key: [0; 32],
                    kind: *kind,
                    late: true,
                });
            published.late = true;
            selected
                .entry(binding.clone())
                .and_modify(|spec| spec.late = true)
                .or_insert(published);
        }
    }
    for class in &portable.classes {
        if let Some((binding, _)) = classes.iter().find(|(_, candidate)| candidate == &class) {
            let definition = &hir.classes[*class as usize];
            if definition.native_repr.is_none()
                && definition.kind != lm_types::ClassKind::EnumParent
            {
                let mut published =
                    env.published_binding(binding)
                        .cloned()
                        .unwrap_or(IfaceSlotSpec {
                            binding: binding.clone(),
                            contract_hash: [0; 32],
                            key: [0; 32],
                            kind: IfaceSlotKind::Class,
                            late: true,
                        });
                published.late = true;
                selected
                    .entry(binding.clone())
                    .and_modify(|spec| spec.late = true)
                    .or_insert(published);
            }
        }
    }
    for name in &options.late_functions {
        let qualified = lm_bytecode::qualified_key(path, name);
        let binding = if functions.contains_key(name) {
            name.clone()
        } else {
            qualified
        };
        if !functions.contains_key(&binding) {
            return Err(format!("error: no function binding named `{binding}`\n"));
        }
        let kind = functions[&binding].1;
        let mut published = env
            .published_binding(&binding)
            .cloned()
            .unwrap_or(IfaceSlotSpec {
                binding: binding.clone(),
                contract_hash: [0; 32],
                key: [0; 32],
                kind,
                late: true,
            });
        published.late = true;
        selected
            .entry(binding.clone())
            .and_modify(|spec| spec.late = true)
            .or_insert(published);
    }
    for name in &options.late_classes {
        let qualified = lm_bytecode::qualified_key(path, name);
        let binding = if classes.contains_key(name) {
            name.clone()
        } else {
            qualified
        };
        if !classes.contains_key(&binding) {
            return Err(format!("error: no class binding named `{binding}`\n"));
        }
        let mut published = env
            .published_binding(&binding)
            .cloned()
            .unwrap_or(IfaceSlotSpec {
                binding: binding.clone(),
                contract_hash: [0; 32],
                key: [0; 32],
                kind: IfaceSlotKind::Class,
                late: true,
            });
        published.late = true;
        selected
            .entry(binding.clone())
            .and_modify(|spec| spec.late = true)
            .or_insert(published);
    }
    if options.late_definitions {
        for (binding, (function, kind)) in &functions {
            let local = !hir.funcs[*function as usize].imported && !binding.starts_with("core.");
            if local {
                selected
                    .entry(binding.clone())
                    .and_modify(|spec| spec.late = true)
                    .or_insert(IfaceSlotSpec {
                        binding: binding.clone(),
                        contract_hash: [0; 32],
                        key: [0; 32],
                        kind: *kind,
                        late: true,
                    });
            }
        }
        for (binding, class) in &classes {
            let definition = &hir.classes[*class as usize];
            let local = !definition.imported
                && definition.native_repr.is_none()
                && definition.kind != lm_types::ClassKind::EnumParent
                && !binding.starts_with("core.");
            if local {
                selected
                    .entry(binding.clone())
                    .and_modify(|spec| spec.late = true)
                    .or_insert(IfaceSlotSpec {
                        binding: binding.clone(),
                        contract_hash: [0; 32],
                        key: [0; 32],
                        kind: IfaceSlotKind::Class,
                        late: true,
                    });
            }
        }
    }

    let mut linkage = LowerLinkage::default();
    let mut published = Vec::new();
    let mut used_keys = BTreeMap::new();
    for (binding, mut spec) in selected {
        let local_contract = match spec.kind {
            IfaceSlotKind::Function | IfaceSlotKind::Method => {
                let Some((function, found_kind)) = functions.get(&binding).copied() else {
                    return Err(format!("error: no callable binding named `{binding}`\n"));
                };
                if found_kind != spec.kind {
                    return Err(format!("error: `{binding}` has another late target kind\n"));
                }
                (!hir.funcs[function as usize].imported)
                    .then(|| callable_contract_hash(hir, function, spec.kind))
                    .transpose()?
            }
            IfaceSlotKind::Class => {
                let Some(class) = classes.get(&binding).copied() else {
                    return Err(format!("error: no class binding named `{binding}`\n"));
                };
                (!hir.classes[class as usize].imported)
                    .then(|| class_contract_hash(hir, class))
                    .transpose()?
            }
        };
        if let Some(mut contract_hash) = local_contract {
            if binding == entry_binding && options.dynamic_result {
                contract_hash = lm_bytecode::hash::sha256(b"lm-dynamic-entry-contract-v1\0");
            }
            spec.contract_hash = contract_hash;
            spec.key = lm_bytecode::slot_key(&binding, &contract_hash);
        }
        if let Some(old) = used_keys.insert(spec.key, binding.clone()) {
            return Err(format!(
                "error: `{old}` and `{binding}` use the same late slot key\n"
            ));
        }
        match spec.kind {
            IfaceSlotKind::Function | IfaceSlotKind::Method => {
                let Some((function, found_kind)) = functions.get(&binding).copied() else {
                    return Err(format!("error: no callable binding named `{binding}`\n"));
                };
                if found_kind != spec.kind {
                    return Err(format!("error: `{binding}` has another late target kind\n"));
                }
                linkage.functions.insert(
                    function,
                    LateCallable {
                        key: spec.key,
                        kind: if spec.kind == IfaceSlotKind::Method {
                            LateCallableKind::Method
                        } else {
                            LateCallableKind::Function
                        },
                    },
                );
                if spec.late {
                    linkage.dynamic_functions.insert(function);
                }
                if !hir.funcs[function as usize].imported && binding != entry_binding {
                    published.push(spec);
                }
            }
            IfaceSlotKind::Class => {
                let Some(class) = classes.get(&binding).copied() else {
                    return Err(format!("error: no class binding named `{binding}`\n"));
                };
                linkage.classes.insert(
                    class,
                    lm_hir::LateClass {
                        key: spec.key,
                        abi: spec.contract_hash,
                    },
                );
                if spec.late {
                    linkage.dynamic_classes.insert(class);
                }
                if !hir.classes[class as usize].imported {
                    published.push(spec);
                }
            }
        }
    }
    published.sort();
    Ok((linkage, published))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lm_bytecode::{ExtendedInstr, Instr, SlotContract};

    fn compile(source: &str, options: &CompileOptions) -> CompiledModule {
        compile_module_with_options(
            "probe",
            &SourceFile::new("probe.lm", source),
            &crate::CompileEnv::new().freeze(),
            true,
            options,
        )
        .expect("the probe compiles")
    }

    fn instructions(module: &Module) -> impl Iterator<Item = &Instr> {
        module
            .funcs
            .iter()
            .flat_map(|function| &function.blocks)
            .flatten()
    }

    #[test]
    fn default_options_keep_static_artifacts_identical() {
        let source = SourceFile::new(
            "probe.lm",
            "def add1(n: Int): Int\n  n + 1\nend\nadd1(41)\n",
        );
        let env = crate::CompileEnv::new().freeze();
        let plain = compile_module("probe", &source, &env, true).expect("the probe compiles");
        let explicit =
            compile_module_with_options("probe", &source, &env, true, &CompileOptions::new())
                .expect("the probe compiles");
        assert_eq!(plain.artifact, explicit.artifact);
        assert_eq!(plain.interface_bytes, explicit.interface_bytes);
        assert!(!instructions(&plain.module).any(|instruction| matches!(
            instruction,
            Instr::Extended(ExtendedInstr::CallSlot { .. })
        )));
        assert!(plain.interface.slots.iter().all(|slot| !slot.late));
    }

    #[test]
    fn a_dynamic_result_changes_only_the_main_entry_contract() {
        let compiled = compile("[1, 2, 3]\n", &CompileOptions::new().dynamic_result());
        let entry = &compiled.module.funcs[compiled.module.entry as usize];
        let class = compiled.module.core_roles[lm_bytecode::corepin::ROLE_DYN_VALUE];
        assert_eq!(
            compiled.module.types[entry.ret as usize],
            BcType::Class(class)
        );
        assert!(entry.blocks.iter().flatten().any(|instruction| matches!(
            instruction,
            Instr::Extended(ExtendedInstr::DynPack { .. })
        )));
        assert_eq!(
            instructions(&compiled.module)
                .filter(|instruction| matches!(
                    instruction,
                    Instr::Extended(ExtendedInstr::DynPack { .. })
                ))
                .count(),
            1
        );
    }

    #[test]
    fn a_library_rejects_a_dynamic_result() {
        let error = compile_module_with_options(
            "library",
            &SourceFile::new("library.lm", "def value(): Int\n  1\nend\n"),
            &crate::CompileEnv::new().freeze(),
            false,
            &CompileOptions::new().dynamic_result(),
        )
        .expect_err("a library cannot package an entry result");
        assert!(error.contains("dynamic result needs a main module"));
    }

    #[test]
    fn a_late_function_uses_call_slot_and_blocks_inlining() {
        let compiled = compile(
            "def add1(n: Int): Int\n  n + 1\nend\nadd1(41)\n",
            &CompileOptions::new().late_function("add1"),
        );
        let published = &compiled.interface.slots[0];
        let slot = compiled
            .module
            .slots
            .iter()
            .position(|slot| slot.key == published.key)
            .expect("the published slot exists");
        assert!(matches!(
            compiled.module.slots[slot].contract,
            SlotContract::Function(_)
        ));
        assert!(instructions(&compiled.module).any(|instruction| matches!(
            instruction,
            Instr::Extended(ExtendedInstr::CallSlot { slot: found, .. }) if *found as usize == slot
        )));
        assert_eq!(compiled.interface.slots.len(), 1);
        assert!(compiled.interface.slots[0].late);
        let decoded = lm_bytecode::interface::decode_interface(&compiled.interface_bytes)
            .expect("the interface decodes");
        assert_eq!(decoded, compiled.interface);
    }

    #[test]
    fn a_late_function_body_does_not_change_its_slot_key() {
        let first = compile(
            "def step(value: Int): Int\n  value + 1\nend\n0\n",
            &CompileOptions::new().late_function("step"),
        );
        let second = compile(
            "def step(value: Int): Int\n  value + 10\nend\n0\n",
            &CompileOptions::new().late_function("step"),
        );
        assert_eq!(first.interface.slots[0].key, second.interface.slots[0].key);
        assert_eq!(
            first.interface.slots[0].contract_hash,
            second.interface.slots[0].contract_hash
        );
    }

    #[test]
    fn a_late_function_contract_changes_its_slot_key() {
        let first = compile(
            "def step(value: Int): Int\n  value + 1\nend\n0\n",
            &CompileOptions::new().late_function("step"),
        );
        let second = compile(
            "def step(value: String): String\n  value\nend\n0\n",
            &CompileOptions::new().late_function("step"),
        );
        assert_ne!(first.interface.slots[0].key, second.interface.slots[0].key);
        assert_ne!(
            first.interface.slots[0].contract_hash,
            second.interface.slots[0].contract_hash
        );
    }

    #[test]
    fn a_late_class_allocates_through_new_slot() {
        let compiled = compile(
            "final class Box\n  value: Int\n  def init(mut self, value: Int)\n    self.value = value\n  end\nend\nBox(42).value\n",
            &CompileOptions::new().late_class("Box"),
        );
        let published = &compiled.interface.slots[0];
        let slot = compiled
            .module
            .slots
            .iter()
            .position(|slot| slot.key == published.key)
            .expect("the published slot exists");
        assert!(matches!(
            compiled.module.slots[slot].contract,
            SlotContract::Class { .. }
        ));
        assert!(instructions(&compiled.module).any(|instruction| matches!(
            instruction,
            Instr::Extended(ExtendedInstr::NewSlot { slot: found, .. }) if *found as usize == slot
        )));
    }

    #[test]
    fn a_late_class_default_changes_only_its_constructor_version() {
        let first = compile(
            "final class Box\n  value: Int = 5\nend\nBox().value\n",
            &CompileOptions::new().late_class("Box"),
        );
        let second = compile(
            "final class Box\n  value: Int = 50\nend\nBox().value\n",
            &CompileOptions::new().late_class("Box"),
        );
        assert_eq!(first.interface.slots[0].key, second.interface.slots[0].key);
        assert_eq!(
            first.interface.slots[0].contract_hash,
            second.interface.slots[0].contract_hash
        );
        let first_identity = lm_bytecode::identity::module_identity(&first.module)
            .expect("the first revision has an identity");
        let second_identity = lm_bytecode::identity::module_identity(&second.module)
            .expect("the second revision has an identity");
        assert_eq!(first_identity.class_hashes, second_identity.class_hashes);
        let first_slot = first
            .module
            .slots
            .iter()
            .find(|slot| slot.key == first.interface.slots[0].key)
            .expect("the first class slot exists");
        let second_slot = second
            .module
            .slots
            .iter()
            .find(|slot| slot.key == second.interface.slots[0].key)
            .expect("the second class slot exists");
        let (first_class, first_constructor) = match first_slot.initial {
            Some(lm_bytecode::SlotTarget::Class { class, constructor }) => (class, constructor),
            _ => panic!("the first class slot has no constructor"),
        };
        let second_constructor = match second_slot.initial {
            Some(lm_bytecode::SlotTarget::Class { constructor, .. }) => constructor,
            _ => panic!("the second class slot has no constructor"),
        };
        assert_ne!(
            first_identity.func_hashes[first_constructor as usize],
            second_identity.func_hashes[second_constructor as usize]
        );
        assert!(first.module.funcs[first_constructor as usize]
            .blocks
            .iter()
            .flatten()
            .any(|instruction| {
                matches!(instruction, Instr::New(class) if *class == first_class)
            }));
    }

    #[test]
    fn imported_late_linkage_relocates_through_the_linker() {
        let library = compile_module_with_options(
            "lib.math",
            &SourceFile::new("lib/math.lm", "def twice(n: Int): Int\n  n * 2\nend\n"),
            &crate::CompileEnv::new().freeze(),
            false,
            &CompileOptions::new().late_function("twice"),
        )
        .expect("the library compiles");
        let mut compile_env = crate::CompileEnv::new();
        compile_env
            .bind_interface(library.interface.clone())
            .expect("the interface binds");
        compile_env.bind_root("lib", "lib").expect("the root binds");
        let program = compile_module(
            "app.main",
            &SourceFile::new(
                "app/main.lm",
                "use lib.math\ndef run(): Int\n  math.twice(21)\nend\nrun()\n",
            ),
            &compile_env.freeze(),
            true,
        )
        .expect("the program compiles");
        assert!(instructions(&program.module).any(|instruction| matches!(
            instruction,
            Instr::Extended(ExtendedInstr::CallSlot { .. })
        )));

        let mut link_env = crate::LinkEnv::new();
        for unit in [&library, &program] {
            link_env
                .bind(crate::LinkUnit {
                    path: unit.path.clone(),
                    module: unit.module.clone(),
                    interface: unit.interface.clone(),
                })
                .expect("the unit binds");
        }
        let linked = crate::link("app.main", &link_env.freeze()).expect("the program links");
        let key = library
            .interface
            .slots
            .iter()
            .find(|slot| slot.binding.ends_with(".twice"))
            .expect("the library publishes twice")
            .key;
        assert!(linked.module.slots.iter().any(|slot| slot.key == key));
    }

    #[test]
    fn imported_reification_uses_the_published_static_contract() {
        let library = compile_module(
            "lib.math",
            &SourceFile::new("lib/math.lm", "def twice(n: Int): Int\n  n * 2\nend\n"),
            &crate::CompileEnv::new().freeze(),
            false,
        )
        .expect("the library compiles");
        let published = library
            .interface
            .slots
            .iter()
            .find(|slot| slot.binding == "lib.math.twice")
            .expect("the library publishes twice");
        assert!(!published.late);

        let mut compile_env = crate::CompileEnv::new();
        compile_env
            .bind_interface(library.interface.clone())
            .expect("the interface binds");
        compile_env.bind_root("lib", "lib").expect("the root binds");
        let program = compile_module(
            "app.main",
            &SourceFile::new(
                "app/main.lm",
                "use lib.math.twice\ndef portable(): FunctionCode[(Int,), Int]\n  codeof(twice)\nend\ndef run(): Int\n  twice(21)\nend\nrun()\n",
            ),
            &compile_env.freeze(),
            true,
        )
        .expect("the program compiles");
        assert!(instructions(&program.module).any(|instruction| matches!(
            instruction,
            Instr::Extended(ExtendedInstr::CallSlot { slot, .. })
                if program.module.slots[*slot as usize].key == published.key
        )));
    }

    #[test]
    fn reified_class_dependencies_use_published_slots() {
        let compiled = compile(
            "def rate(value: Int): Int\n  value * 2\nend\ndef quote(value: Int): Int\n  rate(value)\nend\ndef unused(value: Int): Int\n  value + 1\nend\nfinal class Worker\n  def price(self, value: Int): Int\n    quote(value)\n  end\nend\ncodeof(Worker)\n",
            &CompileOptions::new(),
        );
        let rate = compiled
            .interface
            .slots
            .iter()
            .find(|slot| slot.binding == "probe.rate")
            .expect("the dependency has a published slot");
        let quote = compiled
            .interface
            .slots
            .iter()
            .find(|slot| slot.binding == "probe.quote")
            .expect("the direct dependency has a published slot");
        let unused = compiled
            .interface
            .slots
            .iter()
            .find(|slot| slot.binding == "probe.unused")
            .expect("the unrelated function stays published");
        let worker = compiled
            .interface
            .slots
            .iter()
            .find(|slot| slot.binding == "probe.Worker")
            .expect("the class has a published slot");
        assert!(rate.late);
        assert!(quote.late);
        assert!(worker.late);
        assert!(!unused.late);
        assert!(instructions(&compiled.module).any(|instruction| matches!(
            instruction,
            Instr::Extended(ExtendedInstr::CallSlot { slot, .. })
                if compiled.module.slots[*slot as usize].key == rate.key
        )));
        assert!(instructions(&compiled.module).any(|instruction| matches!(
            instruction,
            Instr::Extended(ExtendedInstr::CallSlot { slot, .. })
                if compiled.module.slots[*slot as usize].key == quote.key
        )));
    }

    #[test]
    fn runtime_append_keeps_every_existing_index() {
        let base = compile(
            "def base(): Int\n  1\nend\nbase()\n",
            &CompileOptions::new(),
        );
        let addition = compile_module_with_options(
            "revision",
            &SourceFile::new("revision.lm", "def added(): Int\n  2\nend\nadded()\n"),
            &crate::CompileEnv::new().freeze(),
            true,
            &CompileOptions::new().late_function("added"),
        )
        .expect("the addition compiles");
        let appended = lm_bytecode::append::append_linked(&base.module, &addition.module)
            .expect("the module appends");
        assert_eq!(
            &appended.module.strings[..base.module.strings.len()],
            &base.module.strings
        );
        assert_eq!(
            &appended.module.types[..base.module.types.len()],
            &base.module.types
        );
        assert_eq!(
            &appended.module.classes[..base.module.classes.len()],
            &base.module.classes
        );
        assert_eq!(
            &appended.module.funcs[..base.module.funcs.len()],
            &base.module.funcs
        );
        lm_verify::verify_module(&appended.module).expect("the appended module verifies");
    }
}
