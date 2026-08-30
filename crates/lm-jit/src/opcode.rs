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

/// The current implementation state for one opcode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreatmentStatus {
    /// The JIT has the production treatment.
    Dedicated,
    /// The JIT uses one temporary interpreter site.
    Temporary,
}

/// The control-flow behavior after one production treatment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitBehavior {
    Continue,
    Branch,
    Call,
    Allocation,
    Effect,
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
    status: TreatmentStatus,
    exit: ExitBehavior,
    replay: bool,
    fault_stack: FaultStack,
}

impl InstructionTreatment {
    const fn dedicated(class: TreatmentClass, exit: ExitBehavior) -> InstructionTreatment {
        InstructionTreatment {
            class,
            status: TreatmentStatus::Dedicated,
            exit,
            replay: false,
            fault_stack: FaultStack::None,
        }
    }

    const fn temporary(class: TreatmentClass, exit: ExitBehavior) -> InstructionTreatment {
        InstructionTreatment {
            class,
            status: TreatmentStatus::Temporary,
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

    /// Return the current implementation state.
    pub fn status(self) -> TreatmentStatus {
        self.status
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

    /// Return true when the JIT has the production treatment.
    pub fn is_dedicated(self) -> bool {
        self.status == TreatmentStatus::Dedicated
    }
}

const fn dedicated(class: TreatmentClass) -> InstructionTreatment {
    InstructionTreatment::dedicated(class, ExitBehavior::Continue)
}

const fn temporary(class: TreatmentClass) -> InstructionTreatment {
    InstructionTreatment::temporary(class, ExitBehavior::Continue)
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
        Instr::Call(_)
        | Instr::CallG { .. }
        | Instr::CallVirtual { .. }
        | Instr::CallVirtualG { .. }
        | Instr::CallInterface { .. } => InstructionTreatment::dedicated(Call, ExitBehavior::Call),
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
        Instr::TupleNew { .. } | Instr::ListNew { .. } | Instr::MapNew { .. } => {
            InstructionTreatment::temporary(FastPath, ExitBehavior::Allocation)
        }
        Instr::ListPush => dedicated(FastPath)
            .with_replay()
            .with_fault_stack(FaultStack::Pop(2)),
        Instr::MapLen => dedicated(Guarded).with_replay(),
        Instr::MapHas | Instr::MapAt | Instr::MapPut { .. } => temporary(Helper),
        Instr::Freeze | Instr::Digest { .. } | Instr::EqValue | Instr::NeValue => temporary(Helper),
        Instr::EqDigest | Instr::NeDigest => dedicated(Guarded).with_replay(),
        Instr::Jump(_) | Instr::JumpIfFalse(_) | Instr::JumpIfTrue(_) => {
            InstructionTreatment::dedicated(Inline, ExitBehavior::Branch)
        }
        Instr::Return => InstructionTreatment::dedicated(Inline, ExitBehavior::Return),
        Instr::Perform { .. } | Instr::PerformValue { .. } => {
            InstructionTreatment::dedicated(Exit, ExitBehavior::Effect)
        }
        Instr::TableEdit { .. } | Instr::RequestOp => {
            InstructionTreatment::temporary(Exit, ExitBehavior::Effect)
        }
        Instr::AsCall { .. } | Instr::CallArgs | Instr::FaultCode => temporary(Helper),
        Instr::FaultDenied => temporary(FastPath),
        Instr::RaiseUserPanic | Instr::RaiseAssertionFailed | Instr::RaiseFault => {
            InstructionTreatment::temporary(Exit, ExitBehavior::Fault)
        }
        Instr::Unreachable => InstructionTreatment::dedicated(Exit, ExitBehavior::Fault)
            .with_fault_stack(FaultStack::Before),
        Instr::Extended(operation) => extended_treatment(*operation),
    }
}

fn numeric_treatment(operation: NumericInstr) -> InstructionTreatment {
    use TreatmentClass::{FastPath, Helper, Inline};

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
        NumericInstr::SbAppendFloat => temporary(FastPath),
        NumericInstr::BytesBitAnd
        | NumericInstr::BytesBitOr
        | NumericInstr::BytesBitXor
        | NumericInstr::BytesBitNot
        | NumericInstr::TextParseFloatStatus
        | NumericInstr::TextParseFloatValue
        | NumericInstr::FloatFixed => temporary(Helper),
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
        ExtendedInstr::MapGet { .. }
        | ExtendedInstr::MapNextIndex
        | ExtendedInstr::MapKeyAt
        | ExtendedInstr::MapValueAt
        | ExtendedInstr::MapRemove { .. }
        | ExtendedInstr::MapClear
        | ExtendedInstr::MapReserve
        | ExtendedInstr::MapProbe
        | ExtendedInstr::MapProbeFound
        | ExtendedInstr::MapProbeKey
        | ExtendedInstr::MapProbeValue
        | ExtendedInstr::MapProbeSetValue
        | ExtendedInstr::MapProbeRemove
        | ExtendedInstr::MapInsertHashed
        | ExtendedInstr::MapWriteGuard => temporary(Helper),
        ExtendedInstr::ListEpoch
        | ExtendedInstr::ListIterLen
        | ExtendedInstr::SealInstance
        | ExtendedInstr::ListCapacity => dedicated(Guarded).with_replay(),
        ExtendedInstr::ListSet => dedicated(Guarded)
            .with_replay()
            .with_fault_stack(FaultStack::Pop(3)),
        ExtendedInstr::MapEpoch | ExtendedInstr::MapIterLen => dedicated(Guarded).with_replay(),
        ExtendedInstr::ListPop { .. }
        | ExtendedInstr::ListInsert
        | ExtendedInstr::ListRemove
        | ExtendedInstr::ListSwapRemove
        | ExtendedInstr::ListTruncate => temporary(FastPath),
        ExtendedInstr::ListReserve => dedicated(FastPath)
            .with_replay()
            .with_fault_stack(FaultStack::Pop(2)),
        ExtendedInstr::ListReorder => dedicated(FastPath)
            .with_replay()
            .with_fault_stack(FaultStack::Pop(1)),
        ExtendedInstr::ListContains => temporary(Helper),
        ExtendedInstr::CallSlot { .. } => InstructionTreatment::temporary(Call, ExitBehavior::Call),
        ExtendedInstr::NewSlot { .. } => {
            InstructionTreatment::temporary(Call, ExitBehavior::Allocation)
        }
        ExtendedInstr::LoadSlot { .. } => temporary(Guarded),
        ExtendedInstr::SendSlot { .. } | ExtendedInstr::PrepareWait { .. } => {
            InstructionTreatment::temporary(Exit, ExitBehavior::Effect)
        }
        ExtendedInstr::SyntaxKind
        | ExtendedInstr::SyntaxCategory
        | ExtendedInstr::SyntaxRangeStart
        | ExtendedInstr::SyntaxRangeEnd => temporary(Inline),
        ExtendedInstr::SyntaxTreeRoot
        | ExtendedInstr::SyntaxText
        | ExtendedInstr::SyntaxChildren
        | ExtendedInstr::SyntaxDetach
        | ExtendedInstr::DynRender
        | ExtendedInstr::CodeSource { .. }
        | ExtendedInstr::CodeDefinition
        | ExtendedInstr::FaultSite { .. }
        | ExtendedInstr::FaultTrace { .. } => temporary(Helper),
        ExtendedInstr::DynPack { .. }
        | ExtendedInstr::SyntaxBuildToken
        | ExtendedInstr::SyntaxBuildTrivia
        | ExtendedInstr::SyntaxBuildNode
        | ExtendedInstr::SyntaxToTree
        | ExtendedInstr::FunctionCode { .. }
        | ExtendedInstr::ClassCode { .. } => temporary(FastPath),
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
        NativeInstr::SbNew
        | NativeInstr::SbAppendStr
        | NativeInstr::SbAppendInt
        | NativeInstr::SbAppendBool
        | NativeInstr::SbBuild
        | NativeInstr::SbAppendChar
        | NativeInstr::SbFinish
        | NativeInstr::BbNew
        | NativeInstr::BbAppend
        | NativeInstr::BbBuild
        | NativeInstr::BbFinish
        | NativeInstr::BytesNew
        | NativeInstr::SbClear
        | NativeInstr::BbExtend
        | NativeInstr::BbReserve
        | NativeInstr::BbClear
        | NativeInstr::BytesSlice
        | NativeInstr::BytesConcat
        | NativeInstr::BytesCompact
        | NativeInstr::BytesTextView => temporary(FastPath),
        NativeInstr::SbByteLen | NativeInstr::SbLen | NativeInstr::BbLen | NativeInstr::BbAt => {
            temporary(Guarded)
        }
        NativeInstr::EqStr
        | NativeInstr::NeStr
        | NativeInstr::StrConcat
        | NativeInstr::StrStartsWith
        | NativeInstr::StrEndsWith
        | NativeInstr::StrContains
        | NativeInstr::StrFindIndex
        | NativeInstr::TextFindByteIndex
        | NativeInstr::TextTrim
        | NativeInstr::TextTrimStart
        | NativeInstr::TextTrimEnd
        | NativeInstr::TextToLowerAscii
        | NativeInstr::TextToUpperAscii
        | NativeInstr::TextReplace
        | NativeInstr::TextParseIntStatus
        | NativeInstr::TextParseIntValue
        | NativeInstr::TextPadStart
        | NativeInstr::TextPadEnd
        | NativeInstr::BytesEndsWith
        | NativeInstr::BytesContains
        | NativeInstr::TextSplit
        | NativeInstr::TextLines
        | NativeInstr::TextSlice
        | NativeInstr::TextSliceBytes
        | NativeInstr::TextBytes
        | NativeInstr::TextLt
        | NativeInstr::TextLe
        | NativeInstr::TextGt
        | NativeInstr::TextGe
        | NativeInstr::TextToString
        | NativeInstr::BytesText
        | NativeInstr::BbFindFrom
        | NativeInstr::BytesStartsWith
        | NativeInstr::BytesFindIndex
        | NativeInstr::BytesHex
        | NativeInstr::BytesIsUtf8
        | NativeInstr::EqBytes
        | NativeInstr::NeBytes
        | NativeInstr::LtBytes
        | NativeInstr::LeBytes
        | NativeInstr::GtBytes
        | NativeInstr::GeBytes
        | NativeInstr::TextHash
        | NativeInstr::BytesHash => temporary(Helper),
    }
}
