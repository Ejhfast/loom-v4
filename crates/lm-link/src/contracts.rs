//! Structural comparisons for unresolved import declarations.

use lm_bytecode::identity::ModuleIdentity;
use lm_bytecode::{BcClass, BcConformance, BcInterfaceUse, BcRow, Module, NO_PARENT};

struct ContractView<'a> {
    module: &'a Module,
    identity: &'a ModuleIdentity,
}

pub(crate) fn function_difference(
    left_module: &Module,
    left_identity: &ModuleIdentity,
    left: u32,
    right_module: &Module,
    right_identity: &ModuleIdentity,
    right: u32,
) -> Option<&'static str> {
    let left_view = ContractView {
        module: left_module,
        identity: left_identity,
    };
    let right_view = ContractView {
        module: right_module,
        identity: right_identity,
    };
    function_difference_in(&left_view, left, &right_view, right)
}

pub(crate) fn class_difference(
    left_module: &Module,
    left_identity: &ModuleIdentity,
    left: u32,
    right_module: &Module,
    right_identity: &ModuleIdentity,
    right: u32,
) -> Option<&'static str> {
    let left_view = ContractView {
        module: left_module,
        identity: left_identity,
    };
    let right_view = ContractView {
        module: right_module,
        identity: right_identity,
    };
    let left_class = left_module.classes.get(left as usize)?;
    let right_class = right_module.classes.get(right as usize)?;
    if !same_class_layout(&left_view, left_class, &right_view, right_class) {
        return Some("the class layout differs");
    }
    let left_bounds = left_module.class_bounds.get(left as usize)?;
    let right_bounds = right_module.class_bounds.get(right as usize)?;
    if !same_bounds(&left_view, left_bounds, &right_view, right_bounds) {
        return Some("the generic bounds differ");
    }
    if !same_conformance_sets(&left_view, left, &right_view, right) {
        return Some("the conformance set differs");
    }
    if !same_enum_arms(&left_view, left, &right_view, right) {
        return Some("the enum arm set differs");
    }
    None
}

fn function_difference_in(
    left_view: &ContractView<'_>,
    left_index: u32,
    right_view: &ContractView<'_>,
    right_index: u32,
) -> Option<&'static str> {
    let left = left_view.module.funcs.get(left_index as usize)?;
    let right = right_view.module.funcs.get(right_index as usize)?;
    if left.type_params != right.type_params {
        return Some("the type parameter count differs");
    }
    if left.effect_params != right.effect_params {
        return Some("the effect parameter count differs");
    }
    if left.param_muts != right.param_muts {
        return Some("the parameter mutability differs");
    }
    if !same_types(left_view, &left.params, right_view, &right.params) {
        return Some("the parameter types differ");
    }
    if !same_type(left_view, left.ret, right_view, right.ret) {
        return Some("the result type differs");
    }
    if !same_row(left_view, &left.row, right_view, &right.row) {
        return Some("the effect row differs");
    }
    if !same_types(left_view, &left.captures, right_view, &right.captures) {
        return Some("the capture types differ");
    }
    let left_bounds = left_view.module.func_bounds.get(left_index as usize)?;
    let right_bounds = right_view.module.func_bounds.get(right_index as usize)?;
    if !same_bounds(left_view, left_bounds, right_view, right_bounds) {
        return Some("the generic bounds differ");
    }
    None
}

fn same_class_layout(
    left_view: &ContractView<'_>,
    left: &BcClass,
    right_view: &ContractView<'_>,
    right: &BcClass,
) -> bool {
    left.name == right.name
        && left.key == right.key
        && left.is_final == right.is_final
        && left.is_frozen == right.is_frozen
        && left.type_params == right.type_params
        && left.kind == right.kind
        && same_parent(left_view, left, right_view, right)
        && same_types(left_view, &left.parent_args, right_view, &right.parent_args)
        && same_fields(left_view, &left.fields, right_view, &right.fields)
        && same_methods(left_view, &left.methods, right_view, &right.methods)
}

fn same_parent(
    left_view: &ContractView<'_>,
    left: &BcClass,
    right_view: &ContractView<'_>,
    right: &BcClass,
) -> bool {
    match (left.parent, right.parent) {
        (NO_PARENT, NO_PARENT) => true,
        (NO_PARENT, _) | (_, NO_PARENT) => false,
        (left, right) => class_key(left_view, left) == class_key(right_view, right),
    }
}

fn same_fields(
    left_view: &ContractView<'_>,
    left: &[(String, u32)],
    right_view: &ContractView<'_>,
    right: &[(String, u32)],
) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|((left_name, left_type), (right_name, right_type))| {
                left_name == right_name && same_type(left_view, *left_type, right_view, *right_type)
            })
}

fn same_methods(
    left_view: &ContractView<'_>,
    left: &[(u32, u32)],
    right_view: &ContractView<'_>,
    right: &[(u32, u32)],
) -> bool {
    left.len() == right.len()
        && left.iter().zip(right).all(
            |((left_selector, left_function), (right_selector, right_function))| {
                selector(left_view, *left_selector) == selector(right_view, *right_selector)
                    && function_difference_in(
                        left_view,
                        *left_function,
                        right_view,
                        *right_function,
                    )
                    .is_none()
            },
        )
}

fn same_conformance_sets(
    left_view: &ContractView<'_>,
    left_class: u32,
    right_view: &ContractView<'_>,
    right_class: u32,
) -> bool {
    let left: Vec<&BcConformance> = left_view
        .module
        .conformances
        .iter()
        .filter(|item| item.class == left_class)
        .collect();
    let right: Vec<&BcConformance> = right_view
        .module
        .conformances
        .iter()
        .filter(|item| item.class == right_class)
        .collect();
    left.len() == right.len()
        && left.iter().all(|left_item| {
            right
                .iter()
                .any(|right_item| same_conformance(left_view, left_item, right_view, right_item))
        })
}

fn same_conformance(
    left_view: &ContractView<'_>,
    left: &BcConformance,
    right_view: &ContractView<'_>,
    right: &BcConformance,
) -> bool {
    same_interface_use(left_view, &left.application, right_view, &right.application)
        && left.premises.len() == right.premises.len()
        && left
            .premises
            .iter()
            .zip(&right.premises)
            .all(|(left, right)| {
                left.param == right.param
                    && same_interface_uses(left_view, &left.bounds, right_view, &right.bounds)
            })
        && same_types(left_view, &left.associated, right_view, &right.associated)
        && left.method_overrides == right.method_overrides
}

fn same_enum_arms(
    left_view: &ContractView<'_>,
    left_parent: u32,
    right_view: &ContractView<'_>,
    right_parent: u32,
) -> bool {
    enum_arm_keys(left_view, left_parent) == enum_arm_keys(right_view, right_parent)
}

fn enum_arm_keys<'a>(view: &'a ContractView<'_>, parent: u32) -> Vec<&'a str> {
    let mut arms: Vec<&str> = view
        .module
        .classes
        .iter()
        .filter(|class| class.parent == parent)
        .filter(|class| class.kind == lm_bytecode::BcClassKind::Case)
        .map(|class| class.key.as_str())
        .collect();
    arms.sort_unstable();
    arms
}

fn same_bounds(
    left_view: &ContractView<'_>,
    left: &[Vec<BcInterfaceUse>],
    right_view: &ContractView<'_>,
    right: &[Vec<BcInterfaceUse>],
) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| same_interface_uses(left_view, left, right_view, right))
}

fn same_interface_uses(
    left_view: &ContractView<'_>,
    left: &[BcInterfaceUse],
    right_view: &ContractView<'_>,
    right: &[BcInterfaceUse],
) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| same_interface_use(left_view, left, right_view, right))
}

fn same_interface_use(
    left_view: &ContractView<'_>,
    left: &BcInterfaceUse,
    right_view: &ContractView<'_>,
    right: &BcInterfaceUse,
) -> bool {
    interface_key(left_view, left.interface) == interface_key(right_view, right.interface)
        && interface_hash(left_view, left.interface) == interface_hash(right_view, right.interface)
        && same_types(left_view, &left.types, right_view, &right.types)
        && left.rows.len() == right.rows.len()
        && left
            .rows
            .iter()
            .zip(&right.rows)
            .all(|(left, right)| same_row(left_view, left, right_view, right))
}

fn same_types(
    left_view: &ContractView<'_>,
    left: &[u32],
    right_view: &ContractView<'_>,
    right: &[u32],
) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| same_type(left_view, *left, right_view, *right))
}

fn same_type(
    left_view: &ContractView<'_>,
    left: u32,
    right_view: &ContractView<'_>,
    right: u32,
) -> bool {
    left_view.identity.type_hashes.get(left as usize)
        == right_view.identity.type_hashes.get(right as usize)
}

fn same_row(
    left_view: &ContractView<'_>,
    left: &[BcRow],
    right_view: &ContractView<'_>,
    right: &[BcRow],
) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| match (left, right) {
                (BcRow::Var(left), BcRow::Var(right)) => left == right,
                (BcRow::Op(left), BcRow::Op(right)) => {
                    string(left_view, *left) == string(right_view, *right)
                }
                _ => false,
            })
}

fn string<'a>(view: &'a ContractView<'_>, index: u32) -> Option<&'a str> {
    view.module.strings.get(index as usize).map(String::as_str)
}

fn selector<'a>(view: &'a ContractView<'_>, index: u32) -> Option<&'a str> {
    view.module
        .selectors
        .get(index as usize)
        .map(String::as_str)
}

fn class_key<'a>(view: &'a ContractView<'_>, index: u32) -> Option<&'a str> {
    view.module
        .classes
        .get(index as usize)
        .map(|class| class.key.as_str())
}

fn interface_key<'a>(view: &'a ContractView<'_>, index: u32) -> Option<&'a str> {
    view.module
        .interfaces
        .get(index as usize)
        .map(|interface| interface.key.as_str())
}

fn interface_hash(view: &ContractView<'_>, index: u32) -> Option<[u8; 32]> {
    view.identity.interface_hashes.get(index as usize).copied()
}
