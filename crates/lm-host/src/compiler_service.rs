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
    pub path: SharedText,
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
    let source = SourceFile::new(request.path.as_str(), request.source.as_str());
    let compiled = compile_module_with_options(
        request.path.as_str(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_and_direct_compilation_match() {
        let path: SharedText = "runtime".into();
        let text: SharedText = "40 + 2\n".into();
        let request = CompileRequest {
            path: path.clone(),
            source: text.clone(),
            env: HostCompileEnv {
                modules: Vec::new(),
                roots: Vec::new(),
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
