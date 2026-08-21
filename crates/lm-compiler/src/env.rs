//! The explicit typed environments: `CompileEnv` and `LinkEnv`.
//!
//! The build tool constructs both. Ordinary development never touches
//! them; they stay the embedding and sandbox path (specification 3.4
//! and 3.6).
//!
//! A `CompileEnv` carries the interfaces a module may name and the
//! root names its `use` lines may start with. A `LinkEnv` carries the
//! compiled modules that fulfill the import slots. Both freeze before
//! use, so no build step mutates an environment another step reads.

use lm_bytecode::interface::{IfaceSlotKind, IfaceSlotSpec, Interface};
use lm_bytecode::Module;
use lm_hir::import::ImportEnv;
use std::collections::{BTreeMap, BTreeSet};

/// A failure to build a compile environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompileEnvError {
    /// The root name is already bound.
    DuplicateRoot(String),
    /// The root name is reserved for a fixed binding.
    ReservedRoot(String),
    /// The root name is not a valid lowercase name.
    InvalidRoot(String),
    /// Two modules claim one module path.
    DuplicateModule(String),
    /// Two late bindings claim one source name.
    DuplicateBinding(String),
}

impl std::fmt::Display for CompileEnvError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompileEnvError::DuplicateRoot(name) => write!(
                f,
                "the root name `{name}` is bound twice; rename the dependency \
                 key in lm.package"
            ),
            CompileEnvError::ReservedRoot(name) => {
                write!(f, "the root name `{name}` is reserved")
            }
            CompileEnvError::InvalidRoot(name) => write!(
                f,
                "`{name}` is not a root name; use a lowercase letter, then \
                 letters, digits, or underscores"
            ),
            CompileEnvError::DuplicateModule(path) => {
                write!(f, "two modules claim the path `{path}`")
            }
            CompileEnvError::DuplicateBinding(name) => {
                write!(f, "two late bindings claim `{name}`")
            }
        }
    }
}

/// The typed compile environment builder.
#[derive(Debug, Clone, Default)]
pub struct CompileEnv {
    env: ImportEnv,
    late: BTreeMap<String, IfaceSlotSpec>,
    static_bindings: BTreeSet<String>,
}

impl CompileEnv {
    pub fn new() -> CompileEnv {
        CompileEnv::default()
    }

    /// Make one interface visible without binding a root name. A
    /// signature of a bound module may name a class of this one.
    ///
    /// Binding the same interface twice is allowed, because a build
    /// may reach one module through two paths. Binding a different
    /// interface at one module path is an error.
    pub fn bind_interface(&mut self, interface: Interface) -> Result<(), CompileEnvError> {
        let path = interface.module_path.clone();
        if let Some(old) = self.env.modules.get(&path) {
            if *old != interface {
                return Err(CompileEnvError::DuplicateModule(path));
            }
            return Ok(());
        }
        for spec in &interface.slots {
            if self.static_bindings.contains(&spec.binding) {
                continue;
            }
            self.insert_late(spec.clone())?;
        }
        self.env.modules.insert(path, interface);
        Ok(())
    }

    fn insert_late(&mut self, spec: IfaceSlotSpec) -> Result<(), CompileEnvError> {
        if let Some(old) = self.late.get(&spec.binding) {
            if old != &spec {
                return Err(CompileEnvError::DuplicateBinding(spec.binding));
            }
            return Ok(());
        }
        self.late.insert(spec.binding.clone(), spec);
        Ok(())
    }

    /// Bind one source name through a stable late slot.
    pub fn bind_late(
        &mut self,
        name: &str,
        contract_hash: [u8; 32],
        key: [u8; 32],
        kind: IfaceSlotKind,
    ) -> Result<(), CompileEnvError> {
        self.static_bindings.remove(name);
        self.insert_late(IfaceSlotSpec {
            binding: name.to_string(),
            contract_hash,
            key,
            kind,
        })
    }

    /// Force one source name to use static linkage.
    pub fn bind_static(&mut self, name: &str) {
        self.late.remove(name);
        self.static_bindings.insert(name.to_string());
    }

    /// Bind one root name to a module path prefix. A `use` line may
    /// start with the root name.
    pub fn bind_root(&mut self, name: &str, prefix: &str) -> Result<(), CompileEnvError> {
        if name == "sys" || name == "std" {
            return Err(CompileEnvError::ReservedRoot(name.to_string()));
        }
        if !crate::manifest::valid_name(name) {
            return Err(CompileEnvError::InvalidRoot(name.to_string()));
        }
        if self.env.roots.contains_key(name) {
            return Err(CompileEnvError::DuplicateRoot(name.to_string()));
        }
        self.env.roots.insert(name.to_string(), prefix.to_string());
        Ok(())
    }

    /// Bind the fixed toolchain standard-library root.
    ///
    /// Only the bundled module catalog uses this path. Package
    /// manifests cannot replace the reserved `std` root.
    pub(crate) fn bind_standard_root(&mut self) {
        self.env.roots.insert("std".to_string(), "std".to_string());
    }

    /// The root names, for a diagnostic.
    pub fn roots(&self) -> Vec<&str> {
        self.env.roots.keys().map(|k| k.as_str()).collect()
    }

    /// Freeze the environment. A frozen environment is the only form
    /// a compilation accepts.
    pub fn freeze(self) -> FrozenCompileEnv {
        FrozenCompileEnv {
            env: self.env,
            late: self.late,
        }
    }
}

/// A frozen compile environment.
#[derive(Debug, Clone, Default)]
pub struct FrozenCompileEnv {
    env: ImportEnv,
    late: BTreeMap<String, IfaceSlotSpec>,
}

impl FrozenCompileEnv {
    pub(crate) fn imports(&self) -> &ImportEnv {
        &self.env
    }

    /// The interface of one module path.
    pub fn interface(&self, path: &str) -> Option<&Interface> {
        self.env.modules.get(path)
    }

    /// The late linkage for one qualified source binding.
    pub fn late_binding(&self, name: &str) -> Option<&IfaceSlotSpec> {
        self.late.get(name)
    }

    pub(crate) fn late_bindings(&self) -> &BTreeMap<String, IfaceSlotSpec> {
        &self.late
    }
}

/// One compiled module a link step may consume.
#[derive(Debug, Clone)]
pub struct LinkUnit {
    pub path: String,
    pub module: Module,
    pub interface: Interface,
}

/// A failure to build a link environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkEnvError {
    DuplicateModule(String),
}

impl std::fmt::Display for LinkEnvError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LinkEnvError::DuplicateModule(path) => {
                write!(f, "the module `{path}` is bound twice")
            }
        }
    }
}

/// The typed link environment builder: the modules that fulfill the
/// import slots.
#[derive(Debug, Clone, Default)]
pub struct LinkEnv {
    units: BTreeMap<String, LinkUnit>,
}

impl LinkEnv {
    pub fn new() -> LinkEnv {
        LinkEnv::default()
    }

    /// Bind one compiled module. Binding a path twice is an error.
    pub fn bind(&mut self, unit: LinkUnit) -> Result<(), LinkEnvError> {
        let path = unit.path.clone();
        if self.units.contains_key(&path) {
            return Err(LinkEnvError::DuplicateModule(path));
        }
        self.units.insert(path, unit);
        Ok(())
    }

    pub fn freeze(self) -> FrozenLinkEnv {
        FrozenLinkEnv { units: self.units }
    }
}

/// A frozen link environment.
#[derive(Debug, Clone, Default)]
pub struct FrozenLinkEnv {
    units: BTreeMap<String, LinkUnit>,
}

impl FrozenLinkEnv {
    pub fn unit(&self, path: &str) -> Option<&LinkUnit> {
        self.units.get(path)
    }

    pub fn paths(&self) -> Vec<&str> {
        self.units.keys().map(|k| k.as_str()).collect()
    }
}
