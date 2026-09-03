use super::*;
use crate::plan::{
    compute_dirty_locals, compute_liveness, split_segments, RegionPlan, Segment, SegmentExit,
};
use lm_bytecode::{BcClass, BcClassKind, BcType, Func, Instr, Module, NativeInstr, NO_PARENT};
use lm_heap::{Heap, JitHeapView, Object, SharedBytes};
use lm_value::{Value, ValueTag, Witness};

fn module(blocks: Vec<Vec<Instr>>) -> Module {
    Module {
        strings: vec![],
        bytes: vec![],
        types: vec![BcType::Unit, BcType::Bool, BcType::Int, BcType::Str],
        selectors: vec![],
        apps: vec![],
        interfaces: vec![],
        conformances: vec![],
        class_bounds: vec![],
        func_bounds: vec![vec![]],
        classes: vec![],
        funcs: vec![Func {
            name: "main".to_string(),
            param_names: vec![],
            type_params: 0,
            effect_params: 0,
            params: vec![],
            param_muts: vec![],
            ret: 2,
            row: vec![],
            captures: vec![],
            local_types: vec![2, 2],
            blocks,
        }],
        imports: vec![],
        slots: vec![],
        core_roles: [lm_bytecode::NO_ROLE; lm_bytecode::CORE_ROLE_COUNT],
        entry: 0,
        exports: vec![],
        bindings: vec![],
        debug: vec![],
    }
}

#[test]
fn opcode_ledger_separates_class_and_exit() {
    use lm_bytecode::{Instr, NumericInstr};

    let field = instruction_treatment(&Instr::LoadField(0));
    assert_eq!(field.class(), TreatmentClass::Guarded);
    assert_eq!(field.exit(), ExitBehavior::Continue);

    let call = instruction_treatment(&Instr::CallInterface {
        site: 0,
        recv_ty: 0,
        app: lm_bytecode::NO_APP,
    });
    assert_eq!(call.class(), TreatmentClass::Call);
    assert_eq!(call.exit(), ExitBehavior::Call);
    assert!(call.replays());

    let helper = instruction_treatment(&Instr::Numeric(NumericInstr::BytesBitAnd));
    assert_eq!(helper.class(), TreatmentClass::Helper);
    assert_eq!(helper.exit(), ExitBehavior::Allocation);

    let fault = instruction_treatment(&Instr::RaiseFault);
    assert_eq!(fault.class(), TreatmentClass::Exit);
    assert_eq!(fault.exit(), ExitBehavior::Fault);
}

#[test]
fn dynamic_boundary_batch_has_only_dedicated_treatments() {
    use lm_bytecode::ExtendedInstr;

    let operations = [
        Instr::TableEdit {
            action: 0,
            kind: 0,
            slot: 0,
        },
        Instr::RequestOp,
        Instr::AsCall { op: 0, ty: 0 },
        Instr::CallArgs,
        Instr::FaultCode,
        Instr::FaultDenied,
        Instr::RaiseUserPanic,
        Instr::RaiseAssertionFailed,
        Instr::RaiseFault,
        Instr::Extended(ExtendedInstr::CallSlot { slot: 0, app: 0 }),
        Instr::Extended(ExtendedInstr::NewSlot { slot: 0, app: 0 }),
        Instr::Extended(ExtendedInstr::LoadSlot { slot: 0 }),
        Instr::Extended(ExtendedInstr::SendSlot { slot: 0 }),
        Instr::Extended(ExtendedInstr::prepare_wait(0, 0, 0).expect("the wait instruction fits")),
    ];
    assert!(operations
        .iter()
        .all(|operation| instruction_treatment(operation).class() != TreatmentClass::Inline));
}

#[test]
fn syntax_dynamic_and_code_batch_has_production_treatments() {
    use lm_bytecode::ExtendedInstr;

    let helpers = [
        ExtendedInstr::SyntaxTreeRoot,
        ExtendedInstr::SyntaxKind,
        ExtendedInstr::SyntaxCategory,
        ExtendedInstr::SyntaxRangeStart,
        ExtendedInstr::SyntaxRangeEnd,
        ExtendedInstr::SyntaxText,
        ExtendedInstr::SyntaxChildren,
        ExtendedInstr::SyntaxDetach,
        ExtendedInstr::DynPack { ty: 0 },
        ExtendedInstr::SyntaxBuildToken,
        ExtendedInstr::SyntaxBuildTrivia,
        ExtendedInstr::SyntaxBuildNode,
        ExtendedInstr::SyntaxToTree,
    ];
    assert!(helpers.iter().all(|operation| {
        instruction_treatment(&Instr::Extended(*operation)).class() == TreatmentClass::Helper
    }));

    let boundaries = [
        ExtendedInstr::DynRender,
        ExtendedInstr::FunctionCode { func: 0 },
        ExtendedInstr::ClassCode { class: 0 },
        ExtendedInstr::CodeSource { ty: 0 },
        ExtendedInstr::CodeDefinition,
        ExtendedInstr::FaultSite { ty: 0 },
        ExtendedInstr::FaultTrace { ty: 0 },
    ];
    assert!(boundaries.iter().all(|operation| {
        let treatment = instruction_treatment(&Instr::Extended(*operation));
        treatment.class() == TreatmentClass::Exit && treatment.exit() == ExitBehavior::Boundary
    }));
}

#[test]
fn segments_split_conditional_fallthrough() {
    let module = module(vec![
        vec![Instr::ConstInt(0), Instr::StoreLocal(0), Instr::Jump(1)],
        vec![
            Instr::LoadLocal(0),
            Instr::ConstInt(10),
            Instr::LtInt,
            Instr::JumpIfFalse(3),
            Instr::Jump(2),
        ],
        vec![
            Instr::LoadLocal(0),
            Instr::ConstInt(1),
            Instr::Add,
            Instr::StoreLocal(0),
            Instr::Jump(1),
        ],
        vec![Instr::LoadLocal(0), Instr::Return],
    ]);
    lm_verify::verify_module(&module).expect("the loop verifies");
    let segments = split_segments(&module.funcs[0]).expect("the loop splits");
    assert_eq!(segments.len(), 5);
    assert_eq!((segments[1].block, segments[1].start), (1, 0));
    assert_eq!((segments[2].block, segments[2].start), (1, 4));

    let bundle = lm_abi::standard_bundle();
    let plan = RegionPlan::for_function(&FunctionInput::new(
        0,
        &module.funcs[0],
        &module,
        &bundle,
        0,
    ))
    .expect("the loop plan builds");
    let reserves: Vec<u32> = plan
        .segments
        .iter()
        .map(|segment| segment.fuel_reserve)
        .collect();
    assert_eq!(reserves, vec![13, 10, 6, 5, 2]);
    let reserved_prefixes: Vec<u32> = plan
        .segments
        .iter()
        .map(|segment| segment.reserved_prefix_cost)
        .collect();
    assert_eq!(reserved_prefixes, vec![0, 0, 4, 5, 4]);
    let carried_prefixes: Vec<bool> = plan
        .segments
        .iter()
        .map(|segment| segment.carries_reserved_prefix)
        .collect();
    assert_eq!(carried_prefixes, vec![false, false, true, true, true]);
    assert_eq!(plan.segments[1].carry_reserved_cost, vec![true, true]);
    assert_eq!(plan.segments[3].carry_reserved_cost, vec![false]);
}

#[test]
fn replay_barriers_end_the_current_segment() {
    let module = module(vec![vec![
        Instr::StoreField(0),
        Instr::LoadField(0),
        Instr::Return,
    ]]);
    let segments = split_segments(&module.funcs[0]).expect("the field segment splits");
    assert_eq!(segments.len(), 2);
    assert_eq!((segments[0].start, segments[0].end), (0, 1));
    assert_eq!((segments[1].start, segments[1].end), (1, 3));
    assert!(instruction_treatment(&Instr::StoreField(0)).is_replay_barrier());
    assert!(!instruction_treatment(&Instr::LoadField(0)).is_replay_barrier());
}

fn counting_loop_region() -> Arc<CompiledRegion> {
    let module = module(vec![
        vec![Instr::ConstInt(0), Instr::StoreLocal(0), Instr::Jump(1)],
        vec![
            Instr::LoadLocal(0),
            Instr::ConstInt(10),
            Instr::LtInt,
            Instr::JumpIfFalse(3),
            Instr::Jump(2),
        ],
        vec![
            Instr::LoadLocal(0),
            Instr::ConstInt(1),
            Instr::Add,
            Instr::StoreLocal(0),
            Instr::Jump(1),
        ],
        vec![Instr::LoadLocal(0), Instr::Return],
    ]);
    let bundle = lm_abi::standard_bundle();
    lm_verify::verify_module_with_bundle(&module, &bundle).expect("the poll loop verifies");
    JitEngine::default()
        .compile(FunctionInput::new(0, &module.funcs[0], &module, &bundle, 0))
        .expect("the poll loop compiles")
}

fn execute_counting_loop(
    region: &CompiledRegion,
    activation: &mut NativeActivation,
    fuel: u64,
    poll: NativePoll<'_>,
) -> ExecutionExit {
    activation
        .prepare_root(NativePreparation {
            function: 0,
            environment: 0,
            capture_tag: ValueTag::Uninit as u64,
            capture_bits: 0,
            capture_data: 0,
            capture_len: 0,
            block: 0,
            instruction: 0,
            local_count: 2,
            max_stack: region.max_stack(),
            operand_len: 0,
            scalar_limit: 4_096,
            frame_limit: 256,
        })
        .expect("the poll root prepares");
    let mut runtime = TestRuntime {
        heap: Heap::new(1 << 20),
    };
    let mut roots = vec![0; region.max_roots().max(1)];
    let mut root_tags = vec![0; region.max_roots().max(1)];
    let mut root_states = vec![0; region.max_roots().max(1)];
    region
        .execute(
            &mut runtime,
            activation,
            NativeExecution {
                entry: 0,
                entries: &[],
                base_stack_values: 0,
                max_stack_values: 4_096,
                base_frames: 0,
                max_frames: 256,
                roots: &mut roots,
                root_tags: &mut root_tags,
                root_states: &mut root_states,
                fuel,
                poll,
                heap: JitHeapView::EMPTY,
                class_parents: &[],
                dispatch_rows: &[],
                dispatch_methods: &[],
                literals: NativeLiteralView::EMPTY,
                type_store_id: 1,
                type_environments: NativeTypeEnvironmentView::EMPTY,
                resolved_calls: NativeResolvedCallView::EMPTY,
                image_slots: NativeImageSlotView::EMPTY,
            },
        )
        .expect("the poll loop executes")
}

#[test]
fn idle_native_polls_keep_the_region_active() {
    let region = counting_loop_region();
    let mut expected_activation = NativeActivation::default();
    let expected = execute_counting_loop(
        region.as_ref(),
        &mut expected_activation,
        1_000,
        NativePoll::disabled(),
    );
    let requested = std::sync::atomic::AtomicU32::new(0);
    let mut actual_activation = NativeActivation::default();
    let actual = execute_counting_loop(
        region.as_ref(),
        &mut actual_activation,
        1_000,
        NativePoll::new(&requested, 3, 4),
    );
    assert_eq!(actual.kind(), ExitKind::Return);
    assert_eq!(actual.retired(), expected.retired());
    assert_eq!(actual.result(), expected.result());
    assert_eq!(
        actual_activation.root_buffers(),
        expected_activation.root_buffers()
    );
}

#[test]
fn requested_native_poll_stops_at_a_segment_boundary() {
    let region = counting_loop_region();
    let requested = std::sync::atomic::AtomicU32::new(1);
    let mut actual_activation = NativeActivation::default();
    let actual = execute_counting_loop(
        region.as_ref(),
        &mut actual_activation,
        1_000,
        NativePoll::new(&requested, 5, 7),
    );
    assert_eq!(actual.kind(), ExitKind::Poll);
    assert!(actual.retired() >= 5);
    let entry = region
        .entry_plan(actual.block(), actual.instruction())
        .expect("the poll stops at a native segment entry");
    assert_eq!(actual.stack_len() as usize, entry.operand_kinds().len());
}

#[test]
fn poll_schedule_keeps_one_phase_at_boundaries() {
    let schedule = PollSchedule::new(3, 4);
    assert_eq!(schedule.remaining_after(0), 3);
    assert_eq!(schedule.remaining_after(1), 2);
    assert_eq!(schedule.remaining_after(2), 1);
    assert_eq!(schedule.remaining_after(3), 4);
    assert_eq!(schedule.remaining_after(4), 3);
    assert_eq!(schedule.remaining_after(7), 4);
    assert!(!schedule.due_at(2));
    assert!(schedule.due_at(3));
    assert!(schedule.due_at(7));
}

#[test]
fn liveness_ignores_a_local_replaced_before_use() {
    let mut segments = vec![analysis_segment(
        vec![false, true],
        vec![true, false],
        vec![],
    )];
    compute_liveness(&mut segments, 2);
    assert_eq!(segments[0].live_in, vec![false, true]);
}

#[test]
fn dirty_locals_follow_merges_and_loops() {
    let mut segments = vec![
        analysis_segment(vec![false; 3], vec![true, false, false], vec![1, 2]),
        analysis_segment(vec![false; 3], vec![false, true, false], vec![3]),
        analysis_segment(vec![false; 3], vec![false, false, false], vec![3]),
        analysis_segment(vec![false; 3], vec![false, false, false], vec![1]),
    ];
    compute_dirty_locals(&mut segments, 3);
    assert_eq!(segments[0].dirty_locals, vec![true, false, false]);
    assert_eq!(segments[1].dirty_locals, vec![true, true, false]);
    assert_eq!(segments[2].dirty_locals, vec![true, false, false]);
    assert_eq!(segments[3].dirty_locals, vec![true, true, false]);
}

fn analysis_segment(uses: Vec<bool>, definitions: Vec<bool>, successors: Vec<usize>) -> Segment {
    Segment {
        block: 0,
        start: 0,
        end: 3,
        cost: 3,
        fuel_reserve: 3,
        reserved_prefix_cost: 0,
        carry_reserved_cost: vec![],
        carries_reserved_prefix: false,
        retry_entry: false,
        defer_integer_overflow: false,
        exit: SegmentExit::Return,
        uses,
        definitions,
        successors,
        live_in: vec![],
        dirty_locals: vec![],
        entry_stack: vec![],
        virtual_locals_in: vec![],
        virtual_stack_in: vec![],
        virtual_barriers: vec![],
        call_contract: None,
        exit_stack: vec![],
        boundary_stack: vec![],
        heap_accesses: vec![],
        option_accesses: vec![],
        fuel_stacks: vec![],
        replay_stacks: vec![],
        fault_stacks: vec![],
        allocations: vec![],
    }
}

#[test]
fn unreachable_code_uses_one_native_fault_exit() {
    let module = module(vec![vec![Instr::Unreachable]]);
    let bundle = lm_abi::standard_bundle();
    lm_verify::verify_module_with_bundle(&module, &bundle).expect("the function verifies");
    let region = JitEngine::default()
        .compile(FunctionInput::new(0, &module.funcs[0], &module, &bundle, 0))
        .expect("the terminal function compiles");
    assert!(region.code_size() > 0);
    assert_eq!(region.plan.interpreter_sites, 0);
    assert_eq!(region.plan.segments.len(), 1);
    let mut runtime = TestRuntime {
        heap: Heap::new(1 << 20),
    };
    let mut activation = NativeActivation::default();
    activation
        .prepare_root(NativePreparation {
            function: 0,
            environment: 0,
            capture_tag: ValueTag::Uninit as u64,
            capture_bits: 0,
            capture_data: 0,
            capture_len: 0,
            block: 0,
            instruction: 0,
            local_count: 2,
            max_stack: region.max_stack(),
            operand_len: 0,
            scalar_limit: 4_096,
            frame_limit: 256,
        })
        .expect("the terminal root prepares");
    let mut roots = vec![0; region.max_roots().max(1)];
    let mut root_tags = vec![0; region.max_roots().max(1)];
    let mut root_states = vec![0; region.max_roots().max(1)];
    let exit = region
        .execute(
            &mut runtime,
            &mut activation,
            NativeExecution {
                entry: 0,
                entries: &[],
                base_stack_values: 0,
                max_stack_values: 4_096,
                base_frames: 0,
                max_frames: 256,
                roots: &mut roots,
                root_tags: &mut root_tags,
                root_states: &mut root_states,
                fuel: 1,
                poll: NativePoll::disabled(),
                heap: JitHeapView::EMPTY,
                class_parents: &[],
                dispatch_rows: &[],
                dispatch_methods: &[],
                literals: NativeLiteralView::EMPTY,
                type_store_id: 1,
                type_environments: NativeTypeEnvironmentView::EMPTY,
                resolved_calls: NativeResolvedCallView::EMPTY,
                image_slots: NativeImageSlotView::EMPTY,
            },
        )
        .expect("the terminal function executes");
    assert_eq!(exit.kind(), ExitKind::Unreachable);
    assert_eq!(exit.retired(), 1);
    assert_eq!((exit.block(), exit.instruction()), (0, 1));
}

fn field_module() -> Module {
    let mut module = module(vec![vec![
        Instr::LoadLocal(0),
        Instr::LoadField(0),
        Instr::Return,
    ]]);
    module.types.push(BcType::Class(0));
    module.classes.push(BcClass {
        name: "Pair".to_string(),
        key: "Pair".to_string(),
        is_final: true,
        is_frozen: false,
        parent: NO_PARENT,
        parent_args: vec![],
        type_params: 0,
        kind: BcClassKind::Normal,
        fields: vec![("left".to_string(), 2)],
        methods: vec![],
        field_defaults: vec![false],
        own_start: 0,
        has_init: false,
    });
    module.class_bounds.push(vec![]);
    module.funcs[0].param_names = vec!["pair".to_string()];
    module.funcs[0].params = vec![4];
    module.funcs[0].param_muts = vec![false];
    module.funcs[0].local_types = vec![4];
    module.funcs.push(Func {
        name: "main".to_string(),
        param_names: vec![],
        type_params: 0,
        effect_params: 0,
        params: vec![],
        param_muts: vec![],
        ret: 2,
        row: vec![],
        captures: vec![],
        local_types: vec![],
        blocks: vec![vec![Instr::ConstInt(0), Instr::Return]],
    });
    module.func_bounds.push(vec![]);
    module.entry = 1;
    module
}

fn field_store_module() -> Module {
    let mut module = field_module();
    module.funcs[0].param_names = vec!["pair".to_string(), "value".to_string()];
    module.funcs[0].params = vec![4, 2];
    module.funcs[0].param_muts = vec![true, false];
    module.funcs[0].ret = 0;
    module.funcs[0].local_types = vec![4, 2];
    module.funcs[0].blocks = vec![vec![
        Instr::LoadLocal(0),
        Instr::LoadLocal(1),
        Instr::StoreField(0),
        Instr::ConstUnit,
        Instr::Return,
    ]];
    module
}

fn bytes_get_module() -> Module {
    let mut module = module(vec![vec![
        Instr::LoadLocal(0),
        Instr::LoadLocal(1),
        Instr::Native(NativeInstr::BytesGet),
        Instr::Return,
    ]]);
    module.types.push(BcType::Bytes);
    module.funcs[0].param_names = vec!["bytes".to_string(), "index".to_string()];
    module.funcs[0].params = vec![4, 2];
    module.funcs[0].param_muts = vec![false, false];
    module.funcs[0].local_types = vec![4, 2];
    module.funcs.push(Func {
        name: "main".to_string(),
        param_names: vec![],
        type_params: 0,
        effect_params: 0,
        params: vec![],
        param_muts: vec![],
        ret: 2,
        row: vec![],
        captures: vec![],
        local_types: vec![],
        blocks: vec![vec![Instr::ConstInt(0), Instr::Return]],
    });
    module.func_bounds.push(vec![]);
    module.entry = 1;
    module
}

struct TestRuntime {
    heap: Heap,
}

macro_rules! interpreter_heap_operations {
    ($($name:ident),+ $(,)?) => {
        $(
            fn $name(&mut self, _request: HeapOperationRequest<'_>) -> HeapOperationResult {
                HeapOperationResult::Interpreter
            }
        )+
    };
}

impl NativeRuntime for TestRuntime {
    fn allocate_instance(
        &mut self,
        _class: u32,
        _environment: u32,
        _roots: NativeRoots<'_>,
        _allow_collection: bool,
    ) -> AllocationResult {
        AllocationResult::Interpreter
    }

    fn allocate_closure(&mut self, _request: ClosureAllocationRequest<'_>) -> AllocationResult {
        AllocationResult::Interpreter
    }

    fn allocate_callback(
        &mut self,
        _request: CallbackAllocationRequest<'_>,
    ) -> CallbackAllocationResult {
        CallbackAllocationResult::Interpreter
    }

    fn allocate_tuple(&mut self, _request: ValueArrayAllocationRequest<'_>) -> AllocationResult {
        AllocationResult::Interpreter
    }

    fn allocate_list(&mut self, _request: ValueArrayAllocationRequest<'_>) -> AllocationResult {
        AllocationResult::Interpreter
    }

    fn allocate_map(&mut self, _request: ValueArrayAllocationRequest<'_>) -> AllocationResult {
        AllocationResult::Interpreter
    }

    fn grow_list(&mut self, _request: ListGrowthRequest<'_>) -> ListGrowthResult {
        ListGrowthResult::Interpreter
    }

    fn insert_list(&mut self, _request: ListInsertRequest<'_>) -> ListGrowthResult {
        ListGrowthResult::Interpreter
    }

    fn reserve_list(&mut self, _request: CollectionReserveRequest<'_>) -> CollectionReserveResult {
        CollectionReserveResult::Interpreter
    }

    fn reserve_map(&mut self, _request: CollectionReserveRequest<'_>) -> CollectionReserveResult {
        CollectionReserveResult::Interpreter
    }

    fn list_contains(
        &mut self,
        _reference: u64,
        _value_bits: u64,
        _value_tag: u64,
    ) -> RuntimeValueResult {
        RuntimeValueResult::Interpreter
    }

    fn map_has(&mut self, _reference: u64, _key_bits: u64, _key_tag: u64) -> RuntimeValueResult {
        RuntimeValueResult::Interpreter
    }

    fn map_at(&mut self, _reference: u64, _key_bits: u64, _key_tag: u64) -> RuntimeValueResult {
        RuntimeValueResult::Interpreter
    }

    fn map_get(&mut self, _reference: u64, _key_bits: u64, _key_tag: u64) -> RuntimeValueResult {
        RuntimeValueResult::Interpreter
    }

    fn map_next_index(
        &mut self,
        _reference: u64,
        _cursor: u64,
        _expected: u64,
    ) -> RuntimeValueResult {
        RuntimeValueResult::Interpreter
    }

    fn map_key_at(&mut self, _reference: u64, _index: u64) -> RuntimeValueResult {
        RuntimeValueResult::Interpreter
    }

    fn map_value_at(&mut self, _reference: u64, _index: u64) -> RuntimeValueResult {
        RuntimeValueResult::Interpreter
    }

    fn map_remove(&mut self, _reference: u64, _key_bits: u64, _key_tag: u64) -> RuntimeValueResult {
        RuntimeValueResult::Interpreter
    }

    fn map_clear(&mut self, _reference: u64) -> RuntimeValueResult {
        RuntimeValueResult::Interpreter
    }

    fn map_probe(&mut self, _reference: u64, _semantic: u64, _prior: u64) -> RuntimeValueResult {
        RuntimeValueResult::Interpreter
    }

    fn map_probe_key(&mut self, _reference: u64, _token: u64) -> RuntimeValueResult {
        RuntimeValueResult::Interpreter
    }

    fn map_probe_value(&mut self, _reference: u64, _token: u64) -> RuntimeValueResult {
        RuntimeValueResult::Interpreter
    }

    fn map_probe_set_value(
        &mut self,
        _reference: u64,
        _token: u64,
        _value_bits: u64,
        _value_tag: u64,
    ) -> RuntimeValueResult {
        RuntimeValueResult::Interpreter
    }

    fn map_probe_remove(&mut self, _reference: u64, _token: u64) -> RuntimeValueResult {
        RuntimeValueResult::Interpreter
    }

    fn map_insert_hashed(&mut self, _request: MapInsertHashedRequest<'_>) -> RuntimeUnitResult {
        RuntimeUnitResult::Interpreter
    }

    fn map_put_probe(
        &mut self,
        _reference: u64,
        _key_bits: u64,
        _key_tag: u64,
    ) -> MapPutProbeResult {
        MapPutProbeResult::Interpreter
    }

    fn map_put_discard(&mut self, _request: MapPutDiscardRequest<'_>) -> RuntimeUnitResult {
        RuntimeUnitResult::Interpreter
    }

    fn map_put_commit(&mut self, _request: MapPutCommitRequest<'_>) -> RuntimeUnitResult {
        RuntimeUnitResult::Interpreter
    }

    fn map_intern_text_range(
        &mut self,
        _request: MapInternTextRangeRequest<'_>,
    ) -> HeapOperationResult {
        HeapOperationResult::Interpreter
    }

    interpreter_heap_operations!(
        fault_code,
        fault_denied,
        dyn_pack,
        syntax_tree_root,
        syntax_kind,
        syntax_category,
        syntax_range_start,
        syntax_range_end,
        syntax_text,
        syntax_children,
        syntax_detach,
        syntax_build_token,
        syntax_build_trivia,
        syntax_build_node,
        syntax_to_tree,
        string_builder_new,
        string_builder_append_text,
        string_builder_append_int,
        string_builder_append_bool,
        string_builder_append_char,
        string_builder_append_float,
        string_builder_build,
        string_builder_finish,
        byte_buffer_new,
        byte_buffer_append,
        byte_buffer_build,
        byte_buffer_extend,
        byte_buffer_reserve,
        byte_buffer_finish,
        bytes_from_text,
        bytes_slice,
        bytes_concat,
        bytes_compact,
        bytes_text_view,
        bytes_bit_and,
        bytes_bit_or,
        bytes_bit_xor,
        bytes_bit_not,
        text_concat,
        text_starts_with,
        text_ends_with,
        text_contains,
        text_find_scalar,
        text_find_byte,
        text_trim,
        text_trim_start,
        text_trim_end,
        text_lower_ascii,
        text_upper_ascii,
        text_replace,
        text_parse_int_status,
        text_parse_int_value,
        text_pad_start,
        text_pad_end,
        bytes_ends_with,
        bytes_contains,
        text_split,
        text_lines,
        text_slice,
        text_slice_bytes,
        text_bytes,
        text_to_string,
        bytes_text,
        bytes_text_range,
        byte_buffer_find_from,
        bytes_starts_with,
        bytes_find_index,
        bytes_hex,
        bytes_is_utf8,
        text_parse_float_status,
        text_parse_float_value,
        float_fixed,
        regex_compile_status,
        regex_compile_value,
        regex_source,
        regex_is_match,
        regex_captures,
        regex_count,
        regex_split,
        regex_replace_all,
        regex_match_start,
        regex_match_end,
        regex_match_text,
        regex_match_group_count,
        regex_match_group,
        regex_match_named,
    );

    fn values_equal(
        &mut self,
        _left_bits: u64,
        _left_tag: u64,
        _right_bits: u64,
        _right_tag: u64,
    ) -> RuntimeValueResult {
        RuntimeValueResult::Interpreter
    }

    fn compare_text(&mut self, _left: u64, _right: u64) -> RuntimeValueResult {
        RuntimeValueResult::Interpreter
    }

    fn compare_bytes(&mut self, _left: u64, _right: u64) -> RuntimeValueResult {
        RuntimeValueResult::Interpreter
    }

    fn hash_text(&mut self, _reference: u64) -> RuntimeValueResult {
        RuntimeValueResult::Interpreter
    }

    fn hash_bytes(&mut self, _reference: u64) -> RuntimeValueResult {
        RuntimeValueResult::Interpreter
    }

    fn freeze_graph(&mut self, _reference: u64) -> RuntimeValueResult {
        RuntimeValueResult::Interpreter
    }

    fn digest_value(&mut self, _request: DigestRequest<'_>) -> AllocationResult {
        AllocationResult::Interpreter
    }
}

#[test]
fn native_safe_byte_reads_return_a_byte_or_minus_one() {
    let module = bytes_get_module();
    let bundle = lm_abi::standard_bundle();
    lm_verify::verify_module_with_bundle(&module, &bundle).expect("the safe byte read verifies");
    let region = JitEngine::default()
        .compile(FunctionInput::new(0, &module.funcs[0], &module, &bundle, 0))
        .expect("the safe byte read compiles");
    let mut runtime = TestRuntime {
        heap: Heap::new(1 << 20),
    };
    let reference = runtime
        .heap
        .alloc(Object::Bytes(SharedBytes::from(&[3, 5, 8])));
    let reference = u64::from(reference.slot) | (u64::from(reference.generation) << 32);

    for (index, expected) in [(1_i64, 5_i64), (-1, -1), (3, -1)] {
        let mut activation = NativeActivation::default();
        activation
            .prepare_root(NativePreparation {
                function: 0,
                environment: 0,
                capture_tag: ValueTag::Uninit as u64,
                capture_bits: 0,
                capture_data: 0,
                capture_len: 0,
                block: 0,
                instruction: 0,
                local_count: 2,
                max_stack: region.max_stack(),
                operand_len: 0,
                scalar_limit: 4_096,
                frame_limit: 256,
            })
            .expect("the safe byte root prepares");
        let (locals, tags, states, _, _) = activation.root_buffers_mut();
        locals[0] = reference;
        locals[1] = index as u64;
        tags[0] = ValueTag::Obj as u64;
        tags[1] = ValueTag::Int as u64;
        states[0] = LOCAL_INITIALIZED;
        states[1] = LOCAL_INITIALIZED;
        let mut roots = vec![0; region.max_roots().max(1)];
        let mut root_tags = vec![0; region.max_roots().max(1)];
        let mut root_states = vec![0; region.max_roots().max(1)];
        let heap = runtime.heap.jit_view();
        let exit = region
            .execute(
                &mut runtime,
                &mut activation,
                NativeExecution {
                    entry: 0,
                    entries: &[],
                    base_stack_values: 0,
                    max_stack_values: 4_096,
                    base_frames: 0,
                    max_frames: 256,
                    roots: &mut roots,
                    root_tags: &mut root_tags,
                    root_states: &mut root_states,
                    fuel: 4,
                    poll: NativePoll::disabled(),
                    heap,
                    class_parents: &[],
                    dispatch_rows: &[],
                    dispatch_methods: &[],
                    literals: NativeLiteralView::EMPTY,
                    type_store_id: 1,
                    type_environments: NativeTypeEnvironmentView::EMPTY,
                    resolved_calls: NativeResolvedCallView::EMPTY,
                    image_slots: NativeImageSlotView::EMPTY,
                },
            )
            .expect("the safe byte read executes");
        assert_eq!(exit.kind(), ExitKind::Return);
        assert_eq!(exit.retired(), 4);
        assert_eq!(exit.result() as i64, expected);
    }
}

#[test]
fn native_field_load_uses_the_direct_heap_view() {
    let module = field_module();
    let bundle = lm_abi::standard_bundle();
    lm_verify::verify_module_with_bundle(&module, &bundle).expect("the field load verifies");
    let engine = JitEngine::default();
    let region = engine
        .compile(FunctionInput::new(0, &module.funcs[0], &module, &bundle, 0))
        .expect("the field load compiles");
    let mut runtime = TestRuntime {
        heap: Heap::new(1 << 20),
    };
    let reference = runtime.heap.alloc(Object::Instance {
        class: 0,
        fields: vec![Value::Int(41)].into(),
        env: Witness::EMPTY,
    });
    let reference = u64::from(reference.slot) | (u64::from(reference.generation) << 32);
    let mut activation = NativeActivation::default();
    activation
        .prepare_root(NativePreparation {
            function: 0,
            environment: 0,
            capture_tag: ValueTag::Uninit as u64,
            capture_bits: 0,
            capture_data: 0,
            capture_len: 0,
            block: 0,
            instruction: 0,
            local_count: 1,
            max_stack: region.max_stack(),
            operand_len: 0,
            scalar_limit: 4_096,
            frame_limit: 256,
        })
        .expect("the native root prepares");
    let (locals, tags, states, _, _) = activation.root_buffers_mut();
    locals[0] = reference;
    tags[0] = ValueTag::Obj as u64;
    states[0] = LOCAL_INITIALIZED;
    let mut roots = vec![0; region.max_roots().max(1)];
    let mut root_tags = vec![0; region.max_roots().max(1)];
    let mut root_states = vec![0; region.max_roots().max(1)];
    let heap = runtime.heap.jit_view();
    let exit = region
        .execute(
            &mut runtime,
            &mut activation,
            NativeExecution {
                entry: 0,
                entries: &[],
                base_stack_values: 0,
                max_stack_values: 4_096,
                base_frames: 0,
                max_frames: 256,
                roots: &mut roots,
                root_tags: &mut root_tags,
                root_states: &mut root_states,
                fuel: 3,
                poll: NativePoll::disabled(),
                heap,
                class_parents: &[],
                dispatch_rows: &[],
                dispatch_methods: &[],
                literals: NativeLiteralView::EMPTY,
                type_store_id: 1,
                type_environments: NativeTypeEnvironmentView::EMPTY,
                resolved_calls: NativeResolvedCallView::EMPTY,
                image_slots: NativeImageSlotView::EMPTY,
            },
        )
        .expect("the field load executes");
    assert_eq!(exit.kind(), ExitKind::Return);
    assert_eq!(exit.retired(), 3);
    assert_eq!(exit.result(), 41);
}

#[test]
fn native_field_fault_keeps_the_exact_program_point() {
    let module = field_module();
    let bundle = lm_abi::standard_bundle();
    let engine = JitEngine::default();
    let region = engine
        .compile(FunctionInput::new(0, &module.funcs[0], &module, &bundle, 0))
        .expect("the field load compiles");
    let mut runtime = TestRuntime {
        heap: Heap::new(1 << 20),
    };
    let reference = runtime.heap.alloc(Object::Instance {
        class: 0,
        fields: vec![Value::Uninit].into(),
        env: Witness::EMPTY,
    });
    let reference = u64::from(reference.slot) | (u64::from(reference.generation) << 32);
    let mut activation = NativeActivation::default();
    activation
        .prepare_root(NativePreparation {
            function: 0,
            environment: 0,
            capture_tag: ValueTag::Uninit as u64,
            capture_bits: 0,
            capture_data: 0,
            capture_len: 0,
            block: 0,
            instruction: 0,
            local_count: 1,
            max_stack: region.max_stack(),
            operand_len: 0,
            scalar_limit: 4_096,
            frame_limit: 256,
        })
        .expect("the native root prepares");
    let (locals, tags, states, _, _) = activation.root_buffers_mut();
    locals[0] = reference;
    tags[0] = ValueTag::Obj as u64;
    states[0] = LOCAL_INITIALIZED;
    let mut roots = vec![0; region.max_roots().max(1)];
    let mut root_tags = vec![0; region.max_roots().max(1)];
    let mut root_states = vec![0; region.max_roots().max(1)];
    let heap = runtime.heap.jit_view();
    let exit = region
        .execute(
            &mut runtime,
            &mut activation,
            NativeExecution {
                entry: 0,
                entries: &[],
                base_stack_values: 0,
                max_stack_values: 4_096,
                base_frames: 0,
                max_frames: 256,
                roots: &mut roots,
                root_tags: &mut root_tags,
                root_states: &mut root_states,
                fuel: 3,
                poll: NativePoll::disabled(),
                heap,
                class_parents: &[],
                dispatch_rows: &[],
                dispatch_methods: &[],
                literals: NativeLiteralView::EMPTY,
                type_store_id: 1,
                type_environments: NativeTypeEnvironmentView::EMPTY,
                resolved_calls: NativeResolvedCallView::EMPTY,
                image_slots: NativeImageSlotView::EMPTY,
            },
        )
        .expect("the field fault executes");
    assert_eq!(exit.kind(), ExitKind::UninitializedField);
    assert_eq!(exit.retired(), 2);
    assert_eq!((exit.block(), exit.instruction()), (0, 2));
    assert_eq!(exit.stack_len(), 0);
}

#[test]
fn another_concrete_class_replays_from_the_segment_head() {
    let module = field_module();
    let bundle = lm_abi::standard_bundle();
    let engine = JitEngine::default();
    let region = engine
        .compile(FunctionInput::new(0, &module.funcs[0], &module, &bundle, 0))
        .expect("the field load compiles");
    let mut runtime = TestRuntime {
        heap: Heap::new(1 << 20),
    };
    let reference = runtime.heap.alloc(Object::Instance {
        class: 1,
        fields: vec![Value::Int(41)].into(),
        env: Witness::EMPTY,
    });
    let reference = u64::from(reference.slot) | (u64::from(reference.generation) << 32);
    let mut activation = NativeActivation::default();
    activation
        .prepare_root(NativePreparation {
            function: 0,
            environment: 0,
            capture_tag: ValueTag::Uninit as u64,
            capture_bits: 0,
            capture_data: 0,
            capture_len: 0,
            block: 0,
            instruction: 0,
            local_count: 1,
            max_stack: region.max_stack(),
            operand_len: 0,
            scalar_limit: 4_096,
            frame_limit: 256,
        })
        .expect("the native root prepares");
    let (locals, tags, states, _, _) = activation.root_buffers_mut();
    locals[0] = reference;
    tags[0] = ValueTag::Obj as u64;
    states[0] = LOCAL_INITIALIZED;
    let mut roots = vec![0; region.max_roots().max(1)];
    let mut root_tags = vec![0; region.max_roots().max(1)];
    let mut root_states = vec![0; region.max_roots().max(1)];
    let heap = runtime.heap.jit_view();
    let exit = region
        .execute(
            &mut runtime,
            &mut activation,
            NativeExecution {
                entry: 0,
                entries: &[],
                base_stack_values: 0,
                max_stack_values: 4_096,
                base_frames: 0,
                max_frames: 256,
                roots: &mut roots,
                root_tags: &mut root_tags,
                root_states: &mut root_states,
                fuel: 3,
                poll: NativePoll::disabled(),
                heap,
                class_parents: &[],
                dispatch_rows: &[],
                dispatch_methods: &[],
                literals: NativeLiteralView::EMPTY,
                type_store_id: 1,
                type_environments: NativeTypeEnvironmentView::EMPTY,
                resolved_calls: NativeResolvedCallView::EMPTY,
                image_slots: NativeImageSlotView::EMPTY,
            },
        )
        .expect("the field load executes");
    assert_eq!(exit.kind(), ExitKind::Replay);
    assert_eq!(exit.retired(), 0);
    assert_eq!((exit.block(), exit.instruction()), (0, 0));
}

#[test]
fn native_field_store_writes_the_canonical_value() {
    let module = field_store_module();
    let bundle = lm_abi::standard_bundle();
    lm_verify::verify_module_with_bundle(&module, &bundle).expect("the field store verifies");
    let region = JitEngine::default()
        .compile(FunctionInput::new(0, &module.funcs[0], &module, &bundle, 0))
        .expect("the field store compiles");
    let mut runtime = TestRuntime {
        heap: Heap::new(1 << 20),
    };
    let reference = runtime.heap.alloc(Object::Instance {
        class: 0,
        fields: vec![Value::Int(1)].into(),
        env: Witness::EMPTY,
    });
    let bits = u64::from(reference.slot) | (u64::from(reference.generation) << 32);
    let mut activation = NativeActivation::default();
    activation
        .prepare_root(NativePreparation {
            function: 0,
            environment: 0,
            capture_tag: ValueTag::Uninit as u64,
            capture_bits: 0,
            capture_data: 0,
            capture_len: 0,
            block: 0,
            instruction: 0,
            local_count: 2,
            max_stack: region.max_stack(),
            operand_len: 0,
            scalar_limit: 4_096,
            frame_limit: 256,
        })
        .expect("the native root prepares");
    let (locals, tags, states, _, _) = activation.root_buffers_mut();
    locals[0] = bits;
    locals[1] = 42;
    tags[0] = ValueTag::Obj as u64;
    tags[1] = ValueTag::Int as u64;
    states[0] = LOCAL_INITIALIZED;
    states[1] = LOCAL_INITIALIZED;
    let mut roots = vec![0; region.max_roots().max(1)];
    let mut root_tags = vec![0; region.max_roots().max(1)];
    let mut root_states = vec![0; region.max_roots().max(1)];
    let heap = runtime.heap.jit_view();
    let exit = region
        .execute(
            &mut runtime,
            &mut activation,
            NativeExecution {
                entry: 0,
                entries: &[],
                base_stack_values: 0,
                max_stack_values: 4_096,
                base_frames: 0,
                max_frames: 256,
                roots: &mut roots,
                root_tags: &mut root_tags,
                root_states: &mut root_states,
                fuel: 5,
                poll: NativePoll::disabled(),
                heap,
                class_parents: &[],
                dispatch_rows: &[],
                dispatch_methods: &[],
                literals: NativeLiteralView::EMPTY,
                type_store_id: 1,
                type_environments: NativeTypeEnvironmentView::EMPTY,
                resolved_calls: NativeResolvedCallView::EMPTY,
                image_slots: NativeImageSlotView::EMPTY,
            },
        )
        .expect("the field store executes");
    assert_eq!(exit.kind(), ExitKind::Return);
    assert_eq!(exit.retired(), 5);
    let Object::Instance { fields, .. } = runtime.heap.get(reference) else {
        panic!("the object remains an instance");
    };
    assert_eq!(fields[0], Value::Int(42));
}

#[test]
fn native_field_store_replays_from_the_segment_head() {
    let module = field_store_module();
    let bundle = lm_abi::standard_bundle();
    let region = JitEngine::default()
        .compile(FunctionInput::new(0, &module.funcs[0], &module, &bundle, 0))
        .expect("the field store compiles");
    let mut runtime = TestRuntime {
        heap: Heap::new(1 << 20),
    };
    let reference = runtime.heap.alloc(Object::Instance {
        class: 0,
        fields: vec![Value::Int(1)].into(),
        env: Witness::EMPTY,
    });
    runtime.heap.set_frozen(reference);
    let bits = u64::from(reference.slot) | (u64::from(reference.generation) << 32);
    let mut activation = NativeActivation::default();
    activation
        .prepare_root(NativePreparation {
            function: 0,
            environment: 0,
            capture_tag: ValueTag::Uninit as u64,
            capture_bits: 0,
            capture_data: 0,
            capture_len: 0,
            block: 0,
            instruction: 0,
            local_count: 2,
            max_stack: region.max_stack(),
            operand_len: 0,
            scalar_limit: 4_096,
            frame_limit: 256,
        })
        .expect("the native root prepares");
    let (locals, tags, states, _, _) = activation.root_buffers_mut();
    locals[0] = bits;
    locals[1] = 42;
    tags[0] = ValueTag::Obj as u64;
    tags[1] = ValueTag::Int as u64;
    states[0] = LOCAL_INITIALIZED;
    states[1] = LOCAL_INITIALIZED;
    let mut roots = vec![0; region.max_roots().max(1)];
    let mut root_tags = vec![0; region.max_roots().max(1)];
    let mut root_states = vec![0; region.max_roots().max(1)];
    let heap = runtime.heap.jit_view();
    let exit = region
        .execute(
            &mut runtime,
            &mut activation,
            NativeExecution {
                entry: 0,
                entries: &[],
                base_stack_values: 0,
                max_stack_values: 4_096,
                base_frames: 0,
                max_frames: 256,
                roots: &mut roots,
                root_tags: &mut root_tags,
                root_states: &mut root_states,
                fuel: 5,
                poll: NativePoll::disabled(),
                heap,
                class_parents: &[],
                dispatch_rows: &[],
                dispatch_methods: &[],
                literals: NativeLiteralView::EMPTY,
                type_store_id: 1,
                type_environments: NativeTypeEnvironmentView::EMPTY,
                resolved_calls: NativeResolvedCallView::EMPTY,
                image_slots: NativeImageSlotView::EMPTY,
            },
        )
        .expect("the field store executes");
    assert_eq!(exit.kind(), ExitKind::Replay);
    assert_eq!(exit.retired(), 0);
    assert_eq!((exit.block(), exit.instruction()), (0, 0));
    assert_eq!(exit.stack_len(), 0);
}
