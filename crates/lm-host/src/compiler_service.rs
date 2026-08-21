//! Bounded runtime compilation for the command-line host.

use lm_compiler::{compile_module_with_options, CompileEnv, CompileOptions};
use lm_source::SourceFile;
use lm_vm::{
    CompletionKey, CoreCtor, HostCompileEnv, HostCompileOptions, HostCompletion, HostValue,
    SharedText,
};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::time::Duration;

const MAX_PENDING_COMPILES: usize = 64;

pub(crate) struct CompilerService {
    jobs: SyncSender<Job>,
    completions: Receiver<HostCompletion>,
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
    pub(crate) fn new() -> CompilerService {
        let (jobs, queue) = mpsc::sync_channel(MAX_PENDING_COMPILES);
        let (results, completions) = mpsc::channel();
        std::thread::Builder::new()
            .name("loom-compiler".to_string())
            .spawn(move || compiler_worker(queue, results))
            .expect("the runtime compiler worker starts");
        CompilerService { jobs, completions }
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

    pub(crate) fn poll(&self) -> Option<HostCompletion> {
        self.completions.try_recv().ok()
    }

    pub(crate) fn wait_timeout(
        &self,
        duration: Duration,
    ) -> Result<HostCompletion, mpsc::RecvTimeoutError> {
        self.completions.recv_timeout(duration)
    }
}

fn compiler_worker(jobs: Receiver<Job>, completions: mpsc::Sender<HostCompletion>) {
    while let Ok(job) = jobs.recv() {
        let value = compile(job.request);
        let _ = completions.send(HostCompletion {
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
    for module in request.env.modules {
        let decoded = lm_bytecode::decode(module.artifact.as_slice())
            .map_err(|error| format!("the compile module did not decode: {error}"))?;
        lm_verify::verify_module(&decoded)
            .map_err(|error| format!("the compile module did not verify: {error}"))?;
        let interface = lm_bytecode::interface::decode_interface(module.interface.as_slice())
            .map_err(|error| format!("the compile interface did not decode: {error}"))?;
        if lm_bytecode::interface::encode_interface(&interface) != module.interface.as_slice() {
            return Err("the compile interface is not canonical".to_string());
        }
        let identity = lm_bytecode::identity::module_identity(&decoded)
            .map_err(|error| format!("the compile module has no identity: {error}"))?;
        lm_bytecode::interface::validate_interface(&decoded, &identity, &interface)
            .map_err(|error| format!("the compile interface is invalid: {error}"))?;
        env.bind_interface(interface)
            .map_err(|error| format!("compile environment error: {error}"))?;
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
    Ok(HostValue::Artifact {
        module: compiled.artifact.into(),
        interface: compiled.interface_bytes.into(),
    })
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
    let first_module = decode_compile_slot(first)?;
    validate_definition_identity(&first_module, &definition)?;

    if definition.slot_keys.len() != definition.slots.len() {
        return Err(format!(
            "definition `{}` has misaligned slot keys",
            definition.local_name
        ));
    }
    for (slot, slot_key) in definition.slots.iter().zip(&definition.slot_keys) {
        if slot.artifact != first.artifact {
            return Err(format!(
                "definition `{}` combines different modules",
                definition.qualified_key
            ));
        }
        let module = decode_compile_slot(slot)?;
        let spec = module
            .slots
            .get(slot.index as usize)
            .ok_or_else(|| "a definition slot index is invalid".to_string())?;
        if spec.key != *slot_key {
            return Err(format!(
                "definition `{}` has another slot key",
                definition.local_name
            ));
        }
        let (binding, kind) = definition_slot_binding(&module, slot, spec, &definition)?;
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
        env.bind_late(&binding, [0; 32], spec.key, kind)
            .map_err(|error| format!("compile environment error: {error}"))?;
    }
    Ok(())
}

fn decode_compile_slot(slot: &lm_vm::HostCompileSlot) -> Result<lm_bytecode::Module, String> {
    let module = lm_bytecode::decode(slot.artifact.as_slice())
        .map_err(|error| format!("the definition module did not decode: {error}"))?;
    lm_verify::verify_module(&module)
        .map_err(|error| format!("the definition module did not verify: {error}"))?;
    if let Some(bytes) = &slot.interface {
        let interface = lm_bytecode::interface::decode_interface(bytes.as_slice())
            .map_err(|error| format!("the definition interface did not decode: {error}"))?;
        if lm_bytecode::interface::encode_interface(&interface) != bytes.as_slice() {
            return Err("the definition interface is not canonical".to_string());
        }
        let identity = lm_bytecode::identity::module_identity(&module)
            .map_err(|error| format!("the definition module has no identity: {error}"))?;
        lm_bytecode::interface::validate_interface(&module, &identity, &interface)
            .map_err(|error| format!("the definition interface is invalid: {error}"))?;
    }
    Ok(module)
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
        .and_then(|index| identity.class_hashes.get(index).copied());
    let function = module
        .bindings
        .iter()
        .find(|binding| {
            binding.key == definition.qualified_key.as_str()
                && binding.class == lm_bytecode::NO_CLASS
        })
        .and_then(|binding| identity.func_hashes.get(binding.func as usize).copied());
    let Some(found) = class.or(function) else {
        return Err(format!(
            "definition `{}` is absent from its module",
            definition.qualified_key
        ));
    };
    if found != definition.definition_hash {
        return Err(format!(
            "definition `{}` has another verified identity",
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
    module: &lm_bytecode::Module,
    source: &lm_vm::HostCompileSlot,
    spec: &lm_bytecode::SlotSpec,
    definition: &lm_vm::HostCompileDefinition,
) -> Result<(String, lm_bytecode::interface::IfaceSlotKind), String> {
    let kind = match spec.contract {
        lm_bytecode::SlotContract::Function(_) => lm_bytecode::interface::IfaceSlotKind::Function,
        lm_bytecode::SlotContract::Method(_) => lm_bytecode::interface::IfaceSlotKind::Method,
        lm_bytecode::SlotContract::Class { .. } => lm_bytecode::interface::IfaceSlotKind::Class,
        _ => return Err("a definition contains a non-code slot".to_string()),
    };
    if let Some(bytes) = &source.interface {
        let interface = lm_bytecode::interface::decode_interface(bytes.as_slice())
            .map_err(|error| format!("the definition interface did not decode: {error}"))?;
        if let Some(found) = interface.slots.iter().find(|found| found.key == spec.key) {
            if found.kind != kind {
                return Err("a definition slot has another target kind".to_string());
            }
            return Ok((found.binding.clone(), kind));
        }
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
        let HostValue::Artifact { module, interface } =
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
        assert_eq!(module.as_slice(), direct.artifact);
        assert_eq!(interface.as_slice(), direct.interface_bytes);
    }
}
