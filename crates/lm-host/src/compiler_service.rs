//! Bounded runtime compilation for the command-line host.

use crate::ReadySender;
use lm_compiler::{compile_module_with_options, CompileEnv, CompileOptions};
use lm_source::SourceFile;
use lm_vm::{
    CompletionKey, CoreCtor, HostCompileEnv, HostCompileOptions, HostCompletion, HostValue,
    SharedText,
};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};

const MAX_PENDING_COMPILES: usize = 64;

pub(crate) struct CompilerService {
    jobs: SyncSender<Job>,
}

pub(crate) struct CompileRequest {
    pub module_name: SharedText,
    pub source_name: SharedText,
    pub source: SharedText,
    pub env: HostCompileEnv,
    pub options: HostCompileOptions,
}

struct Job {
    key: CompletionKey,
    token: u64,
    request: CompileRequest,
}

impl CompilerService {
    pub(crate) fn new(results: ReadySender) -> CompilerService {
        let (jobs, queue) = mpsc::sync_channel(MAX_PENDING_COMPILES);
        std::thread::Builder::new()
            .name("loom-compiler".to_string())
            .spawn(move || compiler_worker(queue, results))
            .expect("the runtime compiler worker starts");
        CompilerService { jobs }
    }

    pub(crate) fn submit(&self, key: CompletionKey, token: u64, request: CompileRequest) -> bool {
        match self.jobs.try_send(Job {
            key,
            token,
            request,
        }) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => false,
        }
    }
}

fn compiler_worker(jobs: Receiver<Job>, completions: ReadySender) {
    while let Ok(job) = jobs.recv() {
        let value = compile(job.request);
        let _ = completions.completion(HostCompletion {
            key: job.key,
            token: job.token,
            result: Ok(value),
        });
    }
}

fn compile(request: CompileRequest) -> HostValue {
    match compile_inner(request) {
        Ok(value) => HostValue::Ctor(CoreCtor::Ok, vec![value]),
        Err(message) => HostValue::Ctor(
            CoreCtor::Err,
            vec![HostValue::Ctor(
                CoreCtor::CompileErrors,
                vec![HostValue::Str(message.into())],
            )],
        ),
    }
}

fn compile_inner(request: CompileRequest) -> Result<HostValue, String> {
    let mut env = CompileEnv::new();
    let core = lm_compiler::core_link_unit()?;
    let mut units = BTreeMap::new();
    units.insert(core.module_path().to_string(), core.as_ref().clone());
    for module in request.env.modules {
        let artifact = lm_bytecode::artifact::decode(module.artifact.as_slice())
            .map_err(|error| format!("the compile artifact did not decode: {error}"))?;
        let (_, linked) = lm_compiler::resolve_artifact(artifact, Some(core.clone()))
            .map_err(|error| format!("the compile artifact did not resolve: {error}"))?;
        for path in linked.paths() {
            let unit = linked
                .unit(path)
                .expect("a resolved path has one link unit")
                .clone();
            if path != lm_bytecode::artifact::CORE_MODULE_PATH {
                env.bind_unit(&unit)
                    .map_err(|error| format!("compile environment error: {error}"))?;
            }
            match units.get(path) {
                Some(found) if found.id() != unit.id() => {
                    return Err(format!("the compile module `{path}` has two identities"));
                }
                Some(_) => {}
                None => {
                    units.insert(path.to_string(), unit);
                }
            }
        }
    }
    for (name, prefix) in request.env.roots {
        env.bind_root(name.as_str(), prefix.as_str())
            .map_err(|error| format!("compile environment error: {error}"))?;
    }
    for definition in request.env.definitions {
        bind_definition(&mut env, request.module_name.as_str(), definition)?;
    }
    let mut options = CompileOptions::new();
    if request.options.late_definitions {
        options = options.late_definitions();
    }
    if request.options.dynamic_result {
        options = options.dynamic_result();
    }
    for name in request.options.late_functions {
        options = options.late_function(name.as_str());
    }
    for name in request.options.late_classes {
        options = options.late_class(name.as_str());
    }
    let source = SourceFile::new(request.source_name.as_str(), request.source.as_str());
    let compiled = compile_module_with_options(
        request.module_name.as_str(),
        &source,
        &env.freeze(),
        request.options.is_main,
        &options,
    )?;
    let mut links = lm_compiler::LinkEnv::new();
    bind_units(&mut links, units)?;
    let unit = links
        .prepare_unit(compiled.path, compiled.module, compiled.interface)
        .map_err(|error| format!("compile link error: {error}"))?;
    links
        .bind_unit(unit)
        .map_err(|error| format!("compile link error: {error}"))?;
    let artifact = links
        .freeze()
        .complete_artifact(request.module_name.as_str())
        .map_err(|error| format!("compile artifact error: {error}"))?;
    let bytes = lm_bytecode::artifact::encode(&artifact)
        .map_err(|error| format!("the compile artifact did not encode: {error}"))?;
    Ok(HostValue::Artifact(bytes.into()))
}

fn bind_units(
    env: &mut lm_compiler::LinkEnv,
    mut units: BTreeMap<String, lm_compiler::LinkUnit>,
) -> Result<(), String> {
    let mut bound = BTreeSet::new();
    while !units.is_empty() {
        let ready = units.iter().find_map(|(path, unit)| {
            unit.dependencies()
                .iter()
                .all(|dependency| bound.contains(dependency.module_path()))
                .then(|| path.clone())
        });
        let Some(path) = ready else {
            return Err("the compile artifact dependencies contain a cycle".to_string());
        };
        let unit = units
            .remove(&path)
            .expect("the selected compile unit remains pending");
        env.bind_unit(unit)
            .map_err(|error| format!("compile link error: {error}"))?;
        bound.insert(path);
    }
    Ok(())
}

fn bind_definition(
    env: &mut CompileEnv,
    module_name: &str,
    definition: lm_vm::HostCompileDefinition,
) -> Result<(), String> {
    if definition.module_name.as_str() != module_name {
        return Err(format!(
            "definition `{}` belongs to module `{}`",
            definition.local_name, definition.module_name
        ));
    }
    let wanted = lm_bytecode::qualified_key(module_name, definition.local_name.as_str());
    if definition.qualified_key.as_str() != wanted {
        return Err(format!(
            "definition `{}` has qualified key `{}`",
            definition.local_name, definition.qualified_key
        ));
    }
    let Some(first) = definition.slots.first() else {
        return Err(format!(
            "definition `{}` has no slot contract",
            definition.local_name
        ));
    };
    let first_artifact = decode_compile_slot(first)?;
    validate_definition_identity(first_artifact.root().module(), &definition)?;

    for slot in &definition.slots {
        if slot.artifact != first.artifact {
            return Err(format!(
                "definition `{}` combines different modules",
                definition.qualified_key
            ));
        }
        let artifact = decode_compile_slot(slot)?;
        let module = artifact.root().module();
        let spec = module
            .slots
            .get(slot.index as usize)
            .ok_or_else(|| "a definition slot index is invalid".to_string())?;
        let (binding, kind) = definition_slot_binding(&artifact, spec, &definition)?;
        let in_family = binding == definition.qualified_key.as_str()
            || binding
                .strip_prefix(definition.qualified_key.as_str())
                .is_some_and(|suffix| suffix.starts_with('.'));
        if !in_family {
            return Err(format!(
                "slot `{binding}` does not belong to definition `{}`",
                definition.qualified_key
            ));
        }
        env.bind_late(&binding, spec.contract_hash, spec.key, kind)
            .map_err(|error| format!("compile environment error: {error}"))?;
    }
    Ok(())
}

fn decode_compile_slot(
    slot: &lm_vm::HostCompileSlot,
) -> Result<lm_bytecode::artifact::Artifact, String> {
    lm_bytecode::artifact::decode(slot.artifact.as_slice())
        .map_err(|error| format!("the definition artifact did not decode: {error}"))
}

fn validate_definition_identity(
    module: &lm_bytecode::Module,
    definition: &lm_vm::HostCompileDefinition,
) -> Result<(), String> {
    let identity = lm_bytecode::identity::module_identity(module)
        .map_err(|error| format!("the definition module has no identity: {error}"))?;
    let class = module
        .classes
        .iter()
        .position(|class| class.key == definition.qualified_key.as_str())
        .map(|index| {
            lm_bytecode::identity::class_definition_hashes(module, &identity, index as u32)
        })
        .transpose()
        .map_err(|error| format!("the definition class has no identity: {error}"))?;
    let function = module
        .bindings
        .iter()
        .find(|binding| {
            binding.key == definition.qualified_key.as_str()
                && binding.class == lm_bytecode::NO_CLASS
        })
        .map(|binding| {
            lm_bytecode::identity::function_definition_hashes(module, &identity, binding.func)
        })
        .transpose()
        .map_err(|error| format!("the definition function has no identity: {error}"))?;
    let Some(found) = class.or(function) else {
        return Err(format!(
            "definition `{}` is absent from its module",
            definition.qualified_key
        ));
    };
    if found.contract != definition.contract_hash {
        return Err(format!(
            "definition `{}` has another contract identity",
            definition.qualified_key
        ));
    }
    if found.implementation != definition.implementation_hash {
        return Err(format!(
            "definition `{}` has another implementation identity",
            definition.qualified_key
        ));
    }
    if identity.semantic_hash != definition.module_hash {
        return Err(format!(
            "definition `{}` has another module identity",
            definition.qualified_key
        ));
    }
    Ok(())
}

fn definition_slot_binding(
    artifact: &lm_bytecode::artifact::Artifact,
    spec: &lm_bytecode::SlotSpec,
    definition: &lm_vm::HostCompileDefinition,
) -> Result<(String, lm_bytecode::interface::IfaceSlotKind), String> {
    let module = artifact.root().module();
    let kind = match spec.contract {
        lm_bytecode::SlotContract::Function(_) => lm_bytecode::interface::IfaceSlotKind::Function,
        lm_bytecode::SlotContract::Method(_) => lm_bytecode::interface::IfaceSlotKind::Method,
        lm_bytecode::SlotContract::Class { .. } => lm_bytecode::interface::IfaceSlotKind::Class,
        _ => return Err("a definition contains a non-code slot".to_string()),
    };
    if let Some(found) = artifact
        .root()
        .interface()
        .slots
        .iter()
        .find(|found| found.key == spec.key)
    {
        if found.kind != kind {
            return Err("a definition slot has another target kind".to_string());
        }
        return Ok((found.binding.clone(), kind));
    }
    match spec.initial {
        Some(lm_bytecode::SlotTarget::Class { class, .. }) => {
            let class = module
                .classes
                .get(class as usize)
                .ok_or_else(|| "a definition class slot has no class".to_string())?;
            Ok((class.key.clone(), kind))
        }
        Some(lm_bytecode::SlotTarget::Function(function)) => {
            let mut bindings: Vec<&str> = module
                .bindings
                .iter()
                .filter(|binding| {
                    binding.func == function
                        && binding.class == lm_bytecode::NO_CLASS
                        && (binding.key == definition.qualified_key.as_str()
                            || binding
                                .key
                                .strip_prefix(definition.qualified_key.as_str())
                                .is_some_and(|suffix| suffix.starts_with('.')))
                })
                .map(|binding| binding.key.as_str())
                .collect();
            bindings.sort_unstable();
            bindings.dedup();
            match bindings.as_slice() {
                [binding] => Ok(((*binding).to_string(), kind)),
                [] => Err("a definition function slot has no binding".to_string()),
                _ => Err("a definition function slot has ambiguous bindings".to_string()),
            }
        }
        None => Err("a definition slot has no initial target".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_and_direct_compilation_match() {
        let path: SharedText = "runtime".into();
        let text: SharedText = "40 + 2\n".into();
        let request = CompileRequest {
            module_name: path.clone(),
            source_name: path.clone(),
            source: text.clone(),
            env: HostCompileEnv {
                modules: Vec::new(),
                roots: Vec::new(),
                definitions: Vec::new(),
            },
            options: HostCompileOptions {
                is_main: true,
                dynamic_result: false,
                late_definitions: false,
                late_functions: Vec::new(),
                late_classes: Vec::new(),
            },
        };
        let HostValue::Artifact(bytes) =
            compile_inner(request).expect("the runtime source compiles")
        else {
            panic!("the compiler returned another value");
        };
        let direct = compile_module_with_options(
            path.as_str(),
            &SourceFile::new(path.as_str(), text.as_str()),
            &CompileEnv::new().freeze(),
            true,
            &CompileOptions::new(),
        )
        .expect("the direct source compiles");
        let artifact = lm_bytecode::artifact::decode(bytes.as_slice())
            .expect("the runtime compiler returns an artifact");
        assert_eq!(artifact.root().module(), &direct.module);
    }
}
