//! Immutable operation bundles for compilation and execution.
//!
//! Standard operations keep their existing dense slots. Extension
//! operations follow them in stable identity order.

use crate::{sha256, AbiType, OpDef, OpKind, SnapshotClass, GROUPS, GROUP_MEMBERS, OPS};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, OnceLock};

/// The transfer rule for one operation boundary value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoundaryMode {
    /// Copy the value into the receiving owner.
    Copy,
    /// Move the value into the receiving owner.
    Move,
}

impl BoundaryMode {
    fn tag(self) -> &'static str {
        match self {
            BoundaryMode::Copy => "copy",
            BoundaryMode::Move => "move",
        }
    }
}

/// One immutable operation in a completed ABI bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleOp {
    pub name: String,
    pub group: String,
    pub member: String,
    pub version: u32,
    pub kind: OpKind,
    pub params: Vec<AbiType>,
    pub reply: AbiType,
    pub param_modes: Vec<BoundaryMode>,
    pub reply_mode: BoundaryMode,
    pub schema: String,
    pub snapshot: SnapshotClass,
}

impl BundleOp {
    /// True when a pending call holds an external attachment.
    pub fn suspends(&self) -> bool {
        matches!(self.snapshot, SnapshotClass::HostAttachment)
    }
}

/// One direct group declaration in a completed ABI bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleGroup {
    pub name: String,
    pub members: Vec<String>,
}

/// One opaque host resource kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceDescriptor {
    pub name: String,
    pub version: u32,
    pub snapshot: SnapshotClass,
}

impl ResourceDescriptor {
    /// Create one host-attached resource descriptor.
    pub fn host_attachment(name: impl Into<String>) -> ResourceDescriptor {
        ResourceDescriptor {
            name: name.into(),
            version: 1,
            snapshot: SnapshotClass::HostAttachment,
        }
    }

    /// Set the resource contract version.
    pub fn version(mut self, version: u32) -> ResourceDescriptor {
        self.version = version;
        self
    }

    /// Return the stable identity of this resource contract.
    pub fn identity(&self) -> [u8; 32] {
        resource_identity(self)
    }

    /// Return the operation-schema type for this resource.
    pub fn abi_type(&self) -> AbiType {
        AbiType::Resource(self.identity())
    }
}

/// One extension operation before bundle construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationSpec {
    pub group: String,
    pub member: String,
    pub version: u32,
    pub params: Vec<AbiType>,
    pub reply: AbiType,
    pub param_modes: Vec<BoundaryMode>,
    pub reply_mode: BoundaryMode,
    pub snapshot: SnapshotClass,
}

impl OperationSpec {
    /// Create one synchronous fixed operation.
    pub fn fixed(
        group: impl Into<String>,
        member: impl Into<String>,
        params: Vec<AbiType>,
        reply: AbiType,
    ) -> OperationSpec {
        let param_modes = vec![BoundaryMode::Copy; params.len()];
        OperationSpec {
            group: group.into(),
            member: member.into(),
            version: 1,
            params,
            reply,
            param_modes,
            reply_mode: BoundaryMode::Copy,
            snapshot: SnapshotClass::MachineState,
        }
    }

    /// Set the operation contract version.
    pub fn version(mut self, version: u32) -> OperationSpec {
        self.version = version;
        self
    }

    /// Mark the operation as a suspending host attachment.
    pub fn suspending(mut self) -> OperationSpec {
        self.snapshot = SnapshotClass::HostAttachment;
        self
    }

    /// Set all boundary modes.
    pub fn boundary_modes(
        mut self,
        params: Vec<BoundaryMode>,
        reply: BoundaryMode,
    ) -> OperationSpec {
        self.param_modes = params;
        self.reply_mode = reply;
        self
    }
}

/// One extension group before bundle construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupSpec {
    pub name: String,
    pub members: Vec<String>,
}

impl GroupSpec {
    /// Create one namespace group.
    pub fn namespace(name: impl Into<String>) -> GroupSpec {
        GroupSpec {
            name: name.into(),
            members: Vec::new(),
        }
    }

    /// Create one effect set with explicit members.
    pub fn effect_set(
        name: impl Into<String>,
        members: impl IntoIterator<Item = impl Into<String>>,
    ) -> GroupSpec {
        GroupSpec {
            name: name.into(),
            members: members.into_iter().map(Into::into).collect(),
        }
    }
}

/// A completed immutable ABI bundle.
#[derive(Debug)]
pub struct AbiBundle {
    operations: Vec<BundleOp>,
    groups: Vec<BundleGroup>,
    resources: Vec<ResourceDescriptor>,
    operation_slots: BTreeMap<String, u32>,
    group_slots: BTreeMap<String, u32>,
    resource_slots: BTreeMap<String, u32>,
    resource_identities: BTreeMap<[u8; 32], u32>,
    group_bits: Vec<Vec<bool>>,
    groups_by_operation: Vec<Vec<u32>>,
    digest: [u8; 32],
}

impl AbiBundle {
    /// Start a bundle with the complete standard ABI.
    pub fn builder() -> AbiBundleBuilder {
        AbiBundleBuilder::new()
    }

    /// Return the exact bundle identity.
    pub fn digest(&self) -> [u8; 32] {
        self.digest
    }

    /// Return the number of operation slots.
    pub fn op_count(&self) -> u32 {
        self.operations.len() as u32
    }

    /// Return the number of group slots.
    pub fn group_count(&self) -> u32 {
        self.groups.len() as u32
    }

    /// Return the operation at one dense slot.
    pub fn op(&self, slot: u32) -> Option<&BundleOp> {
        self.operations.get(slot as usize)
    }

    /// Return the canonical operation name at one dense slot.
    pub fn op_name(&self, slot: u32) -> Option<&str> {
        self.op(slot).map(|operation| operation.name.as_str())
    }

    /// Find one operation by its canonical name.
    pub fn op_by_name(&self, name: &str) -> Option<u32> {
        self.operation_slots.get(name).copied()
    }

    /// Find one fixed operation in a namespace group.
    pub fn fixed_member(&self, group: &str, member: &str) -> Option<u32> {
        let name = format!("{group}.{member}");
        let slot = self.op_by_name(&name)?;
        (self.op(slot)?.kind == OpKind::Fixed).then_some(slot)
    }

    /// Return the stable identity of one operation.
    pub fn op_identity(&self, slot: u32) -> Option<[u8; 32]> {
        self.op(slot).map(operation_identity)
    }

    /// Find one group by its canonical name.
    pub fn group_by_name(&self, name: &str) -> Option<u32> {
        self.group_slots.get(name).copied()
    }

    /// Return the canonical group name at one dense slot.
    pub fn group_name(&self, slot: u32) -> Option<&str> {
        self.groups
            .get(slot as usize)
            .map(|group| group.name.as_str())
    }

    /// Resolve one lower-case `sys` group segment.
    pub fn surface_group(&self, surface: &str) -> Option<&str> {
        self.groups
            .iter()
            .find(|group| group_surface_name(&group.name) == surface)
            .map(|group| group.name.as_str())
    }

    /// Return the direct members of one group.
    pub fn group_members(&self, slot: u32) -> Option<&[String]> {
        self.groups
            .get(slot as usize)
            .map(|group| group.members.as_slice())
    }

    /// True when one group contains one exact operation.
    pub fn group_contains_op(&self, group: u32, operation: u32) -> bool {
        self.group_bits
            .get(group as usize)
            .and_then(|row| row.get(operation as usize))
            .copied()
            .unwrap_or(false)
    }

    /// Return the exact operation closure of one group.
    pub fn group_operations(&self, group: u32) -> Option<Vec<u32>> {
        let row = self.group_bits.get(group as usize)?;
        Some(
            row.iter()
                .enumerate()
                .filter_map(|(operation, included)| included.then_some(operation as u32))
                .collect(),
        )
    }

    /// Return every group that contains one exact operation.
    pub fn groups_containing_op(&self, operation: u32) -> Option<&[u32]> {
        self.groups_by_operation
            .get(operation as usize)
            .map(Vec::as_slice)
    }

    /// Return the exact operation closure of one valid row name.
    pub fn row_name_operations(&self, name: &str) -> Option<Vec<u32>> {
        if let Some(operation) = self.op_by_name(name) {
            return Some(vec![operation]);
        }
        self.group_by_name(name)
            .and_then(|group| self.group_operations(group))
    }

    /// True when one row name is included in another row name.
    pub fn row_name_included(&self, name: &str, expected: &str) -> bool {
        if name == expected {
            return true;
        }
        let Some(actual) = self.row_name_operations(name) else {
            return false;
        };
        let Some(required) = self.row_name_operations(expected) else {
            return false;
        };
        !actual.is_empty()
            && !required.is_empty()
            && actual.iter().all(|operation| required.contains(operation))
    }

    /// True when one name is an operation or group.
    pub fn row_name_valid(&self, name: &str) -> bool {
        self.op_by_name(name).is_some() || self.group_by_name(name).is_some()
    }

    /// Return all resource descriptors.
    pub fn resources(&self) -> &[ResourceDescriptor] {
        &self.resources
    }

    /// Find one resource descriptor by name.
    pub fn resource_by_name(&self, name: &str) -> Option<u32> {
        self.resource_slots.get(name).copied()
    }

    /// Find one resource descriptor by stable identity.
    pub fn resource_by_identity(&self, identity: [u8; 32]) -> Option<u32> {
        self.resource_identities.get(&identity).copied()
    }

    /// Return one resource descriptor by dense slot.
    pub fn resource(&self, slot: u32) -> Option<&ResourceDescriptor> {
        self.resources.get(slot as usize)
    }
}

/// A checked builder for one ABI bundle.
#[derive(Debug, Default)]
pub struct AbiBundleBuilder {
    groups: Vec<GroupSpec>,
    operations: Vec<OperationSpec>,
    resources: Vec<ResourceDescriptor>,
}

impl AbiBundleBuilder {
    /// Create a builder that extends the standard ABI.
    pub fn new() -> AbiBundleBuilder {
        AbiBundleBuilder::default()
    }

    /// Add one extension group.
    pub fn add_group(&mut self, group: GroupSpec) -> &mut AbiBundleBuilder {
        self.groups.push(group);
        self
    }

    /// Add one extension operation.
    pub fn add_operation(&mut self, operation: OperationSpec) -> &mut AbiBundleBuilder {
        self.operations.push(operation);
        self
    }

    /// Add one extension resource kind.
    pub fn add_resource(&mut self, resource: ResourceDescriptor) -> &mut AbiBundleBuilder {
        self.resources.push(resource);
        self
    }

    /// Validate and freeze the bundle.
    pub fn build(self) -> Result<Arc<AbiBundle>, String> {
        let mut groups: Vec<BundleGroup> = GROUPS
            .iter()
            .zip(GROUP_MEMBERS.iter())
            .map(|(name, members)| BundleGroup {
                name: (*name).to_string(),
                members: members.iter().map(|member| (*member).to_string()).collect(),
            })
            .collect();
        let standard_group_names: BTreeSet<&str> = GROUPS.into_iter().collect();
        let mut extension_groups = self.groups;
        extension_groups.sort_by(|left, right| left.name.cmp(&right.name));
        let mut seen_groups: BTreeSet<String> =
            groups.iter().map(|group| group.name.clone()).collect();
        for group in extension_groups {
            validate_qualified_name(&group.name, "group")?;
            if standard_group_names.contains(group.name.as_str())
                || !seen_groups.insert(group.name.clone())
            {
                return Err(format!("the ABI bundle repeats group `{}`", group.name));
            }
            groups.push(BundleGroup {
                name: group.name,
                members: group.members,
            });
        }

        let mut operations: Vec<BundleOp> = OPS.iter().map(standard_operation).collect();
        let standard_operation_names: BTreeSet<String> = operations
            .iter()
            .map(|operation| operation.name.clone())
            .collect();
        let group_names: BTreeSet<String> = groups.iter().map(|group| group.name.clone()).collect();

        let mut resources = self.resources;
        resources.sort_by(|left, right| left.name.cmp(&right.name));
        let mut resource_names = BTreeSet::new();
        let mut resource_identities = BTreeMap::new();
        for (slot, resource) in resources.iter().enumerate() {
            validate_qualified_name(&resource.name, "resource")?;
            if resource.version == 0 {
                return Err(format!("resource `{}` has version zero", resource.name));
            }
            if !resource_names.insert(resource.name.clone()) {
                return Err(format!(
                    "the ABI bundle repeats resource `{}`",
                    resource.name
                ));
            }
            if resource.snapshot != SnapshotClass::HostAttachment {
                return Err(format!(
                    "extension resource `{}` must be a host attachment",
                    resource.name
                ));
            }
            let identity = resource.identity();
            if resource_identities.insert(identity, slot as u32).is_some() {
                return Err("two extension resources have one identity".to_string());
            }
        }
        let resource_slots = resources
            .iter()
            .enumerate()
            .map(|(slot, resource)| (resource.name.clone(), slot as u32))
            .collect();
        let known_resources: BTreeSet<[u8; 32]> = resource_identities.keys().copied().collect();

        let mut extension_operations = Vec::with_capacity(self.operations.len());
        let mut seen_operations = standard_operation_names.clone();
        for operation in self.operations {
            validate_qualified_name(&operation.group, "operation group")?;
            validate_member_name(&operation.member)?;
            if !group_names.contains(&operation.group) {
                return Err(format!(
                    "operation `{}.{}` names unknown group `{}`",
                    operation.group, operation.member, operation.group
                ));
            }
            let name = format!("{}.{}", operation.group, operation.member);
            if standard_operation_names.contains(&name) || !seen_operations.insert(name.clone()) {
                return Err(format!("the ABI bundle repeats operation `{name}`"));
            }
            if group_names.contains(&name) {
                return Err(format!("operation `{name}` collides with a group"));
            }
            if operation.version == 0 {
                return Err(format!("operation `{name}` has version zero"));
            }
            if operation.param_modes.len() != operation.params.len() {
                return Err(format!(
                    "operation `{name}` has the wrong boundary mode count"
                ));
            }
            if type_contains_resource(operation.reply) && operation.reply_mode != BoundaryMode::Move
            {
                return Err(format!("operation `{name}` must move its resource reply"));
            }
            if operation
                .params
                .iter()
                .any(|item| !extension_type_supported(*item, &known_resources))
                || !extension_type_supported(operation.reply, &known_resources)
            {
                return Err(format!("operation `{name}` has an unsupported type schema"));
            }
            extension_operations.push(BundleOp {
                name,
                group: operation.group,
                member: operation.member,
                version: operation.version,
                kind: OpKind::Fixed,
                params: operation.params,
                reply: operation.reply,
                param_modes: operation.param_modes,
                reply_mode: operation.reply_mode,
                schema: String::new(),
                snapshot: operation.snapshot,
            });
        }
        extension_operations.sort_by(|left, right| {
            operation_identity(left)
                .cmp(&operation_identity(right))
                .then_with(|| left.name.cmp(&right.name))
        });
        operations.extend(extension_operations);

        let operation_slots: BTreeMap<String, u32> = operations
            .iter()
            .enumerate()
            .map(|(slot, operation)| (operation.name.clone(), slot as u32))
            .collect();
        let group_slots: BTreeMap<String, u32> = groups
            .iter()
            .enumerate()
            .map(|(slot, group)| (group.name.clone(), slot as u32))
            .collect();
        validate_group_members(&groups, &group_slots, &operation_slots)?;
        let group_bits = build_group_bits(&groups, &group_slots, &operations)?;
        let groups_by_operation = (0..operations.len())
            .map(|operation| {
                (0..groups.len())
                    .filter(|group| group_bits[*group][operation])
                    .map(|group| group as u32)
                    .collect()
            })
            .collect();

        let digest = bundle_digest(&groups, &operations, &resources);
        Ok(Arc::new(AbiBundle {
            operations,
            groups,
            resources,
            operation_slots,
            group_slots,
            resource_slots,
            resource_identities,
            group_bits,
            groups_by_operation,
            digest,
        }))
    }
}

/// Return the process-wide standard ABI bundle.
pub fn standard_bundle() -> Arc<AbiBundle> {
    static STANDARD: OnceLock<Arc<AbiBundle>> = OnceLock::new();
    STANDARD
        .get_or_init(|| {
            AbiBundleBuilder::new()
                .build()
                .expect("the standard ABI bundle is valid")
        })
        .clone()
}

fn standard_operation(definition: &OpDef) -> BundleOp {
    let name = format!("{}.{}", definition.group, definition.member);
    BundleOp {
        name,
        group: definition.group.to_string(),
        member: definition.member.to_string(),
        version: 1,
        kind: definition.kind,
        params: definition.params.to_vec(),
        reply: definition.reply,
        param_modes: vec![BoundaryMode::Copy; definition.params.len()],
        reply_mode: BoundaryMode::Copy,
        schema: definition.schema.to_string(),
        snapshot: definition.snapshot,
    }
}

fn extension_type_supported(ty: AbiType, resources: &BTreeSet<[u8; 32]>) -> bool {
    use crate::{AbiConstructor, AbiPrimitive};
    match ty {
        AbiType::Primitive(
            AbiPrimitive::Unit
            | AbiPrimitive::Bool
            | AbiPrimitive::Int
            | AbiPrimitive::String
            | AbiPrimitive::Bytes,
        ) => true,
        AbiType::List(element) => extension_type_supported(*element, resources),
        AbiType::Tuple(elements) => elements
            .iter()
            .copied()
            .all(|item| extension_type_supported(item, resources)),
        AbiType::Apply(AbiConstructor::Option | AbiConstructor::Result, items) => {
            ty.valid()
                && items
                    .iter()
                    .copied()
                    .all(|item| extension_type_supported(item, resources))
        }
        AbiType::Resource(identity) => resources.contains(&identity),
        _ => false,
    }
}

fn type_contains_resource(ty: AbiType) -> bool {
    match ty {
        AbiType::Resource(_) => true,
        AbiType::List(element) => type_contains_resource(*element),
        AbiType::Tuple(elements) | AbiType::Apply(_, elements) => {
            elements.iter().copied().any(type_contains_resource)
        }
        AbiType::Map(key, value) => type_contains_resource(*key) || type_contains_resource(*value),
        _ => false,
    }
}

fn validate_qualified_name(name: &str, what: &str) -> Result<(), String> {
    if name.is_empty()
        || name.split('.').any(|part| {
            part.is_empty()
                || !part
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
        })
    {
        return Err(format!("the {what} name `{name}` is invalid"));
    }
    Ok(())
}

fn validate_member_name(name: &str) -> Result<(), String> {
    if name.is_empty()
        || !name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    {
        return Err(format!("the operation member `{name}` is invalid"));
    }
    Ok(())
}

fn validate_group_members(
    groups: &[BundleGroup],
    group_slots: &BTreeMap<String, u32>,
    operation_slots: &BTreeMap<String, u32>,
) -> Result<(), String> {
    for group in groups {
        let mut direct = BTreeSet::new();
        for member in &group.members {
            if !direct.insert(member) {
                return Err(format!(
                    "effect set `{}` repeats member `{member}`",
                    group.name
                ));
            }
            if !group_slots.contains_key(member) && !operation_slots.contains_key(member) {
                return Err(format!(
                    "effect set `{}` names unknown member `{member}`",
                    group.name
                ));
            }
        }
    }
    let mut state = vec![0u8; groups.len()];
    fn visit(
        group: usize,
        groups: &[BundleGroup],
        slots: &BTreeMap<String, u32>,
        state: &mut [u8],
    ) -> Result<(), String> {
        if state[group] == 1 {
            return Err(format!(
                "effect set `{}` is part of a cycle",
                groups[group].name
            ));
        }
        if state[group] == 2 {
            return Ok(());
        }
        state[group] = 1;
        for member in &groups[group].members {
            if let Some(child) = slots.get(member) {
                visit(*child as usize, groups, slots, state)?;
            }
        }
        state[group] = 2;
        Ok(())
    }
    for group in 0..groups.len() {
        visit(group, groups, group_slots, &mut state)?;
    }
    Ok(())
}

fn build_group_bits(
    groups: &[BundleGroup],
    group_slots: &BTreeMap<String, u32>,
    operations: &[BundleOp],
) -> Result<Vec<Vec<bool>>, String> {
    fn contains(
        group: usize,
        operation: usize,
        groups: &[BundleGroup],
        group_slots: &BTreeMap<String, u32>,
        operations: &[BundleOp],
        seen: &mut [bool],
    ) -> bool {
        if seen[group] {
            return false;
        }
        seen[group] = true;
        if operations[operation].group == groups[group].name {
            return true;
        }
        for member in &groups[group].members {
            if member == &operations[operation].name {
                return true;
            }
            if let Some(child) = group_slots.get(member) {
                if contains(
                    *child as usize,
                    operation,
                    groups,
                    group_slots,
                    operations,
                    seen,
                ) {
                    return true;
                }
            }
        }
        false
    }
    let mut rows = Vec::with_capacity(groups.len());
    for group in 0..groups.len() {
        let mut row = Vec::with_capacity(operations.len());
        for operation in 0..operations.len() {
            let mut seen = vec![false; groups.len()];
            row.push(contains(
                group,
                operation,
                groups,
                group_slots,
                operations,
                &mut seen,
            ));
        }
        rows.push(row);
    }
    Ok(rows)
}

fn group_surface_name(name: &str) -> String {
    let mut out = String::new();
    for (index, ch) in name.chars().enumerate() {
        if ch == '.' {
            out.push('_');
        } else if ch.is_ascii_uppercase() {
            if index != 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

fn id_field(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
    out.extend_from_slice(bytes);
}

fn operation_identity(operation: &BundleOp) -> [u8; 32] {
    let mut input = Vec::new();
    input.extend_from_slice(b"lm-bundle-operation-v1\0");
    id_field(&mut input, operation.name.as_bytes());
    id_field(&mut input, operation.group.as_bytes());
    id_field(&mut input, operation.member.as_bytes());
    id_field(&mut input, &operation.version.to_le_bytes());
    id_field(&mut input, operation.kind.tag().as_bytes());
    id_field(&mut input, &(operation.params.len() as u64).to_le_bytes());
    for (parameter, mode) in operation.params.iter().zip(&operation.param_modes) {
        id_field(&mut input, parameter.text().as_bytes());
        id_field(&mut input, mode.tag().as_bytes());
    }
    id_field(&mut input, operation.reply.text().as_bytes());
    id_field(&mut input, operation.reply_mode.tag().as_bytes());
    id_field(&mut input, operation.schema.as_bytes());
    id_field(&mut input, operation.snapshot.tag().as_bytes());
    sha256(&input)
}

fn resource_identity(resource: &ResourceDescriptor) -> [u8; 32] {
    let mut input = Vec::new();
    input.extend_from_slice(b"lm-bundle-resource-v1\0");
    id_field(&mut input, resource.name.as_bytes());
    id_field(&mut input, &resource.version.to_le_bytes());
    id_field(&mut input, resource.snapshot.tag().as_bytes());
    sha256(&input)
}

fn bundle_digest(
    groups: &[BundleGroup],
    operations: &[BundleOp],
    resources: &[ResourceDescriptor],
) -> [u8; 32] {
    let mut input = Vec::new();
    input.extend_from_slice(b"lm-abi-bundle-v1\0");
    for group in groups {
        id_field(&mut input, group.name.as_bytes());
        id_field(&mut input, &(group.members.len() as u64).to_le_bytes());
        for member in &group.members {
            id_field(&mut input, member.as_bytes());
        }
    }
    for operation in operations {
        input.extend_from_slice(&operation_identity(operation));
    }
    for resource in resources {
        id_field(&mut input, resource.name.as_bytes());
        id_field(&mut input, &resource.version.to_le_bytes());
        id_field(&mut input, resource.snapshot.tag().as_bytes());
    }
    sha256(&input)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_standard_bundle_keeps_every_standard_slot() {
        let bundle = standard_bundle();
        assert_eq!(bundle.op_count(), crate::OP_COUNT);
        assert_eq!(bundle.group_count(), crate::GROUP_COUNT);
        for slot in 0..crate::OP_COUNT {
            assert_eq!(bundle.op_name(slot), Some(crate::op_name(slot).as_str()));
        }
    }

    #[test]
    fn extension_order_does_not_change_the_bundle() {
        let build = |reverse: bool| {
            let mut builder = AbiBundle::builder();
            builder.add_group(GroupSpec::namespace("Telemetry"));
            let mut operations = vec![
                OperationSpec::fixed(
                    "Telemetry",
                    "Event",
                    vec![AbiType::STR, AbiType::INT],
                    AbiType::UNIT,
                ),
                OperationSpec::fixed("Telemetry", "Count", vec![AbiType::STR], AbiType::INT),
            ];
            if reverse {
                operations.reverse();
            }
            for operation in operations {
                builder.add_operation(operation);
            }
            builder.build().expect("the extension is valid")
        };
        let left = build(false);
        let right = build(true);
        assert_eq!(left.digest(), right.digest());
        assert_eq!(
            left.op_by_name("Telemetry.Event"),
            right.op_by_name("Telemetry.Event")
        );
    }

    #[test]
    fn bundle_validation_rejects_conflicts_and_cycles() {
        let mut conflict = AbiBundle::builder();
        conflict.add_group(GroupSpec::namespace("Io"));
        assert!(conflict.build().is_err());

        let mut cycle = AbiBundle::builder();
        cycle.add_group(GroupSpec::effect_set("First", ["Second"]));
        cycle.add_group(GroupSpec::effect_set("Second", ["First"]));
        assert!(cycle.build().is_err());
    }
}
