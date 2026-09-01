//! Exhaustive native treatment data for every LMBC opcode.

use lm_bytecode::{ExtendedInstr, Instr, NativeInstr, NumericInstr};

/// The production treatment for one opcode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreatmentClass {
    /// Emit pure register code.
    Inline,
    /// Emit direct memory access after one guard.
    Guarded,
    /// Emit an inline fast path with one typed slow path.
    FastPath,
    /// Use the native calling convention.
    Call,
    /// Call one fixed typed runtime helper.
    Helper,
    /// Materialize canonical state and leave native code.
    Exit,
}

/// The control-flow behavior after one production treatment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitBehavior {
    Continue,
    Branch,
    Call,
    Allocation,
    Effect,
    Boundary,
    Return,
    Fault,
}

/// The canonical operand shape for one native fault.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultStack {
    None,
    Before,
    Pop(u8),
}

/// The complete JIT ledger entry for one opcode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstructionTreatment {
    class: TreatmentClass,
    exit: ExitBehavior,
    replay: bool,
    fault_stack: FaultStack,
}

impl InstructionTreatment {
    const fn dedicated(class: TreatmentClass, exit: ExitBehavior) -> InstructionTreatment {
        InstructionTreatment {
            class,
            exit,
            replay: false,
            fault_stack: FaultStack::None,
        }
    }

    const fn with_replay(mut self) -> InstructionTreatment {
        self.replay = true;
        self
    }

    const fn with_fault_stack(mut self, fault_stack: FaultStack) -> InstructionTreatment {
        self.fault_stack = fault_stack;
        self
    }

    /// Return the production treatment class.
    pub fn class(self) -> TreatmentClass {
        self.class
    }

    /// Return the production exit behavior.
    pub fn exit(self) -> ExitBehavior {
        self.exit
    }

    /// Return true when a guard can replay this instruction.
    pub fn replays(self) -> bool {
        self.replay
    }

    /// Return the canonical operand shape for one native fault.
    pub fn fault_stack(self) -> FaultStack {
        self.fault_stack
    }
}

const fn dedicated(class: TreatmentClass) -> InstructionTreatment {
    InstructionTreatment::dedicated(class, ExitBehavior::Continue)
}

/// Return the exhaustive JIT treatment for one instruction.
pub fn instruction_treatment(instruction: &Instr) -> InstructionTreatment {
    use TreatmentClass::{Call, Exit, FastPath, Guarded, Helper, Inline};

    match instruction {
        Instr::ConstUnit
        | Instr::ConstBool(_)
        | Instr::ConstInt(_)
        | Instr::ConstFloat(_)
        | Instr::ConstChar(_)
        | Instr::ConstStr(_)
        | Instr::ConstBytes(_)
        | Instr::LoadLocal(_)
        | Instr::StoreLocal(_)
        | Instr::Pop
        | Instr::Not
        | Instr::LtInt
        | Instr::LeInt
        | Instr::GtInt
        | Instr::GeInt
        | Instr::EqInt
        | Instr::NeInt
        | Instr::EqBool
        | Instr::NeBool
        | Instr::EqRef
        | Instr::NeRef
        | Instr::OpConst(_) => dedicated(Inline),
        Instr::Add | Instr::Sub | Instr::Mul | Instr::Div | Instr::Rem => {
            dedicated(Inline).with_fault_stack(FaultStack::Pop(2))
        }
        Instr::Neg => dedicated(Inline).with_fault_stack(FaultStack::Pop(1)),
        Instr::Native(operation) => native_treatment(*operation),
        Instr::Numeric(operation) => numeric_treatment(*operation),
        Instr::Call(_) => InstructionTreatment::dedicated(Call, ExitBehavior::Call),
        Instr::CallG { .. }
        | Instr::CallVirtual { .. }
        | Instr::CallVirtualG { .. }
        | Instr::CallInterface { .. } => {
            InstructionTreatment::dedicated(Call, ExitBehavior::Call).with_replay()
        }
        Instr::CallValue { .. } => {
            InstructionTreatment::dedicated(Call, ExitBehavior::Call).with_replay()
        }
        Instr::MakeClosure { .. } => {
            InstructionTreatment::dedicated(FastPath, ExitBehavior::Allocation).with_replay()
        }
        Instr::LoadCapture(_) => dedicated(Guarded).with_replay(),
        Instr::New(_) | Instr::NewG { .. } => {
            InstructionTreatment::dedicated(FastPath, ExitBehavior::Allocation)
                .with_replay()
                .with_fault_stack(FaultStack::Before)
        }
        Instr::LoadField(_) | Instr::TupleGet(_) | Instr::ListLen => dedicated(Guarded)
            .with_replay()
            .with_fault_stack(FaultStack::Pop(1)),
        Instr::StoreField(_) | Instr::ListAt => dedicated(Guarded)
            .with_replay()
            .with_fault_stack(FaultStack::Pop(2)),
        Instr::IsType(_) | Instr::CastType(_) => dedicated(Guarded).with_replay(),
        Instr::TupleNew { .. } | Instr::ListNew { .. } => {
            InstructionTreatment::dedicated(FastPath, ExitBehavior::Allocation).with_replay()
        }
        Instr::MapNew { .. } => {
            InstructionTreatment::dedicated(FastPath, ExitBehavior::Allocation).with_replay()
        }
        Instr::ListPush => dedicated(FastPath)
            .with_replay()
            .with_fault_stack(FaultStack::Pop(2)),
        Instr::MapLen => dedicated(Guarded).with_replay(),
        Instr::MapHas | Instr::MapAt => dedicated(Helper)
            .with_replay()
            .with_fault_stack(FaultStack::Pop(2)),
        Instr::MapPut { .. } => dedicated(Helper)
            .with_replay()
            .with_fault_stack(FaultStack::Pop(3)),
        Instr::EqValue | Instr::NeValue => dedicated(Helper)
            .with_replay()
            .with_fault_stack(FaultStack::Pop(2)),
        Instr::Freeze => dedicated(Helper)
            .with_replay()
            .with_fault_stack(FaultStack::Pop(1)),
        Instr::Digest { .. } => dedicated(Helper)
            .with_replay()
            .with_fault_stack(FaultStack::Pop(1)),
        Instr::EqDigest | Instr::NeDigest => dedicated(Guarded).with_replay(),
        Instr::Jump(_) | Instr::JumpIfFalse(_) | Instr::JumpIfTrue(_) => {
            InstructionTreatment::dedicated(Inline, ExitBehavior::Branch)
        }
        Instr::Return => InstructionTreatment::dedicated(Inline, ExitBehavior::Return),
        Instr::Perform { .. } | Instr::PerformValue { .. } => {
            InstructionTreatment::dedicated(Exit, ExitBehavior::Effect)
        }
        Instr::TableEdit { .. } | Instr::RequestOp | Instr::AsCall { .. } | Instr::CallArgs => {
            InstructionTreatment::dedicated(Exit, ExitBehavior::Boundary)
        }
        Instr::FaultCode | Instr::FaultDenied => {
            InstructionTreatment::dedicated(Helper, ExitBehavior::Allocation)
                .with_replay()
                .with_fault_stack(FaultStack::Pop(1))
        }
        Instr::RaiseUserPanic | Instr::RaiseAssertionFailed | Instr::RaiseFault => {
            InstructionTreatment::dedicated(Exit, ExitBehavior::Fault)
        }
        Instr::Unreachable => InstructionTreatment::dedicated(Exit, ExitBehavior::Fault)
            .with_fault_stack(FaultStack::Before),
        Instr::Extended(operation) => extended_treatment(*operation),
    }
}

fn numeric_treatment(operation: NumericInstr) -> InstructionTreatment {
    use TreatmentClass::{Helper, Inline};

    match operation {
        NumericInstr::IntBitAnd
        | NumericInstr::IntBitOr
        | NumericInstr::IntBitXor
        | NumericInstr::IntBitNot
        | NumericInstr::IntWrappingAdd
        | NumericInstr::IntWrappingSub
        | NumericInstr::IntWrappingMul
        | NumericInstr::IntToFloat
        | NumericInstr::FloatNeg
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
        | NumericInstr::FloatIsNan
        | NumericInstr::FloatHash
        | NumericInstr::FloatBits
        | NumericInstr::FloatFromBits
        | NumericInstr::FloatToIntStatus => dedicated(Inline),
        NumericInstr::IntShl
        | NumericInstr::IntShr
        | NumericInstr::IntUshr
        | NumericInstr::IntRotateLeft
        | NumericInstr::IntRotateRight
        | NumericInstr::FloatToIntValue => dedicated(Inline).with_replay(),
        NumericInstr::SbAppendFloat => dedicated(Helper)
            .with_replay()
            .with_fault_stack(FaultStack::Pop(2)),
        NumericInstr::BytesBitAnd | NumericInstr::BytesBitOr | NumericInstr::BytesBitXor => {
            InstructionTreatment::dedicated(Helper, ExitBehavior::Allocation)
                .with_replay()
                .with_fault_stack(FaultStack::Pop(2))
        }
        NumericInstr::BytesBitNot => {
            InstructionTreatment::dedicated(Helper, ExitBehavior::Allocation)
                .with_replay()
                .with_fault_stack(FaultStack::Pop(1))
        }
        NumericInstr::TextParseFloatStatus | NumericInstr::TextParseFloatValue => dedicated(Helper)
            .with_replay()
            .with_fault_stack(FaultStack::Pop(1)),
        NumericInstr::FloatFixed => {
            InstructionTreatment::dedicated(Helper, ExitBehavior::Allocation)
                .with_replay()
                .with_fault_stack(FaultStack::Pop(2))
        }
    }
}

fn extended_treatment(operation: ExtendedInstr) -> InstructionTreatment {
    use TreatmentClass::{Call, Exit, FastPath, Guarded, Helper, Inline};

    match operation {
        ExtendedInstr::MakeCallback { .. } => {
            InstructionTreatment::dedicated(FastPath, ExitBehavior::Allocation).with_replay()
        }
        ExtendedInstr::AsCallback => dedicated(Guarded).with_replay(),
        ExtendedInstr::OptionSome { .. } => dedicated(Inline),
        ExtendedInstr::OptionNone { .. } => dedicated(Inline),
        ExtendedInstr::OptionPayload { .. } => dedicated(Guarded)
            .with_replay()
            .with_fault_stack(FaultStack::Before),
        ExtendedInstr::ListGet { .. } => dedicated(Guarded)
            .with_replay()
            .with_fault_stack(FaultStack::Pop(2)),
        ExtendedInstr::MapGet { .. } => dedicated(Helper)
            .with_replay()
            .with_fault_stack(FaultStack::Pop(2)),
        ExtendedInstr::MapProbe => dedicated(Helper)
            .with_replay()
            .with_fault_stack(FaultStack::Pop(3)),
        ExtendedInstr::MapNextIndex => dedicated(Guarded)
            .with_replay()
            .with_fault_stack(FaultStack::Pop(3)),
        ExtendedInstr::MapProbeKey
        | ExtendedInstr::MapProbeValue
        | ExtendedInstr::MapProbeRemove => dedicated(Helper)
            .with_replay()
            .with_fault_stack(FaultStack::Pop(2)),
        ExtendedInstr::MapKeyAt | ExtendedInstr::MapValueAt => dedicated(Guarded)
            .with_replay()
            .with_fault_stack(FaultStack::Pop(2)),
        ExtendedInstr::MapRemove { .. } => dedicated(FastPath)
            .with_replay()
            .with_fault_stack(FaultStack::Pop(2)),
        ExtendedInstr::MapClear => dedicated(Helper)
            .with_replay()
            .with_fault_stack(FaultStack::Pop(1)),
        ExtendedInstr::MapReserve => dedicated(Helper)
            .with_replay()
            .with_fault_stack(FaultStack::Pop(2)),
        ExtendedInstr::MapProbeFound => dedicated(Inline)
            .with_replay()
            .with_fault_stack(FaultStack::Pop(1)),
        ExtendedInstr::MapProbeSetValue => dedicated(Helper)
            .with_replay()
            .with_fault_stack(FaultStack::Pop(3)),
        ExtendedInstr::MapInsertHashed => dedicated(Helper)
            .with_replay()
            .with_fault_stack(FaultStack::Pop(5)),
        ExtendedInstr::MapWriteGuard => dedicated(Guarded)
            .with_replay()
            .with_fault_stack(FaultStack::Pop(1)),
        ExtendedInstr::ListEpoch
        | ExtendedInstr::ListIterLen
        | ExtendedInstr::SealInstance
        | ExtendedInstr::ListCapacity => dedicated(Guarded).with_replay(),
        ExtendedInstr::ListSet => dedicated(Guarded)
            .with_replay()
            .with_fault_stack(FaultStack::Pop(3)),
        ExtendedInstr::MapEpoch | ExtendedInstr::MapIterLen => dedicated(Guarded).with_replay(),
        ExtendedInstr::ListPop { .. } => dedicated(Guarded)
            .with_replay()
            .with_fault_stack(FaultStack::Pop(1)),
        ExtendedInstr::ListInsert => dedicated(FastPath)
            .with_replay()
            .with_fault_stack(FaultStack::Pop(3)),
        ExtendedInstr::ListRemove | ExtendedInstr::ListSwapRemove => dedicated(Guarded)
            .with_replay()
            .with_fault_stack(FaultStack::Pop(2)),
        ExtendedInstr::ListTruncate => dedicated(Guarded)
            .with_replay()
            .with_fault_stack(FaultStack::Pop(2)),
        ExtendedInstr::ListReserve => dedicated(FastPath)
            .with_replay()
            .with_fault_stack(FaultStack::Pop(2)),
        ExtendedInstr::ListReorder => dedicated(FastPath)
            .with_replay()
            .with_fault_stack(FaultStack::Pop(1)),
        ExtendedInstr::ListContains => dedicated(Helper)
            .with_replay()
            .with_fault_stack(FaultStack::Pop(2)),
        ExtendedInstr::CallSlot { .. } | ExtendedInstr::NewSlot { .. } => {
            InstructionTreatment::dedicated(Call, ExitBehavior::Call).with_replay()
        }
        ExtendedInstr::LoadSlot { .. } => {
            InstructionTreatment::dedicated(Exit, ExitBehavior::Boundary)
        }
        ExtendedInstr::SendSlot { .. } | ExtendedInstr::PrepareWait { .. } => {
            InstructionTreatment::dedicated(Exit, ExitBehavior::Effect)
        }
        ExtendedInstr::SyntaxKind
        | ExtendedInstr::SyntaxCategory
        | ExtendedInstr::SyntaxRangeStart
        | ExtendedInstr::SyntaxRangeEnd => dedicated(Helper)
            .with_replay()
            .with_fault_stack(FaultStack::Pop(1)),
        ExtendedInstr::SyntaxTreeRoot
        | ExtendedInstr::SyntaxText
        | ExtendedInstr::SyntaxChildren
        | ExtendedInstr::SyntaxDetach
        | ExtendedInstr::SyntaxToTree => dedicated(Helper)
            .with_replay()
            .with_fault_stack(FaultStack::Pop(1)),
        ExtendedInstr::DynPack { .. } => {
            InstructionTreatment::dedicated(Helper, ExitBehavior::Allocation)
                .with_replay()
                .with_fault_stack(FaultStack::Pop(1))
        }
        ExtendedInstr::DynRender
        | ExtendedInstr::FunctionCode { .. }
        | ExtendedInstr::ClassCode { .. }
        | ExtendedInstr::CodeSource { .. }
        | ExtendedInstr::CodeDefinition
        | ExtendedInstr::FaultSite { .. }
        | ExtendedInstr::FaultTrace { .. } => {
            InstructionTreatment::dedicated(Exit, ExitBehavior::Boundary)
        }
        ExtendedInstr::SyntaxBuildToken
        | ExtendedInstr::SyntaxBuildTrivia
        | ExtendedInstr::SyntaxBuildNode => dedicated(Helper)
            .with_replay()
            .with_fault_stack(FaultStack::Pop(3)),
    }
}

fn native_treatment(operation: NativeInstr) -> InstructionTreatment {
    use TreatmentClass::{FastPath, Guarded, Helper, Inline};

    match operation {
        NativeInstr::StrByteLen
        | NativeInstr::StrCharCount
        | NativeInstr::BytesLen
        | NativeInstr::BytesAt
        | NativeInstr::BytesGet => dedicated(Guarded).with_replay(),
        NativeInstr::TextAtByte | NativeInstr::TextAt | NativeInstr::TextIsBoundary => {
            dedicated(Guarded).with_replay()
        }
        NativeInstr::CharCodepoint
        | NativeInstr::CharUtf8Len
        | NativeInstr::EqChar
        | NativeInstr::NeChar
        | NativeInstr::LtChar
        | NativeInstr::LeChar
        | NativeInstr::GtChar
        | NativeInstr::GeChar
        | NativeInstr::HashCombine
        | NativeInstr::HashUnorderedCombine => dedicated(Inline),
        NativeInstr::EqStr
        | NativeInstr::NeStr
        | NativeInstr::TextLt
        | NativeInstr::TextLe
        | NativeInstr::TextGt
        | NativeInstr::TextGe
        | NativeInstr::EqBytes
        | NativeInstr::NeBytes
        | NativeInstr::LtBytes
        | NativeInstr::LeBytes
        | NativeInstr::GtBytes
        | NativeInstr::GeBytes => dedicated(Helper)
            .with_replay()
            .with_fault_stack(FaultStack::Pop(2)),
        NativeInstr::TextHash | NativeInstr::BytesHash => dedicated(Helper)
            .with_replay()
            .with_fault_stack(FaultStack::Pop(1)),
        NativeInstr::SbNew | NativeInstr::BbNew => {
            InstructionTreatment::dedicated(Helper, ExitBehavior::Allocation)
                .with_replay()
                .with_fault_stack(FaultStack::Before)
        }
        NativeInstr::SbBuild
        | NativeInstr::SbFinish
        | NativeInstr::BbBuild
        | NativeInstr::BbFinish
        | NativeInstr::BytesNew
        | NativeInstr::BytesCompact
        | NativeInstr::BytesTextView => {
            InstructionTreatment::dedicated(Helper, ExitBehavior::Allocation)
                .with_replay()
                .with_fault_stack(FaultStack::Pop(1))
        }
        NativeInstr::BytesSlice => {
            InstructionTreatment::dedicated(Helper, ExitBehavior::Allocation)
                .with_replay()
                .with_fault_stack(FaultStack::Pop(3))
        }
        NativeInstr::BytesConcat => {
            InstructionTreatment::dedicated(Helper, ExitBehavior::Allocation)
                .with_replay()
                .with_fault_stack(FaultStack::Pop(2))
        }
        NativeInstr::SbAppendStr
        | NativeInstr::SbAppendInt
        | NativeInstr::SbAppendBool
        | NativeInstr::SbAppendChar
        | NativeInstr::BbAppend
        | NativeInstr::BbExtend
        | NativeInstr::BbReserve => dedicated(FastPath)
            .with_replay()
            .with_fault_stack(FaultStack::Pop(2)),
        NativeInstr::SbClear | NativeInstr::BbClear => dedicated(Guarded)
            .with_replay()
            .with_fault_stack(FaultStack::Pop(1)),
        NativeInstr::SbByteLen | NativeInstr::SbLen | NativeInstr::BbLen => dedicated(Guarded)
            .with_replay()
            .with_fault_stack(FaultStack::Pop(1)),
        NativeInstr::BbAt => dedicated(Guarded)
            .with_replay()
            .with_fault_stack(FaultStack::Pop(2)),
        NativeInstr::StrStartsWith
        | NativeInstr::StrEndsWith
        | NativeInstr::StrContains
        | NativeInstr::StrFindIndex
        | NativeInstr::TextFindByteIndex
        | NativeInstr::TextParseIntStatus
        | NativeInstr::TextParseIntValue
        | NativeInstr::BytesEndsWith
        | NativeInstr::BytesContains
        | NativeInstr::BytesStartsWith
        | NativeInstr::BytesFindIndex => dedicated(Helper)
            .with_replay()
            .with_fault_stack(FaultStack::Pop(2)),
        NativeInstr::BbFindFrom => dedicated(Helper)
            .with_replay()
            .with_fault_stack(FaultStack::Pop(3)),
        NativeInstr::BytesIsUtf8 => dedicated(Helper)
            .with_replay()
            .with_fault_stack(FaultStack::Pop(1)),
        NativeInstr::TextTrim
        | NativeInstr::TextTrimStart
        | NativeInstr::TextTrimEnd
        | NativeInstr::TextToLowerAscii
        | NativeInstr::TextToUpperAscii
        | NativeInstr::TextLines
        | NativeInstr::TextBytes
        | NativeInstr::TextToString
        | NativeInstr::BytesText
        | NativeInstr::BytesHex => {
            InstructionTreatment::dedicated(Helper, ExitBehavior::Allocation)
                .with_replay()
                .with_fault_stack(FaultStack::Pop(1))
        }
        NativeInstr::StrConcat
        | NativeInstr::TextPadStart
        | NativeInstr::TextPadEnd
        | NativeInstr::TextSplit => {
            InstructionTreatment::dedicated(Helper, ExitBehavior::Allocation)
                .with_replay()
                .with_fault_stack(FaultStack::Pop(2))
        }
        NativeInstr::TextReplace | NativeInstr::TextSlice | NativeInstr::TextSliceBytes => {
            InstructionTreatment::dedicated(Helper, ExitBehavior::Allocation)
                .with_replay()
                .with_fault_stack(FaultStack::Pop(3))
        }
    }
}
