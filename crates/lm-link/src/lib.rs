//! Artifact linking and append-only code publication.
//!
//! A `LinkEnv` resolves exact artifact dependencies.
//! A `CodeArena` relocates each verified unit once.
//! A `CodeNamespace` provides one immutable execution view.
//! Collection and relocation use shared exhaustive table maps.

mod arena;
mod collect;
mod env;
mod reloc_tables;
mod relocate;

pub use arena::{CodeArena, CodeNamespace, DispatchRow, NamespaceId};
pub use env::{
    collect_compiled_unit, resolve_artifact, select_definition_artifact, DefinitionSelection,
    FrozenLinkEnv, LinkEnv, LinkEnvError, LinkError,
};
pub use lm_bytecode::artifact::LinkUnit;
pub use reloc_tables::{CodeRelocation, UnitRelocation};

#[cfg(test)]
mod tests {
    use crate::arena::{contains_index, mark_indices};

    #[test]
    fn sparse_membership_marks_exact_indices() {
        let mut bits = Vec::new();
        mark_indices(&mut bits, &[0, 63, 64, 4097]);
        for index in [0, 63, 64, 4097] {
            assert!(contains_index(&bits, index));
        }
        for index in [1, 62, 65, 4096, 4098, u32::MAX] {
            assert!(!contains_index(&bits, index));
        }
    }
}
