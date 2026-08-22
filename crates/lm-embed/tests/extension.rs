use lm_abi::{AbiPrimitive, AbiType, BoundaryMode, GroupSpec, OperationSpec, ResourceDescriptor};
use lm_embed::RegistryBuilder;
use lm_vm::{
    CompletionKey, FaultCode, Host, HostArg, HostResource, HostStart, HostValue, Outcome, TaskKey,
    VmConfig,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

const INT: AbiType = AbiType::Primitive(AbiPrimitive::Int);

fn telemetry_builder() -> RegistryBuilder {
    let mut builder = RegistryBuilder::new();
    builder.add_group(GroupSpec::namespace("Telemetry"));
    builder.add_operation(OperationSpec::fixed("Telemetry", "Record", vec![INT], INT));
    builder
}

#[test]
fn a_custom_operation_compiles_and_runs() {
    let mut builder = telemetry_builder();
    builder.serve("Telemetry.Record", |args| match args.as_slice() {
        [HostArg::Int(value)] => Ok(HostValue::Int(value + 1)),
        _ => Err("Telemetry.Record received invalid arguments".to_string()),
    });
    let registry = builder.build().expect("the registry is valid");
    let compiled = registry
        .compile(
            "telemetry_test",
            "telemetry_test.lm",
            "sys.telemetry.record(41)\n",
        )
        .expect("the custom operation compiles");
    let loaded = registry
        .load(&compiled.artifact)
        .expect("the artifact loads");
    let mut world = registry
        .world(&loaded, VmConfig::default())
        .expect("the bundles match");
    world.allow("Telemetry").expect("the group exists");
    let outcome = world.run_root();
    assert_eq!(world.show_outcome(&outcome), "Done(42)");
}

#[test]
fn a_missing_handler_faults_only_the_machine() {
    let registry = telemetry_builder().build().expect("the registry is valid");
    let compiled = registry
        .compile("missing", "missing.lm", "sys.telemetry.record(1)\n")
        .expect("the operation compiles");
    let loaded = registry
        .load(&compiled.artifact)
        .expect("the artifact loads");
    let mut world = registry
        .world(&loaded, VmConfig::default())
        .expect("the bundles match");
    world.allow("Telemetry").expect("the group exists");
    assert_eq!(world.run_root(), Outcome::Fault(FaultCode::HostFault));
}

#[test]
fn a_wrong_reply_type_faults_only_the_machine() {
    let mut builder = telemetry_builder();
    builder.serve("Telemetry.Record", |_| Ok(HostValue::Bool(true)));
    let registry = builder.build().expect("the registry is valid");
    let compiled = registry
        .compile("wrong", "wrong.lm", "sys.telemetry.record(1)\n")
        .expect("the operation compiles");
    let loaded = registry
        .load(&compiled.artifact)
        .expect("the artifact loads");
    let mut world = registry
        .world(&loaded, VmConfig::default())
        .expect("the bundles match");
    world.allow("Telemetry").expect("the group exists");
    assert_eq!(world.run_root(), Outcome::Fault(FaultCode::TypeMismatch));
}

#[test]
fn another_bundle_rejects_the_artifact() {
    let registry = telemetry_builder().build().expect("the registry is valid");
    let compiled = registry
        .compile("bound", "bound.lm", "sys.telemetry.record(1)\n")
        .expect("the operation compiles");
    let other = RegistryBuilder::new()
        .build()
        .expect("the standard registry is valid");
    assert!(other.load(&compiled.artifact).is_err());
}

#[test]
fn a_custom_effect_set_has_exact_members() {
    let mut builder = RegistryBuilder::new();
    builder.add_group(GroupSpec::namespace("Telemetry"));
    builder.add_group(GroupSpec::namespace("Audit"));
    builder.add_group(GroupSpec::namespace("Debug"));
    builder.add_group(GroupSpec::effect_set(
        "Observe",
        ["Telemetry.Record", "Audit.Note"],
    ));
    builder.add_operation(OperationSpec::fixed("Telemetry", "Record", vec![INT], INT));
    builder.add_operation(OperationSpec::fixed("Audit", "Note", vec![INT], INT));
    builder.add_operation(OperationSpec::fixed("Debug", "Peek", vec![INT], INT));
    let registry = builder.build().expect("the registry is valid");
    registry
        .compile(
            "observe",
            "observe.lm",
            "def observe(): Int with Observe\n  sys.telemetry.record(sys.audit.note(1))\nend\nobserve()\n",
        )
        .expect("the exact group members type-check");
    let error = registry
        .compile(
            "outside",
            "outside.lm",
            "def outside(): Int with Observe\n  sys.debug.peek(1)\nend\noutside()\n",
        )
        .expect_err("an operation outside the group must fail");
    assert!(error.contains("needs the effect row"));
    let observe = registry
        .bundle()
        .group_by_name("Observe")
        .expect("the group exists");
    let mut names: Vec<&str> = registry
        .bundle()
        .group_operations(observe)
        .expect("the group resolves")
        .into_iter()
        .map(|slot| {
            registry
                .bundle()
                .op_name(slot)
                .expect("the operation exists")
        })
        .collect();
    names.sort_unstable();
    assert_eq!(names, ["Audit.Note", "Telemetry.Record"]);
}

fn completion_key() -> CompletionKey {
    CompletionKey {
        machine: TaskKey {
            vm: 0,
            generation: 0,
        },
        ordinal: 1,
    }
}

#[test]
fn an_async_handler_completes_once() {
    let mut builder = telemetry_builder();
    builder.serve_async("Telemetry.Record", |_, completion| {
        std::thread::spawn(move || {
            completion
                .complete(HostValue::Int(9))
                .expect("the completion is active");
        });
        Ok(())
    });
    let mut registry = builder.build().expect("the registry is valid");
    let operation = registry
        .bundle()
        .op_by_name("Telemetry.Record")
        .expect("the operation exists");
    let token = match registry.start(completion_key(), operation, vec![HostArg::Int(1)]) {
        HostStart::Waiting(token) => token,
        _ => panic!("the operation did not wait"),
    };
    let reply = registry.wait().expect("the completion arrives");
    assert_eq!(reply.token, token);
    assert_eq!(reply.result, Ok(HostValue::Int(9)));
}

#[test]
fn cancellation_runs_once() {
    let canceled = Arc::new(AtomicBool::new(false));
    let observed = canceled.clone();
    let mut builder = telemetry_builder();
    builder.serve_async("Telemetry.Record", move |_, mut completion| {
        let observed = observed.clone();
        completion
            .on_cancel(move || observed.store(true, Ordering::Release))
            .map_err(|error| error.to_string())?;
        Ok(())
    });
    let mut registry = builder.build().expect("the registry is valid");
    let operation = registry
        .bundle()
        .op_by_name("Telemetry.Record")
        .expect("the operation exists");
    let token = match registry.start(completion_key(), operation, vec![HostArg::Int(1)]) {
        HostStart::Waiting(token) => token,
        _ => panic!("the operation did not wait"),
    };
    assert!(registry.cancel(token));
    assert!(!registry.cancel(token));
    assert!(canceled.load(Ordering::Acquire));
}

#[test]
fn a_live_custom_resource_blocks_a_snapshot_and_then_closes() {
    let closed = Arc::new(AtomicBool::new(false));
    let observed = closed.clone();
    let resource = ResourceDescriptor::host_attachment("Device.Handle");
    let identity = resource.identity();
    let mut builder = RegistryBuilder::new();
    builder.add_group(GroupSpec::namespace("Device"));
    builder.add_resource(resource.clone());
    builder.add_operation(
        OperationSpec::fixed("Device", "Open", vec![], resource.abi_type())
            .boundary_modes(vec![], BoundaryMode::Move),
    );
    builder.serve("Device.Open", move |_| {
        Ok(HostValue::Resource(HostResource {
            kind: identity,
            token: 7,
            generation: 1,
        }))
    });
    builder.clean_resource("Device.Handle", move |resource| {
        observed.store(true, Ordering::Release);
        resource.token == 7 && resource.generation == 1
    });
    let registry = builder.build().expect("the registry is valid");
    let compiled = registry
        .compile(
            "resource",
            "resource.lm",
            "resource = sys.device.open()\nsys.vm.snapshot_self()\n",
        )
        .expect("the resource operation compiles");
    let loaded = registry
        .load(&compiled.artifact)
        .expect("the artifact loads");
    let mut world = registry
        .world(&loaded, VmConfig::default())
        .expect("the bundles match");
    world.allow("Device").expect("the custom group exists");
    world
        .allow("Vm.SnapshotSelf")
        .expect("the snapshot operation exists");
    let outcome = world.run_root();
    let shown = world.show_outcome(&outcome);
    assert!(
        shown.contains("ResourceActive"),
        "unexpected result: {shown}"
    );
    assert!(closed.load(Ordering::Acquire));
}

#[test]
fn a_moved_custom_resource_leaves_guest_ownership() {
    let consumed = Arc::new(AtomicBool::new(false));
    let observed = consumed.clone();
    let resource = ResourceDescriptor::host_attachment("Device.Handle");
    let identity = resource.identity();
    let mut builder = RegistryBuilder::new();
    builder.add_group(GroupSpec::namespace("Device"));
    builder.add_resource(resource.clone());
    builder.add_operation(
        OperationSpec::fixed("Device", "Open", vec![], resource.abi_type())
            .boundary_modes(vec![], BoundaryMode::Move),
    );
    builder.add_operation(
        OperationSpec::fixed("Device", "Close", vec![resource.abi_type()], AbiType::UNIT)
            .boundary_modes(vec![BoundaryMode::Move], BoundaryMode::Copy),
    );
    builder.serve("Device.Open", move |_| {
        Ok(HostValue::Resource(HostResource {
            kind: identity,
            token: 8,
            generation: 2,
        }))
    });
    builder.serve("Device.Close", move |args| match args.as_slice() {
        [HostArg::Resource(resource)] if resource.token == 8 && resource.generation == 2 => {
            observed.store(true, Ordering::Release);
            Ok(HostValue::Unit)
        }
        _ => Err("Device.Close received another resource".to_string()),
    });
    let registry = builder.build().expect("the registry is valid");
    let compiled = registry
        .compile(
            "resource_move",
            "resource_move.lm",
            "resource = sys.device.open()\nsys.device.close(resource)\nsys.vm.snapshot_self()\n",
        )
        .expect("the resource operations compile");
    let loaded = registry
        .load(&compiled.artifact)
        .expect("the artifact loads");
    let mut world = registry
        .world(&loaded, VmConfig::default())
        .expect("the bundles match");
    world.allow("Device").expect("the custom group exists");
    world
        .allow("Vm.SnapshotSelf")
        .expect("the snapshot operation exists");
    let outcome = world.run_root();
    let shown = world.show_outcome(&outcome);
    assert!(
        !shown.contains("ResourceActive"),
        "unexpected result: {shown}"
    );
    assert!(consumed.load(Ordering::Acquire));
}
