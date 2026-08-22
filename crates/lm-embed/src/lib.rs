//! The checked Rust embedding boundary for Loom host operations.
//!
//! One registry binds an immutable ABI bundle to its handlers.
//! The registry supplies owned values to each handler.

use lm_abi::{AbiBundle, GroupSpec, OperationSpec, ResourceDescriptor};
use lm_vm::{
    CompletionKey, Host, HostArg, HostCompletion, HostStart, HostValue, LoadedModule, VmConfig,
    World,
};
use std::collections::BTreeMap;
use std::fmt;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{mpsc, Arc, Mutex};

type SyncHandler = Box<dyn FnMut(Vec<HostArg>) -> Result<HostValue, String> + Send>;
type CheckedHandler =
    Box<dyn FnMut(Vec<HostArg>, ReplyBuilder) -> Result<HostValue, String> + Send>;
type AsyncHandler = Box<dyn FnMut(Vec<HostArg>, Completion) -> Result<(), String> + Send>;
type CancelHandler = Box<dyn FnOnce() + Send>;
type ResourceCleanup = Box<dyn FnMut(lm_vm::HostResource) -> bool + Send>;

enum Handler {
    Sync(SyncHandler),
    Checked(CheckedHandler),
    Async(AsyncHandler),
}

fn value_matches(value: &HostValue, ty: lm_abi::AbiType) -> bool {
    use lm_abi::{AbiConstructor, AbiPrimitive, AbiType};
    match (value, ty) {
        (HostValue::Unit, AbiType::Primitive(AbiPrimitive::Unit))
        | (HostValue::Bool(_), AbiType::Primitive(AbiPrimitive::Bool))
        | (HostValue::Int(_), AbiType::Primitive(AbiPrimitive::Int))
        | (HostValue::Str(_), AbiType::Primitive(AbiPrimitive::String))
        | (HostValue::Bytes(_), AbiType::Primitive(AbiPrimitive::Bytes)) => true,
        (HostValue::Resource(resource), AbiType::Resource(identity)) => resource.kind == identity,
        (HostValue::List(values), AbiType::List(element)) => {
            values.iter().all(|value| value_matches(value, *element))
        }
        (HostValue::Tuple(values), AbiType::Tuple(elements)) => {
            values.len() == elements.len()
                && values
                    .iter()
                    .zip(elements)
                    .all(|(value, ty)| value_matches(value, *ty))
        }
        (
            HostValue::Ctor(lm_vm::CoreCtor::Some, values),
            AbiType::Apply(AbiConstructor::Option, arguments),
        ) => values.len() == 1 && arguments.len() == 1 && value_matches(&values[0], arguments[0]),
        (
            HostValue::Ctor(lm_vm::CoreCtor::None, values),
            AbiType::Apply(AbiConstructor::Option, arguments),
        ) => values.is_empty() && arguments.len() == 1,
        (
            HostValue::Ctor(lm_vm::CoreCtor::Ok, values),
            AbiType::Apply(AbiConstructor::Result, arguments),
        ) => values.len() == 1 && arguments.len() == 2 && value_matches(&values[0], arguments[0]),
        (
            HostValue::Ctor(lm_vm::CoreCtor::Err, values),
            AbiType::Apply(AbiConstructor::Result, arguments),
        ) => values.len() == 1 && arguments.len() == 2 && value_matches(&values[0], arguments[1]),
        (
            HostValue::Ctor(lm_vm::CoreCtor::Pair, values),
            AbiType::Apply(AbiConstructor::Pair, arguments),
        ) => {
            values.len() == 2
                && arguments.len() == 2
                && value_matches(&values[0], arguments[0])
                && value_matches(&values[1], arguments[1])
        }
        _ => false,
    }
}

/// A checked builder for one declared operation reply.
#[derive(Clone, Copy)]
pub struct ReplyBuilder {
    schema: lm_abi::AbiType,
}

impl ReplyBuilder {
    /// Check one reply against the operation schema.
    pub fn value(&self, value: HostValue) -> Result<HostValue, String> {
        if value_matches(&value, self.schema) {
            Ok(value)
        } else {
            Err(format!(
                "the host reply does not match `{}`",
                self.schema.text()
            ))
        }
    }

    /// Build a unit reply.
    pub fn unit(&self) -> Result<HostValue, String> {
        self.value(HostValue::Unit)
    }

    /// Build an integer reply.
    pub fn int(&self, value: i64) -> Result<HostValue, String> {
        self.value(HostValue::Int(value))
    }

    /// Build a Boolean reply.
    pub fn bool(&self, value: bool) -> Result<HostValue, String> {
        self.value(HostValue::Bool(value))
    }
}

struct NamedHandler {
    name: String,
    handler: Handler,
}

/// A failure to use one completion handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionError {
    /// Another action already completed or canceled the request.
    Inactive,
    /// The receiving host no longer exists.
    Disconnected,
}

impl fmt::Display for CompletionError {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CompletionError::Inactive => output.write_str("the completion is not active"),
            CompletionError::Disconnected => output.write_str("the completion host is gone"),
        }
    }
}

struct CompletionState {
    status: AtomicU8,
    cancel: Mutex<Option<CancelHandler>>,
}

struct QueuedCompletion {
    completion: HostCompletion,
    state: Arc<CompletionState>,
}

/// A single-use asynchronous completion handle.
pub struct Completion {
    key: CompletionKey,
    token: u64,
    sender: mpsc::Sender<QueuedCompletion>,
    state: Arc<CompletionState>,
}

impl Completion {
    /// Register one callback for host-side cancellation.
    pub fn on_cancel(
        &mut self,
        callback: impl FnOnce() + Send + 'static,
    ) -> Result<(), CompletionError> {
        if self.state.status.load(Ordering::Acquire) != 0 {
            return Err(CompletionError::Inactive);
        }
        let mut slot = self
            .state
            .cancel
            .lock()
            .map_err(|_| CompletionError::Inactive)?;
        if slot.is_some() {
            return Err(CompletionError::Inactive);
        }
        *slot = Some(Box::new(callback));
        Ok(())
    }

    /// Complete the request with one declared guest value.
    pub fn complete(self, value: HostValue) -> Result<(), CompletionError> {
        self.finish(Ok(value))
    }

    /// Complete the request with one host implementation defect.
    pub fn fail(self, message: impl Into<String>) -> Result<(), CompletionError> {
        self.finish(Err(message.into()))
    }

    fn finish(self, result: Result<HostValue, String>) -> Result<(), CompletionError> {
        self.state
            .status
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| CompletionError::Inactive)?;
        let completion = HostCompletion {
            key: self.key,
            token: self.token,
            result,
        };
        self.sender
            .send(QueuedCompletion {
                completion,
                state: self.state,
            })
            .map_err(|_| CompletionError::Disconnected)
    }
}

/// A checked builder for one host registry.
#[derive(Default)]
pub struct RegistryBuilder {
    abi: lm_abi::AbiBundleBuilder,
    handlers: Vec<NamedHandler>,
    resource_cleanups: Vec<(String, ResourceCleanup)>,
}

impl RegistryBuilder {
    /// Create a builder that extends the standard ABI bundle.
    pub fn new() -> RegistryBuilder {
        RegistryBuilder::default()
    }

    /// Add one extension group.
    pub fn add_group(&mut self, group: GroupSpec) -> &mut RegistryBuilder {
        self.abi.add_group(group);
        self
    }

    /// Add one extension operation.
    pub fn add_operation(&mut self, operation: OperationSpec) -> &mut RegistryBuilder {
        self.abi.add_operation(operation);
        self
    }

    /// Add one extension resource descriptor.
    pub fn add_resource(&mut self, resource: ResourceDescriptor) -> &mut RegistryBuilder {
        self.abi.add_resource(resource);
        self
    }

    /// Register one cleanup handler for an extension resource kind.
    pub fn clean_resource(
        &mut self,
        name: impl Into<String>,
        cleanup: impl FnMut(lm_vm::HostResource) -> bool + Send + 'static,
    ) -> &mut RegistryBuilder {
        self.resource_cleanups
            .push((name.into(), Box::new(cleanup)));
        self
    }

    /// Serve one operation with a synchronous handler.
    pub fn serve(
        &mut self,
        name: impl Into<String>,
        handler: impl FnMut(Vec<HostArg>) -> Result<HostValue, String> + Send + 'static,
    ) -> &mut RegistryBuilder {
        self.handlers.push(NamedHandler {
            name: name.into(),
            handler: Handler::Sync(Box::new(handler)),
        });
        self
    }

    /// Serve one operation with a checked synchronous handler.
    pub fn serve_checked(
        &mut self,
        name: impl Into<String>,
        handler: impl FnMut(Vec<HostArg>, ReplyBuilder) -> Result<HostValue, String> + Send + 'static,
    ) -> &mut RegistryBuilder {
        self.handlers.push(NamedHandler {
            name: name.into(),
            handler: Handler::Checked(Box::new(handler)),
        });
        self
    }

    /// Serve one operation with an asynchronous handler.
    pub fn serve_async(
        &mut self,
        name: impl Into<String>,
        handler: impl FnMut(Vec<HostArg>, Completion) -> Result<(), String> + Send + 'static,
    ) -> &mut RegistryBuilder {
        self.handlers.push(NamedHandler {
            name: name.into(),
            handler: Handler::Async(Box::new(handler)),
        });
        self
    }

    /// Validate the bundle and bind every named handler.
    pub fn build(self) -> Result<Registry, String> {
        let bundle = self.abi.build()?;
        let mut handlers = BTreeMap::new();
        for named in self.handlers {
            let slot = bundle
                .op_by_name(&named.name)
                .ok_or_else(|| format!("operation `{}` is not in the ABI bundle", named.name))?;
            if handlers.insert(slot, named.handler).is_some() {
                return Err(format!("operation `{}` has two host handlers", named.name));
            }
        }
        let (sender, receiver) = mpsc::channel();
        let mut resource_cleanups = BTreeMap::new();
        for (name, cleanup) in self.resource_cleanups {
            let slot = bundle
                .resource_by_name(&name)
                .ok_or_else(|| format!("resource `{name}` is not in the ABI bundle"))?;
            let identity = bundle
                .resource(slot)
                .ok_or_else(|| format!("resource `{name}` has no descriptor"))?
                .identity();
            if resource_cleanups.insert(identity, cleanup).is_some() {
                return Err(format!("resource `{name}` has two cleanup handlers"));
            }
        }
        Ok(Registry {
            bundle,
            handlers,
            sender,
            receiver,
            pending: BTreeMap::new(),
            next_token: 1,
            resource_cleanups,
        })
    }
}

/// One ABI bundle and its registered host handlers.
pub struct Registry {
    bundle: Arc<AbiBundle>,
    handlers: BTreeMap<u32, Handler>,
    sender: mpsc::Sender<QueuedCompletion>,
    receiver: mpsc::Receiver<QueuedCompletion>,
    pending: BTreeMap<u64, Arc<CompletionState>>,
    next_token: u64,
    resource_cleanups: BTreeMap<[u8; 32], ResourceCleanup>,
}

impl Registry {
    /// Return the bundle used by this registry.
    pub fn bundle(&self) -> &Arc<AbiBundle> {
        &self.bundle
    }

    /// Compile one source program against this registry bundle.
    pub fn compile(
        &self,
        path: &str,
        name: &str,
        text: &str,
    ) -> Result<lm_compiler::CompiledModule, String> {
        let source = lm_source::SourceFile::new(name, text);
        let env = lm_compiler::CompileEnv::new().freeze();
        lm_compiler::compile_module_with_bundle(path, &source, &env, true, &self.bundle)
    }

    /// Decode and verify one artifact against this registry bundle.
    pub fn load(&self, bytes: &[u8]) -> Result<LoadedModule, lm_vm::LoadError> {
        lm_vm::load_bytes_with_bundle(bytes, &self.bundle)
    }

    /// Create one world after checking its bundle identity.
    pub fn world(self, loaded: &LoadedModule, config: VmConfig) -> Result<World, String> {
        if loaded.bundle().digest() != self.bundle.digest() {
            return Err("the loaded module uses another ABI bundle".to_string());
        }
        Ok(World::new(loaded, config, Box::new(self)))
    }

    fn take_token(&mut self) -> Option<u64> {
        let token = self.next_token;
        self.next_token = token.checked_add(1)?;
        Some(token)
    }

    fn accept(&mut self, queued: QueuedCompletion) -> Option<HostCompletion> {
        let token = queued.completion.token;
        let state = self.pending.remove(&token)?;
        if !Arc::ptr_eq(&state, &queued.state) {
            return None;
        }
        Some(queued.completion)
    }
}

impl Host for Registry {
    fn start(&mut self, key: CompletionKey, op: u32, args: Vec<HostArg>) -> HostStart {
        let Some(mut handler) = self.handlers.remove(&op) else {
            let name = self.bundle.op_name(op).unwrap_or("<unknown>");
            return HostStart::Failed(format!("no host implementation serves `{name}`"));
        };
        let result = match &mut handler {
            Handler::Sync(handler) => match handler(args) {
                Ok(value) => HostStart::Completed(value),
                Err(message) => HostStart::Failed(message),
            },
            Handler::Checked(handler) => {
                if let Some(operation) = self.bundle.op(op) {
                    match handler(
                        args,
                        ReplyBuilder {
                            schema: operation.reply,
                        },
                    ) {
                        Ok(value) => HostStart::Completed(value),
                        Err(message) => HostStart::Failed(message),
                    }
                } else {
                    HostStart::Failed("the operation slot is invalid".to_string())
                }
            }
            Handler::Async(handler) => {
                if let Some(token) = self.take_token() {
                    let state = Arc::new(CompletionState {
                        status: AtomicU8::new(0),
                        cancel: Mutex::new(None),
                    });
                    self.pending.insert(token, state.clone());
                    let completion = Completion {
                        key,
                        token,
                        sender: self.sender.clone(),
                        state,
                    };
                    match handler(args, completion) {
                        Ok(()) => HostStart::Waiting(token),
                        Err(message) => {
                            self.cancel(token);
                            HostStart::Failed(message)
                        }
                    }
                } else {
                    HostStart::Failed("the completion token space is exhausted".to_string())
                }
            }
        };
        self.handlers.insert(op, handler);
        result
    }

    fn poll(&mut self) -> Option<HostCompletion> {
        loop {
            let queued = self.receiver.try_recv().ok()?;
            if let Some(completion) = self.accept(queued) {
                return Some(completion);
            }
        }
    }

    fn wait(&mut self) -> Option<HostCompletion> {
        loop {
            if self.pending.is_empty() {
                return None;
            }
            let queued = self.receiver.recv().ok()?;
            if let Some(completion) = self.accept(queued) {
                return Some(completion);
            }
        }
    }

    fn cancel(&mut self, token: u64) -> bool {
        let Some(state) = self.pending.remove(&token) else {
            return false;
        };
        if state
            .status
            .compare_exchange(0, 2, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            if let Ok(mut callback) = state.cancel.lock() {
                if let Some(callback) = callback.take() {
                    callback();
                }
            }
        }
        true
    }

    fn close_resource(&mut self, resource: lm_vm::HostResource) -> bool {
        self.resource_cleanups
            .get_mut(&resource.kind)
            .is_some_and(|cleanup| cleanup(resource))
    }
}
