use super::*;
use crate::plan::{compute_liveness, split_segments, Segment};
use lm_bytecode::{BcClass, BcClassKind, BcType, Func, Instr, Module, NO_PARENT};
use lm_heap::{Heap, Object};
use lm_value::{Value, Witness};

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
}

#[test]
fn liveness_ignores_a_local_replaced_before_use() {
    let mut segments = vec![Segment {
        block: 0,
        start: 0,
        end: 3,
        cost: 3,
        exit: SegmentExit::Return,
        uses: vec![false, true],
        definitions: vec![true, false],
        successors: vec![],
        live_in: vec![],
        entry_stack: vec![],
        exit_stack: vec![],
        boundary_stack: vec![],
        heap_accesses: vec![],
        fuel_stacks: vec![],
        replay_stacks: vec![],
        fault_stacks: vec![],
        allocations: vec![],
    }];
    compute_liveness(&mut segments, 2);
    assert_eq!(segments[0].live_in, vec![false, true]);
}

#[test]
fn an_instruction_without_a_dedicated_treatment_splits_the_region() {
    let module = module(vec![vec![Instr::Unreachable]]);
    let bundle = lm_abi::standard_bundle();
    lm_verify::verify_module_with_bundle(&module, &bundle).expect("the function verifies");
    let input = FunctionInput::new(0, &module.funcs[0], &module, &bundle, 0);
    RegionPlan::for_function(&input).expect("the mixed function plans");
    let engine = JitEngine::default();
    let region = engine
        .compile(FunctionInput::new(0, &module.funcs[0], &module, &bundle, 0))
        .expect("the mixed function compiles");
    assert_eq!(region.plan.interpreter_sites, 1);
    assert_eq!(region.plan.segments.len(), 1);
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

struct TestRuntime {
    heap: Heap,
}

impl AllocationRuntime for TestRuntime {
    fn allocate_instance(
        &mut self,
        _class: u32,
        _root_bits: &[u64],
        _root_states: &[u8],
        _allow_collection: bool,
    ) -> AllocationResult {
        AllocationResult::Interpreter
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
            block: 0,
            instruction: 0,
            local_count: 1,
            max_stack: region.max_stack(),
            operand_len: 0,
            scalar_limit: 4_096,
            frame_limit: 256,
        })
        .expect("the native root prepares");
    activation.root_buffers_mut().0[0] = reference;
    let mut roots = vec![0; region.max_roots().max(1)];
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
                root_states: &mut root_states,
                fuel: 3,
                heap,
                class_parents: &[],
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
            block: 0,
            instruction: 0,
            local_count: 1,
            max_stack: region.max_stack(),
            operand_len: 0,
            scalar_limit: 4_096,
            frame_limit: 256,
        })
        .expect("the native root prepares");
    activation.root_buffers_mut().0[0] = reference;
    let mut roots = vec![0; region.max_roots().max(1)];
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
                root_states: &mut root_states,
                fuel: 3,
                heap,
                class_parents: &[],
            },
        )
        .expect("the field fault executes");
    assert_eq!(exit.kind(), ExitKind::UninitializedField);
    assert_eq!(exit.retired(), 2);
    assert_eq!((exit.block(), exit.instruction()), (0, 2));
    assert_eq!(exit.stack_len(), 0);
}

#[test]
fn another_concrete_class_replays_the_field_instruction() {
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
            block: 0,
            instruction: 0,
            local_count: 1,
            max_stack: region.max_stack(),
            operand_len: 0,
            scalar_limit: 4_096,
            frame_limit: 256,
        })
        .expect("the native root prepares");
    activation.root_buffers_mut().0[0] = reference;
    let mut roots = vec![0; region.max_roots().max(1)];
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
                root_states: &mut root_states,
                fuel: 3,
                heap,
                class_parents: &[],
            },
        )
        .expect("the field load executes");
    assert_eq!(exit.kind(), ExitKind::Interpreter);
    assert_eq!((exit.block(), exit.instruction()), (0, 1));
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
            block: 0,
            instruction: 0,
            local_count: 2,
            max_stack: region.max_stack(),
            operand_len: 0,
            scalar_limit: 4_096,
            frame_limit: 256,
        })
        .expect("the native root prepares");
    let (locals, states, _) = activation.root_buffers_mut();
    locals[0] = bits;
    locals[1] = 42;
    states[0] = LOCAL_INITIALIZED;
    states[1] = LOCAL_INITIALIZED;
    let mut roots = vec![0; region.max_roots().max(1)];
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
                root_states: &mut root_states,
                fuel: 5,
                heap,
                class_parents: &[],
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
fn native_field_store_replays_a_frozen_receiver() {
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
            block: 0,
            instruction: 0,
            local_count: 2,
            max_stack: region.max_stack(),
            operand_len: 0,
            scalar_limit: 4_096,
            frame_limit: 256,
        })
        .expect("the native root prepares");
    let (locals, states, _) = activation.root_buffers_mut();
    locals[0] = bits;
    locals[1] = 42;
    states[0] = LOCAL_INITIALIZED;
    states[1] = LOCAL_INITIALIZED;
    let mut roots = vec![0; region.max_roots().max(1)];
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
                root_states: &mut root_states,
                fuel: 5,
                heap,
                class_parents: &[],
            },
        )
        .expect("the field store executes");
    assert_eq!(exit.kind(), ExitKind::Interpreter);
    assert_eq!(exit.retired(), 2);
    assert_eq!((exit.block(), exit.instruction()), (0, 2));
    assert_eq!(exit.stack_len(), 2);
}
