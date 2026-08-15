//! Core-definition lookup for one decoded module.
//!
//! The per-module core copy is positional until week-5 hash linking.
//! This module is the ONE place that resolves the pinned core enums
//! inside a module. The verifier and the VM both use it, so they
//! agree on the class indices behind operation replies and VM events.
//!
//! Resolution takes the LAST class whose name matches, because the
//! compiler appends the core copy after the user definitions. A found
//! family must have the exact pinned shape; a name match with a wrong
//! shape counts as absent. Week 5 replaces this name lookup with
//! definition hashes.

use crate::{BcClassKind, BcType, Module};

/// The shape of one required arm field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FieldShape {
    Var(u32),
    Str,
    Fault,
    Request,
}

/// The resolved core class indices of one module. Each entry is the
/// class index of one enum case. `None` marks an absent family.
#[derive(Debug, Clone, Copy, Default)]
pub struct CoreLayout {
    pub option_some: Option<u32>,
    pub option_none: Option<u32>,
    pub result_ok: Option<u32>,
    pub result_err: Option<u32>,
    pub io_error_failed: Option<u32>,
    pub run_done: Option<u32>,
    pub run_fault: Option<u32>,
    pub step_ran: Option<u32>,
    pub step_waiting: Option<u32>,
    pub step_done: Option<u32>,
    pub step_fault: Option<u32>,
    pub drive_asked: Option<u32>,
    pub drive_done: Option<u32>,
    pub drive_fault: Option<u32>,
    /// The enum parent class indices, aligned with the arms above.
    pub option: Option<u32>,
    pub result: Option<u32>,
    pub io_error: Option<u32>,
    pub run_result: Option<u32>,
    pub step_event: Option<u32>,
    pub drive_event: Option<u32>,
}

/// Find the last class with one name.
fn find_class(module: &Module, name: &str) -> Option<u32> {
    module
        .classes
        .iter()
        .rposition(|c| c.name == name)
        .map(|i| i as u32)
}

/// True when a field type index matches the required shape.
fn field_matches(module: &Module, ty: u32, shape: FieldShape) -> bool {
    match module.types.get(ty as usize) {
        Some(t) => match shape {
            FieldShape::Var(i) => *t == BcType::Var(i),
            FieldShape::Str => *t == BcType::Str,
            FieldShape::Fault => *t == BcType::Fault,
            FieldShape::Request => *t == BcType::Request,
        },
        None => false,
    }
}

/// Validate one arm class against its pinned shape.
fn valid_arm(module: &Module, parent: u32, arm: u32, fields: &[FieldShape]) -> bool {
    let Some(class) = module.classes.get(arm as usize) else {
        return false;
    };
    class.kind == BcClassKind::Case
        && class.parent() == Some(parent)
        && class.type_params == module.classes[parent as usize].type_params
        && class.fields.len() == fields.len()
        && class
            .fields
            .iter()
            .zip(fields.iter())
            .all(|((_, ty), shape)| field_matches(module, *ty, *shape))
}

/// Resolve one enum family: the parent index and the arm indices in
/// the given order. A wrong shape counts as absent.
fn find_family(
    module: &Module,
    name: &str,
    type_params: u32,
    arms: &[(&str, &[FieldShape])],
) -> Option<(u32, Vec<u32>)> {
    let parent = find_class(module, name)?;
    let meta = &module.classes[parent as usize];
    if meta.kind != BcClassKind::Abstract || meta.type_params != type_params {
        return None;
    }
    let mut found = Vec::with_capacity(arms.len());
    for (arm_name, fields) in arms {
        let full = format!("{name}.{arm_name}");
        let arm = find_class(module, &full)?;
        if !valid_arm(module, parent, arm, fields) {
            return None;
        }
        found.push(arm);
    }
    Some((parent, found))
}

/// Resolve the core layout of one module. Absent or malformed
/// families resolve to `None`; the verifier requires presence exactly
/// where an instruction needs a family.
pub fn core_layout(module: &Module) -> CoreLayout {
    let mut layout = CoreLayout::default();
    if let Some((parent, arms)) = find_family(
        module,
        "Option",
        1,
        &[("Some", &[FieldShape::Var(0)]), ("None", &[])],
    ) {
        layout.option = Some(parent);
        layout.option_some = Some(arms[0]);
        layout.option_none = Some(arms[1]);
    }
    if let Some((parent, arms)) = find_family(
        module,
        "Result",
        2,
        &[
            ("Ok", &[FieldShape::Var(0)]),
            ("Err", &[FieldShape::Var(1)]),
        ],
    ) {
        layout.result = Some(parent);
        layout.result_ok = Some(arms[0]);
        layout.result_err = Some(arms[1]);
    }
    if let Some((parent, arms)) =
        find_family(module, "IoError", 0, &[("Failed", &[FieldShape::Str])])
    {
        layout.io_error = Some(parent);
        layout.io_error_failed = Some(arms[0]);
    }
    if let Some((parent, arms)) = find_family(
        module,
        "RunResult",
        1,
        &[
            ("Done", &[FieldShape::Var(0)]),
            ("Fault", &[FieldShape::Fault]),
        ],
    ) {
        layout.run_result = Some(parent);
        layout.run_done = Some(arms[0]);
        layout.run_fault = Some(arms[1]);
    }
    if let Some((parent, arms)) = find_family(
        module,
        "StepEvent",
        1,
        &[
            ("Ran", &[]),
            ("Waiting", &[]),
            ("Done", &[FieldShape::Var(0)]),
            ("Fault", &[FieldShape::Fault]),
        ],
    ) {
        layout.step_event = Some(parent);
        layout.step_ran = Some(arms[0]);
        layout.step_waiting = Some(arms[1]);
        layout.step_done = Some(arms[2]);
        layout.step_fault = Some(arms[3]);
    }
    if let Some((parent, arms)) = find_family(
        module,
        "DriveEvent",
        1,
        &[
            ("Asked", &[FieldShape::Request]),
            ("Done", &[FieldShape::Var(0)]),
            ("Fault", &[FieldShape::Fault]),
        ],
    ) {
        layout.drive_event = Some(parent);
        layout.drive_asked = Some(arms[0]);
        layout.drive_done = Some(arms[1]);
        layout.drive_fault = Some(arms[2]);
    }
    layout
}
