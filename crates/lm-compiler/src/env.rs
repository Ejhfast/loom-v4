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

use lm_bytecode::interface::Interface;
use lm_bytecode::Module;
use lm_hir::import::ImportEnv;
use std::collections::BTreeMap;

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
        }
    }
}

/// The typed compile environment builder.
#[derive(Debug, Clone, Default)]
pub struct CompileEnv {
    env: ImportEnv,
}

impl CompileEnv {
    pub fn new() -> CompileEnv {
        CompileEnv::default()
    }

    /// Make one interface visible without binding a root name. A
    /// signature of a bound module may name a class of this one.
    pub fn bind_interface(&mut self, interface: Interface) -> Result<(), CompileEnvError> {
        let path = interface.module_path.clone();
        if let Some(old) = self.env.modules.insert(path.clone(), interface) {
            if old.module_path != path {
                return Err(CompileEnvError::DuplicateModule(path));
            }
        }
        Ok(())
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

    /// The root names, for a diagnostic.
    pub fn roots(&self) -> Vec<&str> {
        self.env.roots.keys().map(|k| k.as_str()).collect()
    }

    /// Freeze the environment. A frozen environment is the only form
    /// a compilation accepts.
    pub fn freeze(self) -> FrozenCompileEnv {
        FrozenCompileEnv { env: self.env }
    }
}

/// A frozen compile environment.
#[derive(Debug, Clone, Default)]
pub struct FrozenCompileEnv {
    env: ImportEnv,
}

impl FrozenCompileEnv {
    pub(crate) fn imports(&self) -> &ImportEnv {
        &self.env
    }

    /// The interface of one module path.
    pub fn interface(&self, path: &str) -> Option<&Interface> {
        self.env.modules.get(path)
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
