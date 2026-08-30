use super::*;
use crate::plan::{compute_liveness, split_segments, Segment};
use lm_bytecode::{BcClass, BcClassKind, BcType, Func, Instr, Module, NO_PARENT};

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
        field_results: vec![],
        replay_stacks: vec![],
        fault_stacks: vec![],
        allocations: vec![],
    }];
    compute_liveness(&mut segments, 2);
    assert_eq!(segments[0].live_in, vec![false, true]);
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

struct FieldRuntime {
    result: RuntimeResult,
}

impl Runtime for FieldRuntime {
    fn load_field(
        &mut self,
        reference: lm_value::ObjRef,
        field: u32,
        expected: ValueRepr,
    ) -> RuntimeResult {
        assert_eq!(reference.slot, 3);
        assert_eq!(reference.generation, 7);
        assert_eq!(field, 0);
        assert_eq!(expected, ValueRepr::Int);
        self.result
    }

    fn allocate_instance(
        &mut self,
        _class: u32,
        _root_bits: &[u64],
        _root_states: &[u8],
        _allow_collection: bool,
    ) -> RuntimeResult {
        RuntimeResult::Interpreter
    }
}

#[test]
fn native_field_load_uses_the_checked_runtime_boundary() {
    let module = field_module();
    let bundle = lm_abi::standard_bundle();
    lm_verify::verify_module_with_bundle(&module, &bundle).expect("the field load verifies");
    let engine = JitEngine::default();
    let region = engine
        .compile(FunctionInput::new(0, &module.funcs[0], &module, &bundle, 0))
        .expect("the field load compiles");
    let reference = u64::from(3u32) | (u64::from(7u32) << 32);
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
    let mut runtime = FieldRuntime {
        result: RuntimeResult::Value(41),
    };
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
    let reference = u64::from(3u32) | (u64::from(7u32) << 32);
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
    let mut runtime = FieldRuntime {
        result: RuntimeResult::UninitializedField,
    };
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
            },
        )
        .expect("the field fault executes");
    assert_eq!(exit.kind(), ExitKind::UninitializedField);
    assert_eq!(exit.retired(), 2);
    assert_eq!((exit.block(), exit.instruction()), (0, 2));
    assert_eq!(exit.stack_len(), 0);
}
