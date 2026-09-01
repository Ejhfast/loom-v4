use super::*;

#[test]
fn an_effect_completion_snapshot_resumes_in_both_engines() {
    let source = concat!(
        "def go(): Int with Clock.Now\n",
        "  observed = sys.clock.now()\n",
        "  i = 0\n  total = 0\n",
        "  while i < 1000\n",
        "    total = total + i\n",
        "    i = i + 1\n",
        "  end\n",
        "  total\n",
        "end\n",
        "go()\n",
    );
    let artifact = lm_testkit::compile_text("jit-effect-snapshot.lm", source)
        .expect("the effect snapshot case compiles");
    let (arena, namespace) = lm_testkit::publish_compiled_artifact(artifact.clone())
        .expect("the effect snapshot case publishes");
    let engine = Arc::new(Engine::new(EngineMode::Native));
    let mut world = World::new_with_engine(
        arena,
        namespace,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
        Arc::clone(&engine),
    );
    world.allow("Clock.Now").expect("the clock grant exists");
    let root = lm_vm::TaskKey {
        vm: 0,
        generation: 0,
    };
    assert!(matches!(
        world.drive_slice(root, 128),
        Some(lm_vm::SliceExit::Yielded)
    ));
    assert_eq!(engine.metrics().native_effect_exits, 1);
    let gate = world.next_gate();
    let image = world
        .capture_snapshot(gate, 0, false)
        .expect("the completed effect state captures");
    let (interpreted, _) = restore_with_engine(&artifact, image.world(), EngineMode::Interpreter);
    let (native, metrics) = restore_with_native(&artifact, image.world());
    assert!(matches!(
        interpreted,
        RootEvent::Done(lm_value::Value::Int(499_500))
    ));
    assert!(matches!(
        native,
        RootEvent::Done(lm_value::Value::Int(499_500))
    ));
    assert!(metrics.native_retired_instructions > 0, "{metrics:?}");
}

#[test]
fn a_native_allocation_snapshot_resumes_in_both_engines() {
    let artifact = lm_testkit::compile_text("jit-allocation-snapshot.lm", ALLOCATION_LOOP)
        .expect("the allocation snapshot case compiles");
    let (arena, namespace) = lm_testkit::publish_compiled_artifact(artifact.clone())
        .expect("the allocation snapshot case publishes");
    let engine = Arc::new(Engine::new(EngineMode::Native));
    let mut world = World::new_with_engine(
        arena,
        namespace,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
        Arc::clone(&engine),
    );
    let root = lm_vm::TaskKey {
        vm: 0,
        generation: 0,
    };
    assert!(matches!(
        world.drive_slice(root, 128),
        Some(lm_vm::SliceExit::Yielded)
    ));
    assert!(engine.metrics().native_allocations > 0);
    let gate = world.next_gate();
    let image = world
        .capture_snapshot(gate, 0, false)
        .expect("the allocation state captures");
    let (interpreted, _) = restore_with_engine(&artifact, image.world(), EngineMode::Interpreter);
    let (native, metrics) = restore_with_native(&artifact, image.world());
    assert!(matches!(
        interpreted,
        RootEvent::Done(lm_value::Value::Int(1000))
    ));
    assert!(matches!(
        native,
        RootEvent::Done(lm_value::Value::Int(1000))
    ));
    assert!(metrics.native_allocations > 0, "{metrics:?}");
}

#[test]
fn an_external_scalar_snapshot_uses_guarded_native_code() {
    let (artifact, image) = captured_scalar_loop();
    let (event, metrics) = restore_with_native(&artifact, &image);
    assert!(matches!(event, RootEvent::Done(lm_value::Value::Int(4950))));
    assert!(metrics.guarded_values > 0);
    assert!(metrics.native_retired_instructions > 0);
    assert_eq!(metrics.guard_failures, 0);
}

#[test]
fn an_external_mid_segment_snapshot_reaches_native_code() {
    let artifact = lm_testkit::compile_text("jit-mid-snapshot.lm", SCALAR_LOOP)
        .expect("the snapshot case compiles");
    let (arena, namespace) = lm_testkit::publish_compiled_artifact(artifact.clone())
        .expect("the snapshot case publishes");
    let mut world = World::new(
        arena,
        namespace,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    let root = lm_vm::TaskKey {
        vm: 0,
        generation: 0,
    };
    assert!(matches!(
        world.drive_slice(root, 1),
        Some(lm_vm::SliceExit::Yielded)
    ));
    let gate = world.next_gate();
    let image = world
        .capture_snapshot(gate, 0, false)
        .expect("the mid-segment state captures");
    let (event, metrics) = restore_with_native(&artifact, image.world());
    assert!(matches!(
        event,
        RootEvent::Done(lm_value::Value::Int(49_995_000))
    ));
    assert!(metrics.missing_entry_fallbacks > 0);
    assert!(metrics.native_retired_instructions > 0);
}

#[test]
fn a_wrong_external_scalar_value_never_enters_native_code() {
    let (artifact, mut image) = captured_scalar_loop();
    let local = image.machines[0]
        .locals
        .iter_mut()
        .find(|value| matches!(value, lm_value::Value::Int(_)))
        .expect("the loop holds an integer local");
    *local = lm_value::Value::Bool(false);
    let (event, metrics) = restore_with_native(&artifact, &image);
    assert!(
        matches!(event, RootEvent::Fault(record) if record.code == lm_vm::FaultCode::TypeMismatch)
    );
    assert_eq!(metrics.guard_failures, 1);
    assert_eq!(metrics.native_entries, 0);
}

#[test]
fn a_wrong_external_field_value_replays_before_native_use() {
    let source = concat!(
        "class Pair\n",
        "  left: Int\n",
        "  def init(mut self, left: Int)\n    self.left = left\n  end\n",
        "end\n",
        "pair = Pair(7)\ni = 0\nsum = 0\n",
        "while i < 100\n",
        "  sum = sum + pair.left\n  i = i + 1\n",
        "end\nsum\n",
    );
    let (artifact, mut image) = captured_loop("jit-field-snapshot.lm", source);
    let field = image.machines[0]
        .objects
        .iter_mut()
        .find_map(|object| match &mut object.object {
            lm_vm::Object::Instance { fields, .. } => fields.first_mut(),
            _ => None,
        })
        .expect("the snapshot holds the pair field");
    *field = lm_value::Value::Bool(false);
    let (interpreted, _) = restore_with_engine(&artifact, &image, EngineMode::Interpreter);
    let (native, metrics) = restore_with_native(&artifact, &image);
    assert!(
        matches!(interpreted, RootEvent::Fault(record) if record.code == lm_vm::FaultCode::TypeMismatch)
    );
    assert!(
        matches!(native, RootEvent::Fault(record) if record.code == lm_vm::FaultCode::TypeMismatch)
    );
    assert!(metrics.native_entries > 0, "{metrics:?}");
}

#[test]
fn a_wrong_external_list_value_replays_before_native_use() {
    let source = concat!(
        "items = [1, 2, 3]\ni = 0\nsum = 0\n",
        "while i < 100\n",
        "  sum = sum + items.at(i % 3)\n",
        "  i = i + 1\n",
        "end\nsum\n",
    );
    let (artifact, mut image) = captured_loop("jit-list-snapshot.lm", source);
    let item = image.machines[0]
        .objects
        .iter_mut()
        .find_map(|object| match &mut object.object {
            lm_vm::Object::List { items, .. } => items.first_mut(),
            _ => None,
        })
        .expect("the snapshot holds one list item");
    *item = lm_value::Value::Bool(false);
    let (interpreted, _) = restore_with_engine(&artifact, &image, EngineMode::Interpreter);
    let (native, metrics) = restore_with_native(&artifact, &image);
    assert!(
        matches!(interpreted, RootEvent::Fault(record) if record.code == lm_vm::FaultCode::TypeMismatch)
    );
    assert!(
        matches!(native, RootEvent::Fault(record) if record.code == lm_vm::FaultCode::TypeMismatch)
    );
    assert!(metrics.native_entries > 0, "{metrics:?}");
}

#[test]
fn a_wrong_external_list_parameter_uses_a_checked_pointer_boundary() {
    let source = concat!(
        "def sum_items(items: [Int], keep: (Int, Int)): Int\n",
        "  index = 0\n  total = 0\n",
        "  while index < 100\n",
        "    total = total + items.at(index % 3)\n",
        "    index = index + 1\n",
        "  end\n  total + keep[0] - keep[0]\nend\n",
        "items = [1, 2, 3]\nkeep = (7, 8)\n",
        "sum_items(items, keep)\n",
    );
    let (artifact, mut image) = captured_loop("jit-list-parameter-snapshot.lm", source);
    let tuple = image.machines[0]
        .objects
        .iter()
        .position(|object| matches!(object.object, lm_vm::Object::Tuple { .. }))
        .expect("the snapshot holds the tuple");
    let frame = image.machines[0]
        .frames
        .last()
        .expect("the list function is active");
    let parameter = frame.base_local as usize;
    image.machines[0].locals[parameter] = lm_value::Value::Obj(lm_value::ObjRef {
        slot: tuple as u32,
        generation: 0,
    });
    let (interpreted, _) = restore_with_engine(&artifact, &image, EngineMode::Interpreter);
    let (native, metrics) = restore_with_native(&artifact, &image);
    assert!(
        matches!(interpreted, RootEvent::Fault(record) if record.code == lm_vm::FaultCode::TypeMismatch)
    );
    assert!(
        matches!(native, RootEvent::Fault(ref record) if record.code == lm_vm::FaultCode::TypeMismatch),
        "{native:?} {metrics:?}"
    );
    assert!(metrics.native_entries > 0, "{metrics:?}");
    assert!(metrics.materializations > 0, "{metrics:?}");
}

#[test]
fn a_wrong_external_map_value_replays_before_native_mutation() {
    let source = concat!(
        "table = {\"value\": 1}\ni = 0\ntotal = 0\n",
        "while i < 100\n",
        "  case table.put(\"value\", i)\n",
        "  in Some(previous) then total = total + previous\n",
        "  in None then total = total + 0\n",
        "  end\n",
        "  i = i + 1\n",
        "end\ntotal\n",
    );
    let (artifact, mut image) = captured_loop("jit-map-snapshot.lm", source);
    let value = image.machines[0]
        .objects
        .iter_mut()
        .find_map(|object| match &mut object.object {
            lm_vm::Object::Map { entries, .. } => entries.first_mut(),
            _ => None,
        })
        .expect("the snapshot holds one map entry");
    value.value = lm_value::Value::Bool(false);
    let (interpreted, _) = restore_with_engine(&artifact, &image, EngineMode::Interpreter);
    let (native, metrics) = restore_with_native(&artifact, &image);
    assert!(
        matches!(interpreted, RootEvent::Fault(record) if record.code == lm_vm::FaultCode::TypeMismatch)
    );
    assert!(
        matches!(native, RootEvent::Fault(record) if record.code == lm_vm::FaultCode::TypeMismatch)
    );
    assert!(metrics.native_entries > 0, "{metrics:?}");
    assert!(metrics.native_interpreter_exits > 0, "{metrics:?}");
}

#[test]
fn a_wrong_external_option_payload_matches_the_interpreter() {
    let source = concat!(
        "def read(value: Option[Int]): Int\n",
        "  case value\n",
        "  in Some(found) then found\n",
        "  in None then 0\n",
        "  end\n",
        "end\n",
        "value: Option[Int] = Some(7)\n",
        "i = 0\ntotal = 0\n",
        "while i < 100\n",
        "  total = total + read(value)\n",
        "  i = i + 1\n",
        "end\ntotal\n",
    );
    let (artifact, mut image) = captured_loop("jit-option-snapshot.lm", source);
    let local = image.machines[0]
        .locals
        .iter_mut()
        .find(|value| **value == lm_value::Value::Int(7))
        .expect("the snapshot holds the Option payload");
    *local = lm_value::Value::Bool(false);
    let (interpreted, _) = restore_with_engine(&artifact, &image, EngineMode::Interpreter);
    let (native, metrics) = restore_with_native(&artifact, &image);
    assert!(
        matches!(interpreted, RootEvent::Fault(record) if record.code == lm_vm::FaultCode::TypeMismatch)
    );
    assert!(
        matches!(native, RootEvent::Fault(ref record) if record.code == lm_vm::FaultCode::TypeMismatch),
        "{native:?}: {metrics:?}"
    );
    assert!(metrics.native_entries > 0, "{metrics:?}");
}

#[test]
fn a_wrong_dormant_value_does_not_block_native_code() {
    let source = concat!(
        "dormant = 777\n",
        "i = 0\n",
        "while i < 100\n",
        "  i = i + 1\n",
        "end\n",
        "dormant = 2\n",
        "dormant\n",
    );
    let (artifact, mut image) = captured_loop("jit-dormant.lm", source);
    let local = image.machines[0]
        .locals
        .iter_mut()
        .find(|value| matches!(value, lm_value::Value::Int(777)))
        .expect("the loop holds the dormant local");
    *local = lm_value::Value::Bool(false);
    let (event, metrics) = restore_with_native(&artifact, &image);
    assert!(matches!(event, RootEvent::Done(lm_value::Value::Int(2))));
    assert!(metrics.native_entries > 0);
    assert_eq!(metrics.guard_failures, 0);
}

#[test]
fn engine_switches_preserve_scalar_state() {
    let artifact =
        lm_testkit::compile_text("jit-switch.lm", SCALAR_LOOP).expect("the switch case compiles");
    let (arena, namespace) =
        lm_testkit::publish_compiled_artifact(artifact.clone()).expect("the case publishes");
    let engine = Arc::new(Engine::new(EngineMode::Interpreter));
    let mut world = World::new_with_engine(
        arena,
        namespace,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
        Arc::clone(&engine),
    );
    for _ in 0..20 {
        let gate = world.next_gate();
        let image = world
            .capture_snapshot(gate, 0, false)
            .expect("the switch state captures");
        if image.world().machines[0]
            .frames
            .last()
            .is_some_and(|frame| frame.block == 1 && frame.ip == 0)
        {
            break;
        }
        assert!(matches!(world.step_root(), RootEvent::Ran));
    }
    engine.set_mode(EngineMode::Native);
    assert_eq!(
        world.run_root(),
        Outcome::Done(lm_value::Value::Int(49_995_000))
    );
    assert!(engine.metrics().native_retired_instructions > 0);

    let (arena, namespace) =
        lm_testkit::publish_compiled_artifact(artifact).expect("the case publishes again");
    let engine = Arc::new(Engine::new(EngineMode::Native));
    let mut world = World::new_with_engine(
        arena,
        namespace,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
        Arc::clone(&engine),
    );
    let root = lm_vm::TaskKey {
        vm: 0,
        generation: 0,
    };
    assert!(matches!(
        world.drive_slice(root, 4096),
        Some(lm_vm::SliceExit::Yielded)
    ));
    let suspended = engine.metrics();
    assert!(suspended.native_retired_instructions > 0);
    assert_eq!(suspended.native_continuation_suspends, 0);
    assert!(suspended.materializations > 0);
    assert_eq!(suspended.native_continuation_materializations, 0);
    engine.set_mode(EngineMode::Interpreter);
    assert_eq!(
        world.run_root(),
        Outcome::Done(lm_value::Value::Int(49_995_000))
    );
    assert_eq!(engine.metrics().native_continuation_materializations, 0);
}

#[test]
fn exact_native_quanta_keep_canonical_state() {
    let artifact = lm_testkit::compile_text("jit-quanta.lm", SCALAR_LOOP)
        .expect("the continuation case compiles");
    let (arena, namespace) =
        lm_testkit::publish_compiled_artifact(artifact).expect("the case publishes");
    let engine = Arc::new(Engine::new(EngineMode::Native));
    let mut world = World::new_with_engine(
        arena,
        namespace,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
        Arc::clone(&engine),
    );
    let root = lm_vm::TaskKey {
        vm: 0,
        generation: 0,
    };
    for _ in 0..2 {
        assert!(matches!(
            world.drive_slice(root, 4096),
            Some(lm_vm::SliceExit::Yielded)
        ));
    }
    let metrics = engine.metrics();
    assert_eq!(metrics.native_continuation_suspends, 0);
    assert_eq!(metrics.native_continuation_resumes, 0);
    assert_eq!(metrics.native_continuation_materializations, 0);
    assert!(metrics.materializations >= 2, "{metrics:?}");
    engine.set_mode(EngineMode::Interpreter);
    assert_eq!(
        world.run_root(),
        Outcome::Done(lm_value::Value::Int(49_995_000))
    );
    assert_eq!(engine.metrics().native_continuation_materializations, 0);
}

#[test]
fn a_native_call_stack_survives_a_quantum() {
    let source = concat!(
        "def sum_to(value: Int): Int\n",
        "  if value == 0 then 0 else value + sum_to(value - 1) end\n",
        "end\n",
        "sum_to(100)\n",
    );
    let artifact =
        lm_testkit::compile_text("jit-call-quantum.lm", source).expect("the call case compiles");
    let (arena, namespace) =
        lm_testkit::publish_compiled_artifact(artifact).expect("the call case publishes");
    let engine = Arc::new(Engine::new(EngineMode::Native));
    let mut world = World::new_with_engine(
        arena,
        namespace,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
        Arc::clone(&engine),
    );
    let root = lm_vm::TaskKey {
        vm: 0,
        generation: 0,
    };
    assert!(matches!(
        world.drive_slice(root, 64),
        Some(lm_vm::SliceExit::Yielded)
    ));
    assert!(matches!(
        world.drive_slice(root, 64),
        Some(lm_vm::SliceExit::Yielded)
    ));
    assert_eq!(world.run_root(), Outcome::Done(lm_value::Value::Int(5050)));
    let metrics = engine.metrics();
    assert!(metrics.native_continuation_resumes > 0, "{metrics:?}");
    assert!(metrics.native_retired_instructions > 900, "{metrics:?}");
}

#[test]
fn object_results_survive_native_call_quanta() {
    let source = concat!(
        "class Box\n  value: Int\n",
        "  def init(mut self, value: Int)\n    self.value = value\n  end\nend\n",
        "def leaf(value: Int): Box\n  Box(value)\nend\n",
        "def middle(value: Int): Box\n  leaf(value)\nend\n",
        "def outer(value: Int): Box\n  middle(value)\nend\n",
        "i = 0\nsum = 0\nwhile i < 1000\n",
        "  box = outer(i)\n  sum = sum + box.value\n  i = i + 1\n",
        "end\nsum\n",
    );
    let artifact = lm_testkit::compile_text("jit-object-quantum.lm", source)
        .expect("the object result case compiles");
    let (arena, namespace) =
        lm_testkit::publish_compiled_artifact(artifact).expect("the object result case publishes");
    let engine = Arc::new(Engine::new(EngineMode::Native));
    let mut world = World::new_with_engine(
        arena,
        namespace,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
        engine,
    );
    let root = lm_vm::TaskKey {
        vm: 0,
        generation: 0,
    };
    for _ in 0..100_000 {
        match world.drive_slice(root, 7) {
            Some(lm_vm::SliceExit::Yielded) => {}
            Some(lm_vm::SliceExit::Terminal) => break,
            other => panic!("the object result run stopped early: {other:?}"),
        }
    }
    assert_eq!(
        world.task_outcome(root),
        Outcome::Done(lm_value::Value::Int(499_500))
    );
}

#[test]
fn native_calls_carry_each_supported_object_reference() {
    let source = concat!(
        "def keep_text(value: String, first: Bool): String\n",
        "  if first then value else \"other\" end\nend\n",
        "def keep_map(value: Map[String, Int], first: Bool): Map[String, Int]\n",
        "  if first then value else {} end\nend\n",
        "def keep_task(escaping value: () -> Int, first: Bool): () -> Int\n",
        "  if first then value else do ||: Int 8 end end\nend\n",
        "text = \"loom\"\n",
        "table: Map[String, Int] = {\"loom\": 1}\n",
        "task = do ||: Int 7 end\n",
        "i = 0\nwhile i < 1000\n",
        "  text = keep_text(text, true)\n",
        "  table = keep_map(table, true)\n",
        "  task = keep_task(task, true)\n",
        "  i = i + 1\n",
        "end\ni\n",
    );
    let artifact = lm_testkit::compile_text("jit-object-calls.lm", source)
        .expect("the object call case compiles");
    let (interpreted, _, interpreted_dump) =
        run_artifact(&artifact, EngineMode::Interpreter, u64::MAX);
    let (arena, namespace) =
        lm_testkit::publish_compiled_artifact(artifact).expect("the object call case publishes");
    let engine = Arc::new(Engine::new(EngineMode::Native));
    let mut world = World::new_with_engine(
        arena,
        namespace,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
        Arc::clone(&engine),
    );
    let root = lm_vm::TaskKey {
        vm: 0,
        generation: 0,
    };
    for _ in 0..100_000 {
        match world.drive_slice(root, 17) {
            Some(lm_vm::SliceExit::Yielded) => {}
            Some(lm_vm::SliceExit::Terminal) => break,
            other => panic!("the object call run stopped early: {other:?}"),
        }
    }
    let native = world.task_outcome(root);
    let native_dump = world.dump_live(&native);
    assert_eq!(native, interpreted);
    assert_eq!(native_dump, interpreted_dump);
    assert_eq!(native, Outcome::Done(lm_value::Value::Int(1000)));
    let metrics = engine.metrics();
    assert!(metrics.compiled_call_sites >= 3, "{metrics:?}");
    assert!(metrics.native_retired_instructions > 10_000, "{metrics:?}");
}

#[test]
fn result_objects_survive_native_call_quanta() {
    let source = concat!(
        "class Box\n  value: Int\n",
        "  def init(mut self, value: Int)\n    self.value = value\n  end\nend\n",
        "def leaf(value: Int): Result[Box, String]\n  Ok(Box(value))\nend\n",
        "def outer(value: Int): Result[Box, String]\n  leaf(value)\nend\n",
        "i = 0\nsum = 0\nwhile i < 32\n",
        "  case outer(i)\n",
        "  in Ok(box) then sum = sum + box.value\n",
        "  in Err(_) then sum = sum - 10000\n",
        "  end\n  i = i + 1\n",
        "end\nsum\n",
    );
    let artifact = lm_testkit::compile_text("jit-result-quantum.lm", source)
        .expect("the result object case compiles");
    let (arena, namespace) =
        lm_testkit::publish_compiled_artifact(artifact).expect("the result object case publishes");
    let engine = Arc::new(Engine::new(EngineMode::Native));
    for quantum in 1..=32 {
        let mut world = World::new_with_engine(
            arena.clone(),
            namespace,
            VmConfig::default(),
            Box::new(RecordingHost::new(1)),
            Arc::clone(&engine),
        );
        let root = lm_vm::TaskKey {
            vm: 0,
            generation: 0,
        };
        for _ in 0..100_000 {
            match world.drive_slice(root, quantum) {
                Some(lm_vm::SliceExit::Yielded) => {}
                Some(lm_vm::SliceExit::Terminal) => break,
                other => panic!("the result object run stopped early: {other:?}"),
            }
        }
        let outcome = world.task_outcome(root);
        assert_eq!(
            outcome,
            Outcome::Done(lm_value::Value::Int(496)),
            "quantum {quantum}: {:?}\n{}",
            engine.metrics(),
            world.dump_live(&outcome)
        );
    }
}

#[test]
fn native_execution_resumes_after_an_interpreter_mid_segment_stop() {
    let artifact =
        lm_testkit::compile_text("jit-mid-segment.lm", SCALAR_LOOP).expect("the case compiles");
    let (arena, namespace) =
        lm_testkit::publish_compiled_artifact(artifact).expect("the case publishes");
    let engine = Arc::new(Engine::new(EngineMode::Interpreter));
    let mut world = World::new_with_engine(
        arena,
        namespace,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
        Arc::clone(&engine),
    );
    let root = lm_vm::TaskKey {
        vm: 0,
        generation: 0,
    };
    assert!(matches!(
        world.drive_slice(root, 1),
        Some(lm_vm::SliceExit::Yielded)
    ));
    engine.set_mode(EngineMode::Native);
    assert_eq!(
        world.run_root(),
        Outcome::Done(lm_value::Value::Int(49_995_000))
    );
    let metrics = engine.metrics();
    assert!(metrics.missing_entry_fallbacks > 0);
    assert!(metrics.native_retired_instructions > 0);
}

#[test]
fn a_reserved_chain_entry_advances_to_a_native_head() {
    let source = concat!(
        "items = [0, 1, 2, 3, 4, 5, 6, 7]\n",
        "i = 0\n",
        "while i < 10000\n",
        "  items.set(i % 8, i)\n",
        "  i = i + 1\n",
        "end\n",
        "items.at(7)\n",
    );
    let artifact = lm_testkit::compile_text("jit-reserved-entry.lm", source)
        .expect("the reserved entry case compiles");
    let (arena, namespace) =
        lm_testkit::publish_compiled_artifact(artifact).expect("the case publishes");
    let engine = Arc::new(Engine::new(EngineMode::Interpreter));
    let mut world = World::new_with_engine(
        arena,
        namespace,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
        Arc::clone(&engine),
    );
    let root = lm_vm::TaskKey {
        vm: 0,
        generation: 0,
    };
    assert!(matches!(
        world.drive_slice(root, 48),
        Some(lm_vm::SliceExit::Yielded)
    ));
    engine.set_mode(EngineMode::Native);
    assert_eq!(world.run_root(), Outcome::Done(lm_value::Value::Int(9_999)));
    let metrics = engine.metrics();
    assert!(metrics.missing_entry_fallbacks > 0, "{metrics:?}");
    assert!(metrics.native_retired_instructions > 10_000, "{metrics:?}");
}

#[test]
fn alternating_engines_preserve_each_bounded_turn() {
    let source = "i = 0\ns = 0\nwhile i < 100\n  s = s + i\n  i = i + 1\nend\ns\n";
    let artifact = lm_testkit::compile_text("jit-alternate.lm", source).expect("the case compiles");
    let (arena, namespace) =
        lm_testkit::publish_compiled_artifact(artifact).expect("the case publishes");
    let engine = Arc::new(Engine::new(EngineMode::Interpreter));
    let mut world = World::new_with_engine(
        arena,
        namespace,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
        Arc::clone(&engine),
    );
    let root = lm_vm::TaskKey {
        vm: 0,
        generation: 0,
    };
    for turn in 0..1000 {
        let mode = if turn % 2 == 0 {
            EngineMode::Native
        } else {
            EngineMode::Interpreter
        };
        engine.set_mode(mode);
        match world.drive_slice(root, 64) {
            Some(lm_vm::SliceExit::Yielded) => {}
            Some(lm_vm::SliceExit::Terminal) => break,
            other => panic!("the alternating run stopped early: {other:?}"),
        }
    }
    assert_eq!(
        world.task_outcome(root),
        Outcome::Done(lm_value::Value::Int(4950))
    );
    assert!(engine.metrics().native_retired_instructions > 0);
}

#[test]
fn native_capture_resumes_in_the_interpreter() {
    let artifact =
        lm_testkit::compile_text("jit-capture.lm", SCALAR_LOOP).expect("the capture case compiles");
    let (arena, namespace) =
        lm_testkit::publish_compiled_artifact(artifact.clone()).expect("the case publishes");
    let engine = Arc::new(Engine::new(EngineMode::Native));
    let mut native = World::new_with_engine(
        arena,
        namespace,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
        Arc::clone(&engine),
    );
    let root = lm_vm::TaskKey {
        vm: 0,
        generation: 0,
    };
    assert!(matches!(
        native.drive_slice(root, 4096),
        Some(lm_vm::SliceExit::Yielded)
    ));
    assert_eq!(engine.metrics().native_continuation_suspends, 0);
    assert!(engine.metrics().materializations > 0);
    assert_eq!(engine.metrics().native_continuation_materializations, 0);
    let gate = native.next_gate();
    let snapshot = native
        .capture_snapshot(gate, 0, false)
        .expect("native state captures");
    assert_eq!(engine.metrics().native_continuation_materializations, 0);
    let bytes =
        lm_vm::snapshot::codec::encode(snapshot.world(), usize::MAX).expect("native state encodes");
    let admitted = lm_testkit::load_snapshot_for_artifact(
        &artifact,
        &bytes,
        lm_vm::snapshot::LoadLimits::default(),
    )
    .expect("native state admits");
    let (arena, namespace) =
        lm_testkit::publish_artifact(&artifact).expect("the artifact publishes");
    let mut interpreted = World::new(
        arena,
        namespace,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    let target = interpreted.new_child(0).expect("the child budget exists");
    let restored = interpreted
        .restore_image(0, target, &admitted)
        .expect("native state restores");
    assert!(matches!(
        interpreted.run_machine(restored),
        RootEvent::Done(lm_value::Value::Int(49_995_000))
    ));
}
