//! Canonical operand-stack effects for decoded instructions.

use crate::{ExtendedInstr, Instr, Module, NativeInstr, NumericInstr, SlotContract};

/// Tables that provide variable operand counts.
pub trait StackEffectTables {
    /// Return the parameter count of one function.
    fn function_param_count(&self, function: u32) -> usize;

    /// Return the parameter count of one interface method.
    fn interface_param_count(&self, interface: u32, method: u32) -> usize;

    /// Return the parameter count of one late-binding slot.
    fn slot_param_count(&self, slot: u32) -> usize;
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
                SlotContract::Function(contract) | SlotContract::Method(contract) => {
                    contract.params.len()
                }
                SlotContract::Class { constructor, .. } => constructor.params.len(),
                SlotContract::Value { .. } | SlotContract::Process { .. } => 0,
            })
            .unwrap_or(0)
    }
}

/// Count the values that one instruction pops and pushes.
pub fn stack_effect(tables: &impl StackEffectTables, instruction: &Instr) -> (usize, usize) {
    match instruction {
        Instr::ConstUnit
        | Instr::ConstBool(_)
        | Instr::ConstInt(_)
        | Instr::ConstFloat(_)
        | Instr::ConstChar(_)
        | Instr::ConstStr(_)
        | Instr::ConstBytes(_)
        | Instr::ConstRegex(_)
        | Instr::LoadLocal(_)
        | Instr::LoadCapture(_)
        | Instr::New(_)
        | Instr::NewG { .. }
        | Instr::Native(NativeInstr::SbNew)
        | Instr::Native(NativeInstr::BbNew) => (0, 1),
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
        | Instr::Native(NativeInstr::EqStr)
        | Instr::Native(NativeInstr::NeStr)
        | Instr::EqRef
        | Instr::EqValue
        | Instr::NeValue
        | Instr::NeRef
        | Instr::Native(NativeInstr::StrConcat)
        | Instr::Native(NativeInstr::StrStartsWith)
        | Instr::Native(NativeInstr::StrEndsWith)
        | Instr::Native(NativeInstr::StrContains)
        | Instr::Native(NativeInstr::StrFindIndex)
        | Instr::Native(NativeInstr::TextFindByteIndex)
        | Instr::Native(NativeInstr::TextAtByte)
        | Instr::Native(NativeInstr::TextParseIntStatus)
        | Instr::Native(NativeInstr::TextParseIntValue)
        | Instr::Native(NativeInstr::TextPadStart)
        | Instr::Native(NativeInstr::TextPadEnd)
        | Instr::Native(NativeInstr::BytesEndsWith)
        | Instr::Native(NativeInstr::BytesContains)
        | Instr::Native(NativeInstr::TextSplit)
        | Instr::Native(NativeInstr::BytesAt)
        | Instr::Native(NativeInstr::BytesGet)
        | Instr::Native(NativeInstr::BytesReadU32Be)
        | Instr::Native(NativeInstr::BytesReadU32Le)
        | Instr::Native(NativeInstr::BytesConcat)
        | Instr::Native(NativeInstr::BytesStartsWith)
        | Instr::Native(NativeInstr::BytesFindIndex)
        | Instr::Native(NativeInstr::EqBytes)
        | Instr::Native(NativeInstr::NeBytes)
        | Instr::Native(NativeInstr::BbExtend)
        | Instr::Native(NativeInstr::BbReserve)
        | Instr::Native(NativeInstr::BbAt)
        | Instr::Native(NativeInstr::TextAt)
        | Instr::Native(NativeInstr::TextIsBoundary)
        | Instr::Native(NativeInstr::TextLt)
        | Instr::Native(NativeInstr::TextLe)
        | Instr::Native(NativeInstr::TextGt)
        | Instr::Native(NativeInstr::TextGe)
        | Instr::Native(NativeInstr::EqChar)
        | Instr::Native(NativeInstr::NeChar)
        | Instr::Native(NativeInstr::LtChar)
        | Instr::Native(NativeInstr::LeChar)
        | Instr::Native(NativeInstr::GtChar)
        | Instr::Native(NativeInstr::GeChar)
        | Instr::Native(NativeInstr::LtBytes)
        | Instr::Native(NativeInstr::LeBytes)
        | Instr::Native(NativeInstr::GtBytes)
        | Instr::Native(NativeInstr::GeBytes)
        | Instr::Native(NativeInstr::HashCombine)
        | Instr::Native(NativeInstr::HashUnorderedCombine)
        | Instr::Native(NativeInstr::RegexIsMatch)
        | Instr::Native(NativeInstr::RegexCount)
        | Instr::Native(NativeInstr::RegexSplit)
        | Instr::Native(NativeInstr::SbAppendChar) => (2, 1),
        Instr::Neg
        | Instr::Not
        | Instr::LoadField(_)
        | Instr::TupleGet(_)
        | Instr::IsType(_)
        | Instr::CastType(_)
        | Instr::ListLen
        | Instr::MapLen
        | Instr::Native(NativeInstr::SbBuild)
        | Instr::Native(NativeInstr::SbLen)
        | Instr::Native(NativeInstr::SbClear)
        | Instr::Native(NativeInstr::BbLen)
        | Instr::Native(NativeInstr::BbCapacity)
        | Instr::Native(NativeInstr::BbBuild)
        | Instr::Native(NativeInstr::BbClear)
        | Instr::Native(NativeInstr::StrByteLen)
        | Instr::Native(NativeInstr::StrCharCount)
        | Instr::Native(NativeInstr::BytesNew)
        | Instr::Native(NativeInstr::BytesLen)
        | Instr::Native(NativeInstr::BytesText)
        | Instr::Native(NativeInstr::BytesHex)
        | Instr::Native(NativeInstr::BytesIsUtf8)
        | Instr::Native(NativeInstr::TextBytes)
        | Instr::Native(NativeInstr::TextTrim)
        | Instr::Native(NativeInstr::TextTrimStart)
        | Instr::Native(NativeInstr::TextTrimEnd)
        | Instr::Native(NativeInstr::TextToLowerAscii)
        | Instr::Native(NativeInstr::TextToUpperAscii)
        | Instr::Native(NativeInstr::TextLines)
        | Instr::Native(NativeInstr::TextToString)
        | Instr::Native(NativeInstr::CharCodepoint)
        | Instr::Native(NativeInstr::CharUtf8Len)
        | Instr::Native(NativeInstr::BytesCompact)
        | Instr::Native(NativeInstr::BytesTextView)
        | Instr::Native(NativeInstr::TextHash)
        | Instr::Native(NativeInstr::BytesHash)
        | Instr::Native(NativeInstr::SbByteLen)
        | Instr::Native(NativeInstr::SbFinish)
        | Instr::Native(NativeInstr::BbFinish)
        | Instr::Native(NativeInstr::RegexCompileStatus)
        | Instr::Native(NativeInstr::RegexCompileValue)
        | Instr::Native(NativeInstr::RegexSource)
        | Instr::Native(NativeInstr::RegexMatchStart)
        | Instr::Native(NativeInstr::RegexMatchEnd)
        | Instr::Native(NativeInstr::RegexMatchText)
        | Instr::Native(NativeInstr::RegexMatchGroupCount)
        | Instr::Freeze
        | Instr::Digest { .. } => (1, 1),
        Instr::EqDigest | Instr::NeDigest => (2, 1),
        Instr::StoreField(_) => (2, 0),
        Instr::ListAt
        | Instr::ListPush
        | Instr::MapHas
        | Instr::MapAt
        | Instr::Native(NativeInstr::SbAppendStr)
        | Instr::Native(NativeInstr::SbAppendInt)
        | Instr::Native(NativeInstr::SbAppendBool)
        | Instr::Native(NativeInstr::BbAppend)
        | Instr::Native(NativeInstr::BbTruncate) => (2, 1),
        Instr::MapPut { discard: false, .. }
        | Instr::Native(NativeInstr::BytesSlice)
        | Instr::Native(NativeInstr::BytesTextRange)
        | Instr::Native(NativeInstr::TextSlice)
        | Instr::Native(NativeInstr::TextSliceBytes)
        | Instr::Native(NativeInstr::TextReplace)
        | Instr::Native(NativeInstr::RegexReplaceAll)
        | Instr::Native(NativeInstr::BbFindFrom)
        | Instr::Native(NativeInstr::BbSet) => (3, 1),
        Instr::MapPut { discard: true, .. } => (3, 0),
        Instr::ListNew { count, .. } | Instr::TupleNew { count, .. } => (*count as usize, 1),
        Instr::MapNew { count, .. } => (2 * *count as usize, 1),
        Instr::MakeClosure { captures, .. } => (*captures as usize, 1),
        Instr::Call(function) | Instr::CallG { func: function, .. } => {
            (tables.function_param_count(*function), 1)
        }
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
            if *action == 2 {
                (2, 1)
            } else {
                (1, 1)
            }
        }
        Instr::AsCall { .. }
        | Instr::CallArgs
        | Instr::FaultCode
        | Instr::FaultDenied
        | Instr::RequestOp => (1, 1),
        Instr::RaiseUserPanic | Instr::RaiseAssertionFailed | Instr::RaiseFault => (1, 0),
        Instr::Unreachable => (0, 0),
        Instr::CallInterface { site, .. } => {
            let (interface, method) = crate::unpack_interface_call_site(*site);
            (tables.interface_param_count(interface, method) + 1, 1)
        }
        Instr::Numeric(operation) => numeric_stack_effect(*operation),
        Instr::Extended(operation) => extended_stack_effect(tables, *operation),
    }
}

fn numeric_stack_effect(instruction: NumericInstr) -> (usize, usize) {
    match instruction {
        NumericInstr::IntBitNot
        | NumericInstr::IntCountOnes
        | NumericInstr::IntLeadingZeros
        | NumericInstr::IntTrailingZeros
        | NumericInstr::IntSignum
        | NumericInstr::IntToFloat
        | NumericInstr::FloatNeg
        | NumericInstr::FloatAbs
        | NumericInstr::FloatSqrt
        | NumericInstr::FloatFloor
        | NumericInstr::FloatCeil
        | NumericInstr::FloatRound
        | NumericInstr::FloatTrunc
        | NumericInstr::FloatExp
        | NumericInstr::FloatExp2
        | NumericInstr::FloatExpM1
        | NumericInstr::FloatLn
        | NumericInstr::FloatLog2
        | NumericInstr::FloatLog10
        | NumericInstr::FloatLn1P
        | NumericInstr::FloatCbrt
        | NumericInstr::FloatSin
        | NumericInstr::FloatCos
        | NumericInstr::FloatTan
        | NumericInstr::FloatAsin
        | NumericInstr::FloatAcos
        | NumericInstr::FloatAtan
        | NumericInstr::FloatSinh
        | NumericInstr::FloatCosh
        | NumericInstr::FloatTanh
        | NumericInstr::FloatAsinh
        | NumericInstr::FloatAcosh
        | NumericInstr::FloatAtanh
        | NumericInstr::FloatIsNan
        | NumericInstr::FloatIsFinite
        | NumericInstr::FloatIsInfinite
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
        | NumericInstr::IntRotateLeft32
        | NumericInstr::IntRotateRight32
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
        | NumericInstr::FloatMin
        | NumericInstr::FloatMax
        | NumericInstr::FloatRem
        | NumericInstr::FloatCopySign
        | NumericInstr::FloatPow
        | NumericInstr::FloatHypot
        | NumericInstr::FloatAtan2
        | NumericInstr::FloatFixed
        | NumericInstr::SbAppendFloat
        | NumericInstr::BytesBitAnd
        | NumericInstr::BytesBitOr
        | NumericInstr::BytesBitXor => (2, 1),
        NumericInstr::FloatMulAdd => (3, 1),
    }
}

fn extended_stack_effect(
    tables: &impl StackEffectTables,
    instruction: ExtendedInstr,
) -> (usize, usize) {
    match instruction {
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
        | ExtendedInstr::RegexCaptures { .. }
        | ExtendedInstr::RegexMatchGroup { .. }
        | ExtendedInstr::RegexMatchNamed { .. }
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
        ExtendedInstr::MapNextIndex
        | ExtendedInstr::ListSet
        | ExtendedInstr::ListSwap
        | ExtendedInstr::ListInsert
        | ExtendedInstr::MapPutText { discard: false, .. }
        | ExtendedInstr::SyntaxBuildToken
        | ExtendedInstr::SyntaxBuildTrivia
        | ExtendedInstr::SyntaxBuildNode
        | ExtendedInstr::MapProbe
        | ExtendedInstr::MapProbeSetValue => (3, 1),
        ExtendedInstr::MapPutText { discard: true, .. } => (3, 0),
        ExtendedInstr::MapInternTextRange => (4, 1),
        ExtendedInstr::MakeCallback { captures, .. } => (captures as usize, 1),
        ExtendedInstr::FunctionCode { .. }
        | ExtendedInstr::ClassCode { .. }
        | ExtendedInstr::ModuleCode { .. } => (0, 1),
        ExtendedInstr::CodeSource { .. }
        | ExtendedInstr::CodeDefinition
        | ExtendedInstr::FaultSite { .. }
        | ExtendedInstr::FaultTrace { .. }
        | ExtendedInstr::ReflectionDeclarations
        | ExtendedInstr::ReflectionMembers
        | ExtendedInstr::ReflectionName
        | ExtendedInstr::ReflectionDeclarationKind
        | ExtendedInstr::ReflectionMemberKind
        | ExtendedInstr::ReflectionTypeParameterCount
        | ExtendedInstr::ReflectionInterfaceNames
        | ExtendedInstr::SendSlot { .. }
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
        | ExtendedInstr::SyntaxToTree
        | ExtendedInstr::MapProbeFound
        | ExtendedInstr::MapWriteGuard => (1, 1),
        ExtendedInstr::ReflectionOpen => (2, 1),
        ExtendedInstr::ReflectionRefine { .. } => (1, 1),
        ExtendedInstr::ReflectionEnd { .. } => (0, 0),
        ExtendedInstr::CallSlot { slot, .. } | ExtendedInstr::NewSlot { slot, .. } => {
            (tables.slot_param_count(slot), 1)
        }
        ExtendedInstr::LoadSlot { .. } => (0, 1),
        ExtendedInstr::MapProbeKey
        | ExtendedInstr::MapProbeValue
        | ExtendedInstr::MapProbeRemove => (2, 1),
        ExtendedInstr::MapInsertHashed => (5, 1),
        ExtendedInstr::PrepareWait { op_argc, .. } => {
            let (_, argc) = ExtendedInstr::wait_parts(op_argc);
            (argc as usize, 1)
        }
    }
}
