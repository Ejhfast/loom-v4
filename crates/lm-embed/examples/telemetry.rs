use lm_abi::{AbiType, GroupSpec, OperationSpec};
use lm_embed::RegistryBuilder;
use lm_vm::{HostArg, VmConfig};

fn main() -> Result<(), String> {
    let mut builder = RegistryBuilder::new();
    builder.add_group(GroupSpec::namespace("Telemetry"));
    builder.add_operation(OperationSpec::fixed(
        "Telemetry",
        "Record",
        vec![AbiType::INT],
        AbiType::INT,
    ));
    builder.serve_checked("Telemetry.Record", |args, reply| match args.as_slice() {
        [HostArg::Int(value)] => reply.int(value + 1),
        _ => Err("Telemetry.Record received invalid arguments".to_string()),
    });

    let registry = builder.build()?;
    let compiled = registry.compile("telemetry", "telemetry.lm", "sys.telemetry.record(41)\n")?;
    let loaded = registry
        .load(&compiled.artifact)
        .map_err(|error| error.to_string())?;
    let mut world = registry.world(&loaded, VmConfig::default())?;
    world.allow("Telemetry")?;
    let outcome = world.run_root();
    println!("{}", world.show_outcome(&outcome));
    Ok(())
}
