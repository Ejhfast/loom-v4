//! Shared bytecode table relocation.

use crate::env::{fail, LinkError};
use lm_bytecode::{
    BcAssociated, BcCallableContract, BcClass, BcConformance, BcInterface, BcInterfaceMethod,
    BcInterfaceUse, BcRow, BcType, CodeTables, ExtendedInstr, Func, Instr, SlotContract, SlotSpec,
    SlotTarget, NO_PARENT,
};

/// One module's table relocation maps.
#[derive(Debug, Clone)]
pub(crate) struct Reloc {
    pub(crate) strings: Vec<u32>,
    pub(crate) bytes: Vec<u32>,
    pub(crate) types: Vec<u32>,
    pub(crate) selectors: Vec<u32>,
    pub(crate) apps: Vec<u32>,
    pub(crate) classes: Vec<u32>,
    pub(crate) interfaces: Vec<u32>,
    pub(crate) funcs: Vec<u32>,
    pub(crate) slots: Vec<u32>,
}

pub(crate) fn reloc_row(row: &[BcRow]) -> Vec<BcRow> {
    row.to_vec()
}

pub(crate) fn reloc_type(
    ty: &BcType,
    types: &[u32],
    classes: &[u32],
    interfaces: &[u32],
) -> BcType {
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
            reloc_row(row),
        ),
        BcType::Callback(params, muts, ret, row) => BcType::Callback(
            params.iter().map(|p| types[*p as usize]).collect(),
            muts.clone(),
            types[*ret as usize],
            reloc_row(row),
        ),
        BcType::Projection {
            base,
            interface,
            assoc,
        } => BcType::Projection {
            base: types[*base as usize],
            interface: interfaces[*interface as usize],
            assoc: *assoc,
        },
        BcType::Run(t) => BcType::Run(types[*t as usize]),
        BcType::Wait(t) => BcType::Wait(types[*t as usize]),
        BcType::RunSnapshot(t) => BcType::RunSnapshot(types[*t as usize]),
        BcType::PendingCall(a, r) => BcType::PendingCall(types[*a as usize], types[*r as usize]),
        BcType::Handle(m, r) => BcType::Handle(types[*m as usize], types[*r as usize]),
        BcType::Op(op, f) => BcType::Op(*op, types[*f as usize]),
        other => other.clone(),
    }
}

pub(crate) fn reloc_interface_use(application: &BcInterfaceUse, reloc: &Reloc) -> BcInterfaceUse {
    BcInterfaceUse {
        interface: reloc.interfaces[application.interface as usize],
        types: application
            .types
            .iter()
            .map(|item| reloc.types[*item as usize])
            .collect(),
        rows: application.rows.iter().map(|row| reloc_row(row)).collect(),
    }
}

pub(crate) fn reloc_bounds(
    bounds: &[Vec<BcInterfaceUse>],
    reloc: &Reloc,
) -> Vec<Vec<BcInterfaceUse>> {
    bounds
        .iter()
        .map(|items| {
            items
                .iter()
                .map(|item| reloc_interface_use(item, reloc))
                .collect()
        })
        .collect()
}

pub(crate) fn reloc_callable_contract(
    source: &BcCallableContract,
    reloc: &Reloc,
) -> BcCallableContract {
    BcCallableContract {
        type_params: source.type_params,
        effect_params: source.effect_params,
        type_bounds: reloc_bounds(&source.type_bounds, reloc),
        params: source
            .params
            .iter()
            .map(|ty| reloc.types[*ty as usize])
            .collect(),
        param_muts: source.param_muts.clone(),
        ret: reloc.types[source.ret as usize],
        row: reloc_row(&source.row),
    }
}

pub(crate) fn reloc_slot_contract(source: &SlotContract, reloc: &Reloc) -> SlotContract {
    match source {
        SlotContract::Function(contract) => {
            SlotContract::Function(reloc_callable_contract(contract, reloc))
        }
        SlotContract::Method(contract) => {
            SlotContract::Method(reloc_callable_contract(contract, reloc))
        }
        SlotContract::Class {
            type_params,
            abi,
            ty,
            constructor,
        } => SlotContract::Class {
            type_params: *type_params,
            abi: *abi,
            ty: reloc.types[*ty as usize],
            constructor: reloc_callable_contract(constructor, reloc),
        },
        SlotContract::Value { ty } => SlotContract::Value {
            ty: reloc.types[*ty as usize],
        },
        SlotContract::Process { message, result } => SlotContract::Process {
            message: reloc.types[*message as usize],
            result: reloc.types[*result as usize],
        },
    }
}

pub(crate) fn reloc_slot_target(source: SlotTarget, reloc: &Reloc) -> SlotTarget {
    match source {
        SlotTarget::Function(func) => SlotTarget::Function(reloc.funcs[func as usize]),
        SlotTarget::Class { class, constructor } => SlotTarget::Class {
            class: reloc.classes[class as usize],
            constructor: reloc.funcs[constructor as usize],
        },
    }
}

pub(crate) fn reloc_interface(source: &BcInterface, reloc: &Reloc) -> BcInterface {
    BcInterface {
        name: source.name.clone(),
        key: source.key.clone(),
        type_params: source.type_params,
        effect_params: source.effect_params,
        generic_is_effect: source.generic_is_effect.clone(),
        parents: source
            .parents
            .iter()
            .map(|parent| reloc_interface_use(parent, reloc))
            .collect(),
        type_bounds: reloc_bounds(&source.type_bounds, reloc),
        associated: source
            .associated
            .iter()
            .map(|item| BcAssociated {
                name: item.name.clone(),
                bounds: item
                    .bounds
                    .iter()
                    .map(|bound| reloc_interface_use(bound, reloc))
                    .collect(),
            })
            .collect(),
        methods: source
            .methods
            .iter()
            .map(|method| BcInterfaceMethod {
                selector: reloc.selectors[method.selector as usize],
                mut_self: method.mut_self,
                type_params: method.type_params,
                type_bounds: reloc_bounds(&method.type_bounds, reloc),
                effect_params: method.effect_params,
                premises: method
                    .premises
                    .iter()
                    .map(|premise| lm_bytecode::BcTypePremise {
                        subject: reloc.types[premise.subject as usize],
                        bounds: premise
                            .bounds
                            .iter()
                            .map(|bound| reloc_interface_use(bound, reloc))
                            .collect(),
                    })
                    .collect(),
                params: method
                    .params
                    .iter()
                    .map(|item| reloc.types[*item as usize])
                    .collect(),
                param_muts: method.param_muts.clone(),
                param_names: method.param_names.clone(),
                ret: reloc.types[method.ret as usize],
                row: reloc_row(&method.row),
                default: if method.default == lm_bytecode::NO_FUNC {
                    lm_bytecode::NO_FUNC
                } else {
                    reloc.funcs[method.default as usize]
                },
            })
            .collect(),
    }
}

#[derive(Debug, Clone)]
pub struct UnitRelocation(pub(crate) Reloc);

impl UnitRelocation {
    pub fn strings(&self) -> &[u32] {
        &self.0.strings
    }

    pub fn bytes(&self) -> &[u32] {
        &self.0.bytes
    }

    pub fn types(&self) -> &[u32] {
        &self.0.types
    }

    pub fn selectors(&self) -> &[u32] {
        &self.0.selectors
    }

    pub fn applications(&self) -> &[u32] {
        &self.0.apps
    }

    pub fn classes(&self) -> &[u32] {
        &self.0.classes
    }

    pub fn interfaces(&self) -> &[u32] {
        &self.0.interfaces
    }

    pub fn functions(&self) -> &[u32] {
        &self.0.funcs
    }

    pub fn slots(&self) -> &[u32] {
        &self.0.slots
    }
}

/// Exact dense-index maps between two publications of one graph.
#[derive(Debug, Clone)]
pub struct CodeRelocation {
    identity: bool,
    strings: Vec<Option<u32>>,
    bytes: Vec<Option<u32>>,
    types: Vec<Option<u32>>,
    selectors: Vec<Option<u32>>,
    applications: Vec<Option<u32>>,
    classes: Vec<Option<u32>>,
    interfaces: Vec<Option<u32>>,
    functions: Vec<Option<u32>>,
    slots: Vec<Option<u32>>,
}

impl CodeRelocation {
    pub(crate) fn with_source(source: &CodeTables) -> CodeRelocation {
        CodeRelocation {
            identity: false,
            strings: vec![None; source.strings.len()],
            bytes: vec![None; source.bytes.len()],
            types: vec![None; source.types.len()],
            selectors: vec![None; source.selectors.len()],
            applications: vec![None; source.apps.len()],
            classes: vec![None; source.classes.len()],
            interfaces: vec![None; source.interfaces.len()],
            functions: vec![None; source.funcs.len()],
            slots: vec![None; source.slots.len()],
        }
    }

    /// Build the identity map for one shared arena.
    pub fn identity() -> CodeRelocation {
        CodeRelocation {
            identity: true,
            strings: Vec::new(),
            bytes: Vec::new(),
            types: Vec::new(),
            selectors: Vec::new(),
            applications: Vec::new(),
            classes: Vec::new(),
            interfaces: Vec::new(),
            functions: Vec::new(),
            slots: Vec::new(),
        }
    }

    /// True when every source index is also its target index.
    #[inline]
    pub fn is_identity(&self) -> bool {
        self.identity
    }

    pub(crate) fn merge_unit(
        &mut self,
        source: &UnitRelocation,
        target: &UnitRelocation,
    ) -> Result<(), LinkError> {
        merge_index_map(
            &mut self.strings,
            source.strings(),
            target.strings(),
            "string",
        )?;
        merge_index_map(
            &mut self.bytes,
            source.bytes(),
            target.bytes(),
            "byte literal",
        )?;
        merge_index_map(&mut self.types, source.types(), target.types(), "type")?;
        merge_index_map(
            &mut self.selectors,
            source.selectors(),
            target.selectors(),
            "selector",
        )?;
        merge_index_map(
            &mut self.applications,
            source.applications(),
            target.applications(),
            "type application",
        )?;
        merge_index_map(
            &mut self.classes,
            source.classes(),
            target.classes(),
            "class",
        )?;
        merge_index_map(
            &mut self.interfaces,
            source.interfaces(),
            target.interfaces(),
            "interface",
        )?;
        merge_index_map(
            &mut self.functions,
            source.functions(),
            target.functions(),
            "function",
        )?;
        merge_index_map(&mut self.slots, source.slots(), target.slots(), "slot")?;
        Ok(())
    }

    /// Add every resolved index from another compatible map.
    pub fn merge(&mut self, other: &CodeRelocation) -> Result<(), LinkError> {
        if self.identity || other.identity {
            if self.identity && other.identity {
                return Ok(());
            }
            return Err(fail("an identity relocation cannot merge with another map"));
        }
        merge_optional_map(&mut self.strings, &other.strings, "string")?;
        merge_optional_map(&mut self.bytes, &other.bytes, "byte literal")?;
        merge_optional_map(&mut self.types, &other.types, "type")?;
        merge_optional_map(&mut self.selectors, &other.selectors, "selector")?;
        merge_optional_map(
            &mut self.applications,
            &other.applications,
            "type application",
        )?;
        merge_optional_map(&mut self.classes, &other.classes, "class")?;
        merge_optional_map(&mut self.interfaces, &other.interfaces, "interface")?;
        merge_optional_map(&mut self.functions, &other.functions, "function")?;
        merge_optional_map(&mut self.slots, &other.slots, "slot")?;
        Ok(())
    }

    #[inline]
    pub fn string(&self, source: u32) -> Option<u32> {
        if self.identity {
            return Some(source);
        }
        map_index(&self.strings, source)
    }

    #[inline]
    pub fn bytes(&self, source: u32) -> Option<u32> {
        if self.identity {
            return Some(source);
        }
        map_index(&self.bytes, source)
    }

    #[inline]
    pub fn ty(&self, source: u32) -> Option<u32> {
        if self.identity {
            return Some(source);
        }
        map_index(&self.types, source)
    }

    #[inline]
    pub fn selector(&self, source: u32) -> Option<u32> {
        if self.identity {
            return Some(source);
        }
        map_index(&self.selectors, source)
    }

    #[inline]
    pub fn application(&self, source: u32) -> Option<u32> {
        if self.identity {
            return Some(source);
        }
        map_index(&self.applications, source)
    }

    #[inline]
    pub fn class(&self, source: u32) -> Option<u32> {
        if self.identity {
            return Some(source);
        }
        map_index(&self.classes, source)
    }

    #[inline]
    pub fn interface(&self, source: u32) -> Option<u32> {
        if self.identity {
            return Some(source);
        }
        map_index(&self.interfaces, source)
    }

    #[inline]
    pub fn function(&self, source: u32) -> Option<u32> {
        if self.identity {
            return Some(source);
        }
        map_index(&self.functions, source)
    }

    #[inline]
    pub fn slot(&self, source: u32) -> Option<u32> {
        if self.identity {
            return Some(source);
        }
        map_index(&self.slots, source)
    }
}

fn merge_optional_map(
    target: &mut Vec<Option<u32>>,
    source: &[Option<u32>],
    what: &str,
) -> Result<(), LinkError> {
    target.resize(target.len().max(source.len()), None);
    for (index, value) in source.iter().copied().enumerate() {
        let Some(value) = value else {
            continue;
        };
        match target[index] {
            Some(current) if current != value => {
                return Err(fail(format!(
                    "the {what} index {index} has conflicting relocation targets"
                )));
            }
            Some(_) => {}
            None => target[index] = Some(value),
        }
    }
    Ok(())
}

fn map_index(map: &[Option<u32>], source: u32) -> Option<u32> {
    map.get(source as usize).copied().flatten()
}

fn merge_index_map(
    map: &mut [Option<u32>],
    source: &[u32],
    target: &[u32],
    kind: &str,
) -> Result<(), LinkError> {
    if source.len() != target.len() {
        return Err(fail(format!("the {kind} relocation has another length")));
    }
    for (source, target) in source.iter().copied().zip(target.iter().copied()) {
        let entry = map
            .get_mut(source as usize)
            .ok_or_else(|| fail(format!("the source {kind} index is outside its tables")))?;
        match *entry {
            Some(existing) if existing != target => {
                return Err(fail(format!("the shared {kind} has two target indices")))
            }
            Some(_) => {}
            None => *entry = Some(target),
        }
    }
    Ok(())
}

pub(crate) fn reloc_conformance(source: &BcConformance, reloc: &Reloc) -> BcConformance {
    BcConformance {
        class: reloc.classes[source.class as usize],
        application: reloc_interface_use(&source.application, reloc),
        premises: source
            .premises
            .iter()
            .map(|premise| lm_bytecode::BcConformancePremise {
                param: premise.param,
                bounds: premise
                    .bounds
                    .iter()
                    .map(|bound| reloc_interface_use(bound, reloc))
                    .collect(),
            })
            .collect(),
        associated: source
            .associated
            .iter()
            .map(|item| reloc.types[*item as usize])
            .collect(),
        method_overrides: source.method_overrides.clone(),
    }
}

pub(crate) fn reloc_func(func: &Func, reloc: &Reloc) -> Func {
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
        param_names: func.param_names.clone(),
        ret: reloc.types[func.ret as usize],
        row: reloc_row(&func.row),
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
pub(crate) fn reloc_instr(instr: &Instr, reloc: &Reloc) -> Instr {
    match instr {
        Instr::ConstStr(idx) => Instr::ConstStr(reloc.strings[*idx as usize]),
        Instr::ConstBytes(idx) => Instr::ConstBytes(reloc.bytes[*idx as usize]),
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
        // The reply type index names a module type, so it moves with
        // the type table.
        Instr::Perform { op, argc, reply_ty } => Instr::Perform {
            op: *op,
            argc: *argc,
            reply_ty: reloc.types[*reply_ty as usize],
        },
        Instr::PerformValue { argc, reply_ty } => Instr::PerformValue {
            argc: *argc,
            reply_ty: reloc.types[*reply_ty as usize],
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
        Instr::MapPut { ty, discard } => Instr::MapPut {
            ty: reloc.types[*ty as usize],
            discard: *discard,
        },
        // Every remaining operand is function-local or manifest-dense.
        Instr::ConstUnit
        | Instr::ConstBool(_)
        | Instr::ConstInt(_)
        | Instr::ConstFloat(_)
        | Instr::ConstChar(_)
        | Instr::Numeric(_)
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
        | Instr::Native(lm_bytecode::NativeInstr::EqStr)
        | Instr::Native(lm_bytecode::NativeInstr::NeStr)
        | Instr::Native(lm_bytecode::NativeInstr::StrByteLen)
        | Instr::Native(lm_bytecode::NativeInstr::StrCharCount)
        | Instr::Native(lm_bytecode::NativeInstr::StrConcat)
        | Instr::Native(lm_bytecode::NativeInstr::StrStartsWith)
        | Instr::Native(lm_bytecode::NativeInstr::StrEndsWith)
        | Instr::Native(lm_bytecode::NativeInstr::StrContains)
        | Instr::Native(lm_bytecode::NativeInstr::StrFindIndex)
        | Instr::Native(lm_bytecode::NativeInstr::TextFindByteIndex)
        | Instr::Native(lm_bytecode::NativeInstr::TextAtByte)
        | Instr::Native(lm_bytecode::NativeInstr::TextTrim)
        | Instr::Native(lm_bytecode::NativeInstr::TextTrimStart)
        | Instr::Native(lm_bytecode::NativeInstr::TextTrimEnd)
        | Instr::Native(lm_bytecode::NativeInstr::TextToLowerAscii)
        | Instr::Native(lm_bytecode::NativeInstr::TextToUpperAscii)
        | Instr::Native(lm_bytecode::NativeInstr::TextReplace)
        | Instr::Native(lm_bytecode::NativeInstr::TextParseIntStatus)
        | Instr::Native(lm_bytecode::NativeInstr::TextParseIntValue)
        | Instr::Native(lm_bytecode::NativeInstr::TextPadStart)
        | Instr::Native(lm_bytecode::NativeInstr::TextPadEnd)
        | Instr::Native(lm_bytecode::NativeInstr::TextHash)
        | Instr::Native(lm_bytecode::NativeInstr::BytesEndsWith)
        | Instr::Native(lm_bytecode::NativeInstr::BytesContains)
        | Instr::Native(lm_bytecode::NativeInstr::TextSplit)
        | Instr::Native(lm_bytecode::NativeInstr::TextLines)
        | Instr::Native(lm_bytecode::NativeInstr::TextAt)
        | Instr::Native(lm_bytecode::NativeInstr::TextSlice)
        | Instr::Native(lm_bytecode::NativeInstr::TextIsBoundary)
        | Instr::Native(lm_bytecode::NativeInstr::TextSliceBytes)
        | Instr::Native(lm_bytecode::NativeInstr::TextBytes)
        | Instr::Native(lm_bytecode::NativeInstr::TextLt)
        | Instr::Native(lm_bytecode::NativeInstr::TextLe)
        | Instr::Native(lm_bytecode::NativeInstr::TextGt)
        | Instr::Native(lm_bytecode::NativeInstr::TextGe)
        | Instr::Native(lm_bytecode::NativeInstr::TextToString)
        | Instr::Native(lm_bytecode::NativeInstr::CharCodepoint)
        | Instr::Native(lm_bytecode::NativeInstr::CharUtf8Len)
        | Instr::Native(lm_bytecode::NativeInstr::EqChar)
        | Instr::Native(lm_bytecode::NativeInstr::NeChar)
        | Instr::Native(lm_bytecode::NativeInstr::LtChar)
        | Instr::Native(lm_bytecode::NativeInstr::LeChar)
        | Instr::Native(lm_bytecode::NativeInstr::GtChar)
        | Instr::Native(lm_bytecode::NativeInstr::GeChar)
        | Instr::EqRef
        | Instr::EqValue
        | Instr::NeValue
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
        | Instr::Native(lm_bytecode::NativeInstr::SbNew)
        | Instr::Native(lm_bytecode::NativeInstr::SbAppendStr)
        | Instr::Native(lm_bytecode::NativeInstr::SbAppendInt)
        | Instr::Native(lm_bytecode::NativeInstr::SbAppendBool)
        | Instr::Native(lm_bytecode::NativeInstr::SbBuild)
        | Instr::Native(lm_bytecode::NativeInstr::SbLen)
        | Instr::Native(lm_bytecode::NativeInstr::SbClear)
        | Instr::Native(lm_bytecode::NativeInstr::SbAppendChar)
        | Instr::Native(lm_bytecode::NativeInstr::SbByteLen)
        | Instr::Native(lm_bytecode::NativeInstr::SbFinish)
        | Instr::Native(lm_bytecode::NativeInstr::BbNew)
        | Instr::Native(lm_bytecode::NativeInstr::BbAppend)
        | Instr::Native(lm_bytecode::NativeInstr::BbLen)
        | Instr::Native(lm_bytecode::NativeInstr::BbBuild)
        | Instr::Native(lm_bytecode::NativeInstr::BbExtend)
        | Instr::Native(lm_bytecode::NativeInstr::BbReserve)
        | Instr::Native(lm_bytecode::NativeInstr::BbClear)
        | Instr::Native(lm_bytecode::NativeInstr::BbFinish)
        | Instr::Native(lm_bytecode::NativeInstr::BbAt)
        | Instr::Native(lm_bytecode::NativeInstr::BbFindFrom)
        | Instr::Native(lm_bytecode::NativeInstr::BytesNew)
        | Instr::Native(lm_bytecode::NativeInstr::BytesLen)
        | Instr::Native(lm_bytecode::NativeInstr::BytesText)
        | Instr::Native(lm_bytecode::NativeInstr::BytesTextRange)
        | Instr::Native(lm_bytecode::NativeInstr::BytesAt)
        | Instr::Native(lm_bytecode::NativeInstr::BytesGet)
        | Instr::Native(lm_bytecode::NativeInstr::BytesSlice)
        | Instr::Native(lm_bytecode::NativeInstr::BytesConcat)
        | Instr::Native(lm_bytecode::NativeInstr::BytesStartsWith)
        | Instr::Native(lm_bytecode::NativeInstr::BytesFindIndex)
        | Instr::Native(lm_bytecode::NativeInstr::BytesHex)
        | Instr::Native(lm_bytecode::NativeInstr::BytesIsUtf8)
        | Instr::Native(lm_bytecode::NativeInstr::EqBytes)
        | Instr::Native(lm_bytecode::NativeInstr::NeBytes)
        | Instr::Native(lm_bytecode::NativeInstr::LtBytes)
        | Instr::Native(lm_bytecode::NativeInstr::LeBytes)
        | Instr::Native(lm_bytecode::NativeInstr::GtBytes)
        | Instr::Native(lm_bytecode::NativeInstr::GeBytes)
        | Instr::Native(lm_bytecode::NativeInstr::BytesCompact)
        | Instr::Native(lm_bytecode::NativeInstr::BytesTextView)
        | Instr::Native(lm_bytecode::NativeInstr::BytesHash)
        | Instr::Native(lm_bytecode::NativeInstr::HashCombine)
        | Instr::Native(lm_bytecode::NativeInstr::HashUnorderedCombine)
        | Instr::Freeze
        | Instr::EqDigest
        | Instr::NeDigest
        | Instr::Jump(_)
        | Instr::JumpIfFalse(_)
        | Instr::JumpIfTrue(_)
        | Instr::Return
        | Instr::OpConst(_)
        | Instr::TableEdit { .. }
        | Instr::CallArgs
        | Instr::FaultCode
        | Instr::FaultDenied
        | Instr::RaiseUserPanic
        | Instr::RaiseAssertionFailed
        | Instr::RaiseFault
        | Instr::RequestOp
        | Instr::Unreachable => *instr,
        Instr::Digest { ty } => Instr::Digest {
            ty: reloc.types[*ty as usize],
        },
        Instr::AsCall { op, ty } => Instr::AsCall {
            op: *op,
            ty: reloc.types[*ty as usize],
        },
        Instr::CallInterface { site, recv_ty, app } => {
            let (interface, method) = lm_bytecode::unpack_interface_call_site(*site);
            let relocated = reloc.interfaces[interface as usize];
            Instr::CallInterface {
                site: lm_bytecode::pack_interface_call_site(relocated, method)
                    .expect("the linked interface count was checked"),
                recv_ty: reloc.types[*recv_ty as usize],
                app: if *app == lm_bytecode::NO_APP {
                    lm_bytecode::NO_APP
                } else {
                    reloc.apps[*app as usize]
                },
            }
        }
        Instr::Extended(instr) => Instr::Extended(reloc_extended(instr, reloc)),
    }
}

pub(crate) fn reloc_extended(instr: &ExtendedInstr, reloc: &Reloc) -> ExtendedInstr {
    match instr {
        ExtendedInstr::MakeCallback { func, captures } => ExtendedInstr::MakeCallback {
            func: reloc.funcs[*func as usize],
            captures: *captures,
        },
        ExtendedInstr::FunctionCode { func } => ExtendedInstr::FunctionCode {
            func: reloc.funcs[*func as usize],
        },
        ExtendedInstr::ClassCode { class } => ExtendedInstr::ClassCode {
            class: reloc.classes[*class as usize],
        },
        ExtendedInstr::CodeSource { ty } => ExtendedInstr::CodeSource {
            ty: reloc.types[*ty as usize],
        },
        ExtendedInstr::CodeDefinition => ExtendedInstr::CodeDefinition,
        ExtendedInstr::FaultSite { ty } => ExtendedInstr::FaultSite {
            ty: reloc.types[*ty as usize],
        },
        ExtendedInstr::FaultTrace { ty } => ExtendedInstr::FaultTrace {
            ty: reloc.types[*ty as usize],
        },
        ExtendedInstr::OptionSome { ty } => ExtendedInstr::OptionSome {
            ty: reloc.types[*ty as usize],
        },
        ExtendedInstr::OptionNone { ty } => ExtendedInstr::OptionNone {
            ty: reloc.types[*ty as usize],
        },
        ExtendedInstr::OptionPayload { ty } => ExtendedInstr::OptionPayload {
            ty: reloc.types[*ty as usize],
        },
        ExtendedInstr::ListGet { ty } => ExtendedInstr::ListGet {
            ty: reloc.types[*ty as usize],
        },
        ExtendedInstr::MapGet { ty } => ExtendedInstr::MapGet {
            ty: reloc.types[*ty as usize],
        },
        ExtendedInstr::MapPutText { ty, discard } => ExtendedInstr::MapPutText {
            ty: reloc.types[*ty as usize],
            discard: *discard,
        },
        ExtendedInstr::ListPop { ty } => ExtendedInstr::ListPop {
            ty: reloc.types[*ty as usize],
        },
        ExtendedInstr::MapRemove { ty } => ExtendedInstr::MapRemove {
            ty: reloc.types[*ty as usize],
        },
        ExtendedInstr::DynPack { ty } => ExtendedInstr::DynPack {
            ty: reloc.types[*ty as usize],
        },
        ExtendedInstr::PrepareWait { op_argc, reply_ty } => ExtendedInstr::PrepareWait {
            op_argc: *op_argc,
            reply_ty: reloc.types[*reply_ty as usize],
        },
        ExtendedInstr::CallSlot { slot, app } => ExtendedInstr::CallSlot {
            slot: reloc.slots[*slot as usize],
            app: if *app == lm_bytecode::NO_APP {
                lm_bytecode::NO_APP
            } else {
                reloc.apps[*app as usize]
            },
        },
        ExtendedInstr::NewSlot { slot, app } => ExtendedInstr::NewSlot {
            slot: reloc.slots[*slot as usize],
            app: if *app == lm_bytecode::NO_APP {
                lm_bytecode::NO_APP
            } else {
                reloc.apps[*app as usize]
            },
        },
        ExtendedInstr::LoadSlot { slot } => ExtendedInstr::LoadSlot {
            slot: reloc.slots[*slot as usize],
        },
        ExtendedInstr::SendSlot { slot } => ExtendedInstr::SendSlot {
            slot: reloc.slots[*slot as usize],
        },
        ExtendedInstr::AsCallback
        | ExtendedInstr::ListEpoch
        | ExtendedInstr::ListIterLen
        | ExtendedInstr::MapEpoch
        | ExtendedInstr::MapIterLen
        | ExtendedInstr::MapNextIndex
        | ExtendedInstr::SealInstance
        | ExtendedInstr::MapKeyAt
        | ExtendedInstr::MapValueAt
        | ExtendedInstr::ListCapacity
        | ExtendedInstr::ListSet
        | ExtendedInstr::ListInsert
        | ExtendedInstr::ListRemove
        | ExtendedInstr::ListSwapRemove
        | ExtendedInstr::ListReserve
        | ExtendedInstr::ListTruncate
        | ExtendedInstr::ListContains
        | ExtendedInstr::ListReorder
        | ExtendedInstr::MapClear
        | ExtendedInstr::MapReserve
        | ExtendedInstr::MapProbe
        | ExtendedInstr::MapProbeFound
        | ExtendedInstr::MapProbeKey
        | ExtendedInstr::MapProbeValue
        | ExtendedInstr::MapProbeSetValue
        | ExtendedInstr::MapProbeRemove
        | ExtendedInstr::MapInsertHashed
        | ExtendedInstr::MapWriteGuard
        | ExtendedInstr::SyntaxTreeRoot
        | ExtendedInstr::SyntaxKind
        | ExtendedInstr::SyntaxCategory
        | ExtendedInstr::SyntaxRangeStart
        | ExtendedInstr::SyntaxRangeEnd
        | ExtendedInstr::SyntaxText
        | ExtendedInstr::SyntaxChildren
        | ExtendedInstr::SyntaxDetach
        | ExtendedInstr::DynRender
        | ExtendedInstr::SyntaxBuildToken
        | ExtendedInstr::SyntaxBuildTrivia
        | ExtendedInstr::SyntaxBuildNode
        | ExtendedInstr::SyntaxToTree => *instr,
    }
}

pub(crate) fn reloc_slot(source: &SlotSpec, reloc: &Reloc) -> SlotSpec {
    SlotSpec {
        binding: source.binding.clone(),
        late: source.late,
        key: source.key,
        contract_hash: source.contract_hash,
        contract: reloc_slot_contract(&source.contract, reloc),
        initial: source
            .initial
            .map(|target| reloc_slot_target(target, reloc)),
    }
}

pub(crate) fn reloc_class(source: &BcClass, reloc: &Reloc) -> BcClass {
    BcClass {
        name: source.name.clone(),
        key: source.key.clone(),
        is_final: source.is_final,
        is_frozen: source.is_frozen,
        parent: source
            .parent()
            .map(|parent| reloc.classes[parent as usize])
            .unwrap_or(NO_PARENT),
        parent_args: source
            .parent_args
            .iter()
            .map(|ty| reloc.types[*ty as usize])
            .collect(),
        type_params: source.type_params,
        kind: source.kind,
        fields: source
            .fields
            .iter()
            .map(|(name, ty)| (name.clone(), reloc.types[*ty as usize]))
            .collect(),
        field_defaults: source.field_defaults.clone(),
        own_start: source.own_start,
        has_init: source.has_init,
        methods: source
            .methods
            .iter()
            .filter_map(|(selector, function)| {
                let selector = reloc.selectors[*selector as usize];
                let function = reloc.funcs[*function as usize];
                (selector != u32::MAX && function != u32::MAX).then_some((selector, function))
            })
            .collect(),
    }
}
