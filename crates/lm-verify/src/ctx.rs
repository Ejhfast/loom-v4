//! The verification context: type reads, rows, and shared checks.
//!
//! One part of the bytecode verifier. `lib.rs` holds the shared
//! context, the error type, and the entry points.

use super::*;

impl<'m> Ctx<'m> {
    /// Return the class built by one named constructor function.
    pub(crate) fn constructor_class(&self, func: u32) -> Option<u32> {
        self.constructor_classes
            .get(func as usize)
            .copied()
            .flatten()
    }

    /// Return one direct class conformance.
    pub(crate) fn direct_conformance(
        &self,
        class: u32,
        interface: u32,
    ) -> Option<&lm_bytecode::BcConformance> {
        let index = *self.conformance_index.get(&(class, interface))?;
        self.module.conformances.get(index)
    }

    /// Test whether every value of one type is deeply frozen.
    pub(crate) fn type_always_frozen(&self, ty: u32, allow_var: bool) -> bool {
        self.type_always_frozen_inner(ty, allow_var, 0)
    }

    fn type_always_frozen_inner(&self, ty: u32, allow_var: bool, depth: usize) -> bool {
        if depth >= 128 {
            return false;
        }
        match self.ty(ty) {
            BcType::Unit
            | BcType::Bool
            | BcType::Int
            | BcType::Float
            | BcType::Str
            | BcType::Bytes
            | BcType::Digest
            | BcType::Fault
            | BcType::Op(_, _) => true,
            BcType::Var(_) => allow_var,
            BcType::Tuple(items) => items
                .iter()
                .all(|item| self.type_always_frozen_inner(*item, allow_var, depth + 1)),
            BcType::Class(class) => self.class_always_frozen(class),
            BcType::Inst(class, args) => {
                self.class_always_frozen(class)
                    && args
                        .iter()
                        .all(|arg| self.type_always_frozen_inner(*arg, allow_var, depth + 1))
            }
            _ => false,
        }
    }

    fn class_always_frozen(&self, class: u32) -> bool {
        self.module
            .classes
            .get(class as usize)
            .is_some_and(|entry| entry.is_frozen)
            || [
                self.core.unit,
                self.core.int,
                self.core.float,
                self.core.boolean,
                self.core.text,
                self.core.string,
                self.core.substring,
                self.core.char_value,
                self.core.bytes,
            ]
            .contains(&Some(class))
    }

    /// Find the reserved core `Hashable` interface.
    pub(crate) fn hashable_interface(&self) -> Option<u32> {
        self.module
            .interfaces
            .iter()
            .position(|interface| interface.key == "core.Hashable")
            .map(|index| index as u32)
    }

    /// Test whether native map instructions support one key type.
    pub(crate) fn native_map_key(&self, ty: u32) -> bool {
        match self.ty(ty) {
            BcType::Bool | BcType::Int | BcType::Float | BcType::Str | BcType::Bytes => true,
            BcType::Class(class) | BcType::Inst(class, _) => [
                self.core.text,
                self.core.string,
                self.core.substring,
                self.core.char_value,
                self.core.bytes,
            ]
            .contains(&Some(class)),
            _ => false,
        }
    }

    /// Test whether one key type proves the core `Hashable` contract.
    pub(crate) fn hashable_map_key(&self, func: u32, ty: u32) -> bool {
        self.hashable_interface()
            .is_some_and(|interface| self.interface_application(func, ty, interface, 0).is_some())
    }

    pub(crate) fn ty(&self, idx: u32) -> BcType {
        self.uni.borrow().types[idx as usize].clone()
    }

    pub(crate) fn intern(&self, ty: BcType) -> u32 {
        self.uni.borrow_mut().intern(ty)
    }

    /// True when one class uses a VM-owned native representation.
    pub(crate) fn is_native_core_class(&self, class: u32) -> bool {
        [
            self.core.int,
            self.core.float,
            self.core.boolean,
            self.core.string,
            self.core.substring,
            self.core.char_value,
            self.core.bytes,
            self.core.string_builder,
            self.core.byte_buffer,
            self.core.unit,
            self.core.list,
            self.core.map,
            self.core.tcp_resource,
            self.core.tcp_stream,
            self.core.tcp_listener,
            self.core.tls_stream,
            self.core.file_handle,
            self.core.artifact,
            self.core.verified_module,
            self.core.slot_spec,
            self.core.instance,
            self.core.slot,
            self.core.function_def,
            self.core.class_def,
            self.core.dyn_value,
        ]
        .into_iter()
        .chain(self.core.tuples)
        .any(|candidate| candidate == Some(class))
    }

    /// Return the canonical self type of one class.
    pub(crate) fn class_self_type(&self, class: u32) -> Option<u32> {
        if self.core.unit == Some(class) {
            return Some(TY_UNIT);
        }
        if self.core.int == Some(class) {
            return Some(TY_INT);
        }
        if self.core.float == Some(class) {
            return Some(self.intern(BcType::Float));
        }
        if self.core.boolean == Some(class) {
            return Some(TY_BOOL);
        }
        if self.core.string == Some(class) {
            return Some(TY_STR);
        }
        if self.core.bytes == Some(class) {
            return Some(self.intern(BcType::Bytes));
        }
        if self.core.file_handle == Some(class) {
            return Some(self.intern(BcType::FileHandle));
        }
        let entry = self.module.classes.get(class as usize)?;
        if self.core.list == Some(class) && entry.type_params == 1 {
            let element = self.intern(BcType::Var(0));
            return Some(self.intern(BcType::List(element)));
        }
        if self.core.map == Some(class) && entry.type_params == 2 {
            let key = self.intern(BcType::Var(0));
            let value = self.intern(BcType::Var(1));
            return Some(self.intern(BcType::Map(key, value)));
        }
        if let Some(arity) = self
            .core
            .tuples
            .iter()
            .position(|candidate| *candidate == Some(class))
        {
            let elements = (0..arity)
                .map(|index| self.intern(BcType::Var(index as u32)))
                .collect();
            return Some(self.intern(BcType::Tuple(elements)));
        }
        if entry.type_params == 0 {
            return self.class_ty[class as usize];
        }
        let args = (0..entry.type_params)
            .map(|index| self.intern(BcType::Var(index)))
            .collect();
        Some(self.intern(BcType::Inst(class, args)))
    }

    /// Return the payload type of a pinned `Option` family or arm.
    pub(crate) fn option_arg(&self, ty: u32) -> Option<u32> {
        let (class, args) = self.as_instance(ty)?;
        let is_option = Some(class) == self.core.option
            || Some(class) == self.core.option_some
            || Some(class) == self.core.option_none;
        if is_option && args.len() == 1 {
            Some(args[0])
        } else {
            None
        }
    }

    /// The type arguments of `ancestor` seen from an instance of
    /// `child` applied to `args`. `None` when `child` does not inherit
    /// `ancestor`.
    ///
    /// Three parent shapes exist. An enum case shares the arity of its
    /// family and passes its arguments through. A declared generic
    /// parent records closed arguments, because a generic class never
    /// declares a parent. Every other parent has no arguments. The
    /// walk therefore never substitutes.
    pub(crate) fn ancestor_args(
        &self,
        child: u32,
        args: &[u32],
        ancestor: u32,
    ) -> Option<Vec<u32>> {
        let mut cur = child;
        let mut cur_args = args.to_vec();
        loop {
            if cur == ancestor {
                return Some(cur_args);
            }
            let class = &self.module.classes[cur as usize];
            let parent = class.parent()?;
            if !class.parent_args.is_empty() {
                cur_args = class.parent_args.clone();
            } else if self.module.classes[parent as usize].type_params == 0 {
                cur_args = Vec::new();
            }
            cur = parent;
        }
    }

    /// The parent type arguments of one class, in the class's own type
    /// parameters. An enum case has the implicit identity arguments.
    pub(crate) fn declared_parent_args(&self, cidx: u32) -> Vec<u32> {
        let class = &self.module.classes[cidx as usize];
        if !class.parent_args.is_empty() {
            return class.parent_args.clone();
        }
        let Some(parent) = class.parent() else {
            return Vec::new();
        };
        let arity = self.module.classes[parent as usize].type_params;
        (0..arity).map(|i| self.intern(BcType::Var(i))).collect()
    }

    /// The class that declares one selector, walking the ancestor
    /// chain from `class`.
    pub(crate) fn method_owner(&self, class: u32, selector: u32) -> Option<u32> {
        self.method_resolution(class, selector)
            .map(|(owner, _)| owner)
    }

    /// Resolve one method and its declaring class.
    pub(crate) fn method_resolution(&self, mut class: u32, selector: u32) -> Option<(u32, u32)> {
        loop {
            let entry = &self.module.classes[class as usize];
            if let Some((_, function)) = entry.methods.iter().find(|(sel, _)| *sel == selector) {
                return Some((class, *function));
            }
            class = entry.parent()?;
        }
    }

    /// The sort key of one row element, for canonical order checks.
    pub(crate) fn row_key(&self, elem: &BcRow) -> (u8, u32) {
        match elem {
            BcRow::Op(slot) => (0, *slot),
            BcRow::Group(slot) => (1, *slot),
            BcRow::Var(variable) => (2, *variable),
        }
    }

    /// Return true when the row is sorted and has no duplicate.
    pub(crate) fn row_canonical(&self, row: &[BcRow]) -> bool {
        row.windows(2)
            .all(|w| self.row_key(&w[0]) < self.row_key(&w[1]))
    }

    /// Return true when every element of `sub` is included in `sup`.
    pub(crate) fn row_included(&self, sub: &[BcRow], sup: &[BcRow]) -> bool {
        sub.iter().all(|elem| match elem {
            BcRow::Var(v) => sup.contains(&BcRow::Var(*v)),
            BcRow::Op(n) | BcRow::Group(n) => {
                let name = match elem {
                    BcRow::Op(_) => self.bundle.op_name(*n),
                    BcRow::Group(_) => self.bundle.group_name(*n),
                    BcRow::Var(_) => None,
                };
                let Some(name) = name else {
                    return false;
                };
                sup.iter().any(|s| match s {
                    BcRow::Op(m) => self
                        .bundle
                        .op_name(*m)
                        .is_some_and(|sup_name| self.bundle.row_name_included(name, sup_name)),
                    BcRow::Group(m) => self
                        .bundle
                        .group_name(*m)
                        .is_some_and(|sup_name| self.bundle.row_name_included(name, sup_name)),
                    BcRow::Var(_) => false,
                })
            }
        })
    }

    /// Substitute effect variables in a row and re-canonicalize.
    pub(crate) fn row_subst(&self, row: &[BcRow], rows: &[Vec<BcRow>]) -> Vec<BcRow> {
        let mut out: Vec<BcRow> = Vec::new();
        for elem in row {
            match elem {
                BcRow::Var(v) => match rows.get(*v as usize) {
                    Some(replacement) => out.extend_from_slice(replacement),
                    None => out.push(*elem),
                },
                BcRow::Op(_) | BcRow::Group(_) => out.push(*elem),
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    /// The child type indices of one type, in declaration order.
    ///
    /// Every child of a universe entry has a smaller index, because
    /// `intern` appends and a caller builds a node from the bottom up.
    /// Each walk below reads this one list, so the walks cannot drift.
    pub(crate) fn type_children(&self, ty: u32, out: &mut Vec<u32>) {
        match self.ty(ty) {
            BcType::Inst(_, args) | BcType::Tuple(args) => out.extend(args),
            BcType::List(e)
            | BcType::Projection { base: e, .. }
            | BcType::Run(e)
            | BcType::Wait(e)
            | BcType::RunSnapshot(e)
            | BcType::Op(_, e) => out.push(e),
            BcType::Map(a, b) | BcType::PendingCall(a, b) | BcType::Handle(a, b) => {
                out.push(a);
                out.push(b);
            }
            BcType::Fn(params, _, ret, _) | BcType::Callback(params, _, ret, _) => {
                out.extend(params);
                out.push(ret);
            }
            _ => {}
        }
    }

    /// Substitute type variables and effect variables in one type.
    ///
    /// The walk is iterative. A crafted artifact can nest a type as
    /// deeply as its type table allows, so a walk on the Rust stack
    /// would abort the host.
    pub(crate) fn subst(&self, ty: u32, targs: &[u32], rows: &[Vec<BcRow>]) -> u32 {
        self.subst_with_bounds(ty, targs, rows, &[])
    }

    /// Substitute one type and resolve projections with known bounds.
    pub(crate) fn subst_with_bounds(
        &self,
        ty: u32,
        targs: &[u32],
        rows: &[Vec<BcRow>],
        bounds: &[Vec<BcInterfaceUse>],
    ) -> u32 {
        if targs.is_empty() && rows.is_empty() && bounds.is_empty() {
            return ty;
        }
        let mut done: HashMap<u32, u32> = HashMap::new();
        let mut children: Vec<u32> = Vec::new();
        // Each entry pairs one type with the flag that says whether
        // its children already sit on the stack.
        let mut stack: Vec<(u32, bool)> = vec![(ty, false)];
        while let Some((cur, expanded)) = stack.pop() {
            if done.contains_key(&cur) {
                continue;
            }
            if !expanded {
                stack.push((cur, true));
                children.clear();
                self.type_children(cur, &mut children);
                for child in &children {
                    stack.push((*child, false));
                }
                continue;
            }
            let child = |c: u32| done.get(&c).copied().unwrap_or(c);
            let built = match self.ty(cur) {
                BcType::Var(i) => targs.get(i as usize).copied().unwrap_or(cur),
                BcType::Projection {
                    base,
                    interface,
                    assoc,
                } => {
                    let base = child(base);
                    self.projected_type_with_bounds(base, interface, assoc, bounds)
                        .unwrap_or_else(|| {
                            self.intern(BcType::Projection {
                                base,
                                interface,
                                assoc,
                            })
                        })
                }
                BcType::Inst(c, args) => {
                    self.intern(BcType::Inst(c, args.iter().map(|a| child(*a)).collect()))
                }
                BcType::List(e) => self.intern(BcType::List(child(e))),
                BcType::Map(k, v) => self.intern(BcType::Map(child(k), child(v))),
                BcType::Tuple(elems) => {
                    self.intern(BcType::Tuple(elems.iter().map(|e| child(*e)).collect()))
                }
                BcType::Fn(params, muts, ret, row) => self.intern(BcType::Fn(
                    params.iter().map(|p| child(*p)).collect(),
                    muts,
                    child(ret),
                    self.row_subst(&row, rows),
                )),
                BcType::Callback(params, muts, ret, row) => self.intern(BcType::Callback(
                    params.iter().map(|p| child(*p)).collect(),
                    muts,
                    child(ret),
                    self.row_subst(&row, rows),
                )),
                BcType::Run(t) => self.intern(BcType::Run(child(t))),
                BcType::Wait(t) => self.intern(BcType::Wait(child(t))),
                BcType::RunSnapshot(t) => self.intern(BcType::RunSnapshot(child(t))),
                BcType::PendingCall(a, r) => self.intern(BcType::PendingCall(child(a), child(r))),
                BcType::Handle(m, r) => self.intern(BcType::Handle(child(m), child(r))),
                _ => cur,
            };
            done.insert(cur, built);
        }
        done.get(&ty).copied().unwrap_or(ty)
    }

    pub(crate) fn subst_interface_use(
        &self,
        application: &BcInterfaceUse,
        types: &[u32],
        rows: &[Vec<BcRow>],
    ) -> BcInterfaceUse {
        BcInterfaceUse {
            interface: application.interface,
            types: application
                .types
                .iter()
                .map(|item| self.subst(*item, types, rows))
                .collect(),
            rows: application
                .rows
                .iter()
                .map(|item| self.row_subst(item, rows))
                .collect(),
        }
    }

    /// Substitute one interface application with known type bounds.
    pub(crate) fn subst_interface_use_with_bounds(
        &self,
        application: &BcInterfaceUse,
        types: &[u32],
        rows: &[Vec<BcRow>],
        bounds: &[Vec<BcInterfaceUse>],
    ) -> BcInterfaceUse {
        BcInterfaceUse {
            interface: application.interface,
            types: application
                .types
                .iter()
                .map(|item| self.subst_with_bounds(*item, types, rows, bounds))
                .collect(),
            rows: application
                .rows
                .iter()
                .map(|item| self.row_subst(item, rows))
                .collect(),
        }
    }

    pub(crate) fn concrete_conformance(
        &self,
        ty: u32,
        interface: u32,
    ) -> Option<(&lm_bytecode::BcConformance, Vec<u32>)> {
        self.concrete_conformance_with_bounds(ty, interface, &[], 0)
    }

    /// Resolve one concrete conformance and prove its premises.
    fn concrete_conformance_with_bounds(
        &self,
        ty: u32,
        interface: u32,
        bounds: &[Vec<BcInterfaceUse>],
        depth: u32,
    ) -> Option<(&lm_bytecode::BcConformance, Vec<u32>)> {
        if depth >= 128 {
            return None;
        }
        let (mut class, mut args) = self.as_instance(ty)?;
        loop {
            if let Some(conformance) = self.direct_conformance(class, interface) {
                let satisfied = conformance.premises.iter().all(|premise| {
                    let Some(actual) = args.get(premise.param as usize).copied() else {
                        return false;
                    };
                    premise.bounds.iter().all(|bound| {
                        let required = self.subst_interface_use(bound, &args, &[]);
                        self.interface_application_with_bounds(
                            actual,
                            required.interface,
                            bounds,
                            depth + 1,
                        )
                        .as_ref()
                            == Some(&required)
                    })
                });
                if satisfied {
                    return Some((conformance, args));
                }
            }
            let entry = &self.module.classes[class as usize];
            let parent = entry.parent()?;
            if !entry.parent_args.is_empty() {
                args = entry
                    .parent_args
                    .iter()
                    .map(|item| self.subst(*item, &args, &[]))
                    .collect();
            } else if self.module.classes[parent as usize].type_params == 0 {
                args.clear();
            }
            class = parent;
        }
    }

    /// Resolve one projection with a generic bound table.
    pub(crate) fn projected_type_with_bounds(
        &self,
        base: u32,
        interface: u32,
        assoc: u32,
        bounds: &[Vec<BcInterfaceUse>],
    ) -> Option<u32> {
        if let Some((source, iterable, item)) = self.iterable_item_alias(base, interface, assoc) {
            return Some(
                self.projected_type_with_bounds(source, iterable, item, bounds)
                    .unwrap_or_else(|| {
                        self.intern(BcType::Projection {
                            base: source,
                            interface: iterable,
                            assoc: item,
                        })
                    }),
            );
        }
        let (conformance, args) =
            self.concrete_conformance_with_bounds(base, interface, bounds, 0)?;
        let template = *conformance.associated.get(assoc as usize)?;
        Some(self.subst(template, &args, &[]))
    }

    /// Return the core Iterable item alias for one iterator projection.
    fn iterable_item_alias(
        &self,
        base: u32,
        interface: u32,
        assoc: u32,
    ) -> Option<(u32, u32, u32)> {
        let layout = self.iterable_layout?;
        if interface != layout.iterator || assoc != layout.iterator_item {
            return None;
        }
        let BcType::Projection {
            base: source,
            interface: owner,
            assoc: iter_assoc,
        } = self.ty(base)
        else {
            return None;
        };
        if owner != layout.iterable || iter_assoc != layout.iterable_iter {
            return None;
        }
        Some((source, layout.iterable, layout.iterable_item))
    }

    pub(crate) fn interface_application(
        &self,
        func: u32,
        ty: u32,
        interface: u32,
        depth: u32,
    ) -> Option<BcInterfaceUse> {
        if let Some((owner, method)) = self
            .interface_defaults
            .get(func as usize)
            .copied()
            .flatten()
        {
            let method = &self.module.interfaces[owner as usize].methods[method as usize];
            if let Some(bound) = method
                .premises
                .iter()
                .find(|premise| premise.subject == ty)
                .and_then(|premise| {
                    premise
                        .bounds
                        .iter()
                        .find(|bound| bound.interface == interface)
                })
            {
                return Some(bound.clone());
            }
        }
        let bounds = self.module.func_bounds.get(func as usize)?;
        self.interface_application_with_bounds(ty, interface, bounds, depth)
    }

    /// Return true when a function is an interface-owned default.
    pub(crate) fn is_interface_default(&self, func: u32) -> bool {
        self.interface_defaults
            .get(func as usize)
            .and_then(|owner| *owner)
            .is_some()
    }

    /// Resolve one interface application from an explicit bound table.
    pub(crate) fn interface_application_with_bounds(
        &self,
        ty: u32,
        interface: u32,
        bounds: &[Vec<BcInterfaceUse>],
        depth: u32,
    ) -> Option<BcInterfaceUse> {
        if depth >= 128 {
            return None;
        }
        match self.ty(ty) {
            BcType::Var(index) => bounds
                .get(index as usize)?
                .iter()
                .find(|item| item.interface == interface)
                .cloned(),
            BcType::Projection {
                base,
                interface: owner,
                assoc,
            } => {
                let owner_application =
                    self.interface_application_with_bounds(base, owner, bounds, depth + 1)?;
                let bound = self
                    .module
                    .interfaces
                    .get(owner as usize)?
                    .associated
                    .get(assoc as usize)?
                    .bounds
                    .iter()
                    .find(|bound| bound.interface == interface)?;
                let mut types = vec![base];
                types.extend(owner_application.types.iter().copied());
                Some(self.subst_interface_use(bound, &types, &owner_application.rows))
            }
            _ => {
                let (conformance, args) =
                    self.concrete_conformance_with_bounds(ty, interface, bounds, depth)?;
                Some(self.subst_interface_use(&conformance.application, &args, &[]))
            }
        }
    }

    /// Validate one interface application in one generic scope.
    pub(crate) fn check_interface_use(
        &self,
        application: &BcInterfaceUse,
        type_params: u32,
        effect_params: u32,
    ) -> Result<(), String> {
        let contract = self
            .module
            .interfaces
            .get(application.interface as usize)
            .ok_or_else(|| "the interface index is out of range".to_string())?;
        if application.types.len() != contract.type_params as usize {
            return Err("the interface type argument count is wrong".to_string());
        }
        if application.rows.len() != contract.effect_params as usize {
            return Err("the interface effect argument count is wrong".to_string());
        }
        for ty in &application.types {
            if *ty as usize >= self.module.types.len() {
                return Err("an interface type argument is out of range".to_string());
            }
            if self.stores_callback(*ty) {
                return Err("an interface type argument cannot contain a callback".to_string());
            }
            if !self.vars_bounded(*ty, type_params, effect_params) {
                return Err("an interface type argument uses an unbound variable".to_string());
            }
        }
        for row in &application.rows {
            if !self.row_vars_bounded(row, effect_params) {
                return Err("an interface row uses an unbound variable".to_string());
            }
            for element in row {
                match element {
                    BcRow::Op(slot) if *slot >= self.bundle.op_count() => {
                        return Err("an interface row has an invalid operation slot".to_string());
                    }
                    BcRow::Group(slot) if *slot >= self.bundle.group_count() => {
                        return Err("an interface row has an invalid group slot".to_string());
                    }
                    _ => {}
                }
            }
            if !self.row_canonical(row) {
                return Err("an interface row is not canonical".to_string());
            }
        }
        Ok(())
    }

    /// Test the generic arguments of one interface application.
    pub(crate) fn interface_arguments_meet_bounds(
        &self,
        receiver: u32,
        application: &BcInterfaceUse,
        scope_bounds: &[Vec<BcInterfaceUse>],
    ) -> bool {
        let Some(contract) = self.module.interfaces.get(application.interface as usize) else {
            return false;
        };
        let mut types = Vec::with_capacity(application.types.len() + 1);
        types.push(receiver);
        types.extend_from_slice(&application.types);
        for (actual, bounds) in application.types.iter().zip(&contract.type_bounds) {
            for bound in bounds {
                let required = self.subst_interface_use(bound, &types, &application.rows);
                let found = self.interface_application_with_bounds(
                    *actual,
                    required.interface,
                    scope_bounds,
                    0,
                );
                if found.as_ref() != Some(&required) {
                    return false;
                }
            }
        }
        true
    }

    /// Test a generic type application against one bound table.
    pub(crate) fn type_arguments_meet_bounds(
        &self,
        actual_types: &[u32],
        actual_rows: &[Vec<BcRow>],
        required_bounds: &[Vec<BcInterfaceUse>],
        scope_bounds: &[Vec<BcInterfaceUse>],
    ) -> bool {
        if actual_types.len() != required_bounds.len() {
            return false;
        }
        for (actual, bounds) in actual_types.iter().zip(required_bounds) {
            for bound in bounds {
                let required = self.subst_interface_use(bound, actual_types, actual_rows);
                let found = self.interface_application_with_bounds(
                    *actual,
                    required.interface,
                    scope_bounds,
                    0,
                );
                if found.as_ref() != Some(&required) {
                    return false;
                }
            }
        }
        true
    }

    /// Test method-owned arguments against bounds with an outer prefix.
    pub(crate) fn method_arguments_meet_bounds(
        &self,
        actual_types: &[u32],
        actual_rows: &[Vec<BcRow>],
        required_bounds: &[Vec<BcInterfaceUse>],
        prefix_types: &[u32],
        prefix_rows: &[Vec<BcRow>],
        scope_bounds: &[Vec<BcInterfaceUse>],
    ) -> bool {
        if actual_types.len() != required_bounds.len() {
            return false;
        }
        let mut types = Vec::with_capacity(prefix_types.len() + actual_types.len());
        types.extend_from_slice(prefix_types);
        types.extend_from_slice(actual_types);
        let mut rows = Vec::with_capacity(prefix_rows.len() + actual_rows.len());
        rows.extend_from_slice(prefix_rows);
        rows.extend_from_slice(actual_rows);
        for (actual, bounds) in actual_types.iter().zip(required_bounds) {
            for bound in bounds {
                let required = self.subst_interface_use(bound, &types, &rows);
                let found = self.interface_application_with_bounds(
                    *actual,
                    required.interface,
                    scope_bounds,
                    0,
                );
                if found.as_ref() != Some(&required) {
                    return false;
                }
            }
        }
        true
    }

    /// Test every associated projection in one type.
    pub(crate) fn projections_proven(&self, ty: u32, bounds: &[Vec<BcInterfaceUse>]) -> bool {
        if !self
            .uni
            .borrow()
            .facts
            .get(ty as usize)
            .is_some_and(|facts| facts.contains_projection)
        {
            return true;
        }
        let mut stack = vec![ty];
        let mut children = Vec::new();
        while let Some(current) = stack.pop() {
            if let BcType::Projection {
                base, interface, ..
            } = self.ty(current)
            {
                if self
                    .interface_application_with_bounds(base, interface, bounds, 0)
                    .is_none()
                {
                    return false;
                }
            }
            children.clear();
            self.type_children(current, &mut children);
            stack.extend(children.iter().copied());
        }
        true
    }

    /// Return true when a value of type `found` is valid where the
    /// code expects type `expected`.
    ///
    /// The walk is iterative. A tuple type and a function type both
    /// carry element types, and a crafted artifact can nest either as
    /// deeply as its type table allows.
    ///
    /// The work list holds pairs that must all hold. A pair the rules
    /// refuse answers false at once.
    pub(crate) fn is_subtype(&self, found: u32, expected: u32) -> bool {
        let mut work: Vec<(u32, u32)> = vec![(found, expected)];
        let mut seen: HashSet<(u32, u32)> = HashSet::new();
        while let Some((f, e)) = work.pop() {
            if f == e || !seen.insert((f, e)) {
                continue;
            }
            let ok = if let (Some((a, xs)), Some((b, ys))) =
                (self.as_instance(f), self.as_instance(e))
            {
                self.ancestor_args(a, &xs, b).as_ref() == Some(&ys)
            } else {
                match (self.ty(f), self.ty(e)) {
                    // A plain class position names no argument, so the
                    // walk to the ancestor must also reach it with no
                    // argument. A class that inherits an instantiated
                    // generic parent therefore fits no plain position
                    // of that parent.
                    (BcType::Class(a), BcType::Class(b)) => {
                        self.ancestor_args(a, &[], b) == Some(Vec::new())
                    }
                    // A class may inherit an instantiated generic
                    // parent, so a plain class instance can satisfy an
                    // application type.
                    (BcType::Class(a), BcType::Inst(b, ys)) => {
                        self.ancestor_args(a, &[], b).as_ref() == Some(&ys)
                    }
                    (BcType::Inst(a, xs), BcType::Class(b)) => {
                        self.ancestor_args(a, &xs, b) == Some(Vec::new())
                    }
                    (BcType::Inst(a, xs), BcType::Inst(b, ys)) => {
                        self.ancestor_args(a, &xs, b).as_ref() == Some(&ys)
                    }
                    (BcType::Tuple(xs), BcType::Tuple(ys)) => {
                        if xs.len() != ys.len() {
                            return false;
                        }
                        work.extend(xs.iter().zip(ys.iter()).map(|(x, y)| (*x, *y)));
                        true
                    }
                    (BcType::Fn(fp, fm, fr, frow), BcType::Fn(ep, em, er, erow)) => {
                        // A function that needs a `mut` argument is
                        // not valid where the expected type promises a
                        // read-only call. A parameter compares in the
                        // other direction.
                        if fp.len() != ep.len()
                            || !fm.iter().zip(em.iter()).all(|(f, e)| !*f || *e)
                            || !self.row_included(&frow, &erow)
                        {
                            return false;
                        }
                        work.extend(fp.iter().zip(ep.iter()).map(|(f, e)| (*e, *f)));
                        work.push((fr, er));
                        true
                    }
                    (BcType::Callback(fp, fm, fr, frow), BcType::Callback(ep, em, er, erow)) => {
                        if fp.len() != ep.len()
                            || !fm.iter().zip(em.iter()).all(|(f, e)| !*f || *e)
                            || !self.row_included(&frow, &erow)
                        {
                            return false;
                        }
                        work.extend(fp.iter().zip(ep.iter()).map(|(f, e)| (*e, *f)));
                        work.push((fr, er));
                        true
                    }
                    _ => false,
                }
            };
            if !ok {
                return false;
            }
        }
        true
    }

    /// Test the closed borrowed-key relation for one map lookup.
    pub(crate) fn accepts_map_lookup_key(&self, found: u32, expected: u32) -> bool {
        if self.is_subtype(found, expected) {
            return true;
        }
        let Some(text) = self.core.text else {
            return false;
        };
        let is_text = |ty| {
            self.as_instance(ty).is_some_and(|(class, args)| {
                self.ancestor_args(class, &args, text) == Some(Vec::new())
            })
        };
        is_text(found) && is_text(expected)
    }

    /// Join two types at a control-flow merge. Classes join at their
    /// nearest common ancestor. Unrelated types have no join.
    ///
    /// The walk is iterative. Only a tuple carries a nested join, so
    /// the stack holds the tuple positions the answer still needs.
    pub(crate) fn join(&self, a: u32, b: u32) -> Option<u32> {
        // A post-order walk over the pair DAG. The flag marks a pair
        // whose element pairs already have answers.
        let mut stack: Vec<(u32, u32, bool)> = vec![(a, b, false)];
        let mut done: HashMap<(u32, u32), Option<u32>> = HashMap::new();
        while let Some((x, y, expanded)) = stack.pop() {
            if done.contains_key(&(x, y)) {
                continue;
            }
            if !expanded {
                match self.join_flat(x, y)? {
                    Flat::Type(id) => {
                        done.insert((x, y), Some(id));
                    }
                    Flat::Tuple(xs, ys) => {
                        stack.push((x, y, true));
                        for (ex, ey) in xs.iter().zip(ys.iter()).rev() {
                            if !done.contains_key(&(*ex, *ey)) {
                                stack.push((*ex, *ey, false));
                            }
                        }
                    }
                }
                continue;
            }
            let (BcType::Tuple(xs), BcType::Tuple(ys)) = (self.ty(x), self.ty(y)) else {
                return None;
            };
            let mut elems = Vec::with_capacity(xs.len());
            for pair in xs.iter().copied().zip(ys.iter().copied()) {
                let Some(Some(joined)) = done.get(&pair) else {
                    done.insert((x, y), None);
                    elems.clear();
                    break;
                };
                elems.push(*joined);
            }
            if elems.len() != xs.len() {
                return None;
            }
            let joined = self.intern(BcType::Tuple(elems));
            done.insert((x, y), Some(joined));
        }
        done.remove(&(a, b)).flatten()
    }

    /// One join step that needs no nested answer.
    pub(crate) fn join_flat(&self, a: u32, b: u32) -> Option<Flat> {
        if let (BcType::Tuple(xs), BcType::Tuple(ys)) = (self.ty(a), self.ty(b)) {
            if xs.len() != ys.len() {
                return None;
            }
            return Some(Flat::Tuple(xs, ys));
        }
        if self.is_subtype(a, b) {
            return Some(Flat::Type(b));
        }
        if self.is_subtype(b, a) {
            return Some(Flat::Type(a));
        }
        let (ca, xs) = self.as_instance(a)?;
        let (cb, ys) = self.as_instance(b)?;
        let (common, args) = self.common_applied_ancestor(ca, &xs, cb, &ys)?;
        let joined = if self.module.classes[common as usize].type_params == 0 {
            if Some(common) == self.core.int {
                BcType::Int
            } else if Some(common) == self.core.float {
                BcType::Float
            } else if Some(common) == self.core.boolean {
                BcType::Bool
            } else if Some(common) == self.core.string {
                BcType::Str
            } else if Some(common) == self.core.bytes {
                BcType::Bytes
            } else {
                BcType::Class(common)
            }
        } else {
            BcType::Inst(common, args)
        };
        Some(Flat::Type(self.intern(joined)))
    }

    /// Find the nearest common ancestor with one equal application.
    pub(crate) fn common_applied_ancestor(
        &self,
        a: u32,
        a_args: &[u32],
        b: u32,
        b_args: &[u32],
    ) -> Option<(u32, Vec<u32>)> {
        let mut ancestor = Some(a);
        while let Some(class) = ancestor {
            let left = self.ancestor_args(a, a_args, class)?;
            if let Some(right) = self.ancestor_args(b, b_args, class) {
                if left == right {
                    return Some((class, left));
                }
            }
            ancestor = self.module.classes[class as usize].parent();
        }
        None
    }

    /// Find the nearest common nominal ancestor.
    pub(crate) fn common_ancestor(&self, a: u32, b: u32) -> Option<u32> {
        let mut ancestor = Some(a);
        while let Some(class) = ancestor {
            if self.ancestor_args(b, &[], class).is_some() {
                return Some(class);
            }
            ancestor = self.module.classes[class as usize].parent();
        }
        None
    }

    /// Resolve a selector on a class, walking the ancestor chain.
    pub(crate) fn find_method(&self, class: u32, selector: u32) -> Option<u32> {
        self.method_resolution(class, selector)
            .map(|(_, function)| function)
    }

    /// Test whether one class is an enum parent or an enum case.
    pub(crate) fn is_enum_class(&self, class: u32) -> bool {
        self.module
            .classes
            .get(class as usize)
            .map(|c| {
                matches!(
                    c.kind,
                    lm_bytecode::BcClassKind::Abstract | lm_bytecode::BcClassKind::Case
                )
            })
            .unwrap_or(false)
    }

    /// The nominal class and arguments of one instance type.
    pub(crate) fn as_instance(&self, ty: u32) -> Option<(u32, Vec<u32>)> {
        match self.ty(ty) {
            BcType::Unit => self.core.unit.map(|class| (class, vec![])),
            BcType::Int => self.core.int.map(|class| (class, vec![])),
            BcType::Float => self.core.float.map(|class| (class, vec![])),
            BcType::Bool => self.core.boolean.map(|class| (class, vec![])),
            BcType::Str => self.core.string.map(|class| (class, vec![])),
            BcType::Bytes => self.core.bytes.map(|class| (class, vec![])),
            BcType::FileHandle => self.core.file_handle.map(|class| (class, vec![])),
            BcType::List(element) => self.core.list.map(|class| (class, vec![element])),
            BcType::Map(key, value) => self.core.map.map(|class| (class, vec![key, value])),
            BcType::Tuple(elements) => self
                .core
                .tuples
                .get(elements.len())
                .copied()
                .flatten()
                .map(|class| (class, elements)),
            BcType::Class(c) => Some((c, vec![])),
            BcType::Inst(c, args) => Some((c, args)),
            _ => None,
        }
    }

    /// Return true when the type is a heap object type. An `Op` value
    /// is an immediate.
    pub(crate) fn is_heap(&self, idx: u32) -> bool {
        if let BcType::Class(class) = self.ty(idx) {
            if Some(class) == self.core.char_value {
                return false;
            }
        }
        matches!(
            self.ty(idx),
            BcType::Str
                | BcType::Class(_)
                | BcType::Inst(_, _)
                | BcType::List(_)
                | BcType::Map(_, _)
                | BcType::Tuple(_)
                | BcType::Fn(_, _, _, _)
                | BcType::Digest
                | BcType::Fault
                | BcType::Request
                | BcType::PolicyTable
                | BcType::Vm
                | BcType::Run(_)
                | BcType::Wait(_)
                | BcType::PendingCall(_, _)
                | BcType::Handle(_, _)
                | BcType::VmSnapshot
                | BcType::RunSnapshot(_)
                | BcType::Bytes
                | BcType::FileHandle
                | BcType::ResourceHandle
                | BcType::HostResource
        )
    }

    /// Return true when one value position contains a callback.
    pub(crate) fn stores_callback(&self, ty: u32) -> bool {
        self.uni
            .borrow()
            .facts
            .get(ty as usize)
            .map(|facts| facts.stores_callback)
            .unwrap_or(false)
    }

    /// Check that every type variable inside `ty` is below `limit`
    /// and every row variable is below `elimit`.
    ///
    pub(crate) fn vars_bounded(&self, ty: u32, limit: u32, elimit: u32) -> bool {
        let uni = self.uni.borrow();
        let Some(facts) = uni.facts.get(ty as usize) else {
            return false;
        };
        facts.max_type_var.is_none_or(|var| var < limit)
            && facts.max_effect_var.is_none_or(|var| var < elimit)
    }

    pub(crate) fn row_vars_bounded(&self, row: &[BcRow], elimit: u32) -> bool {
        row.iter().all(|e| match e {
            BcRow::Var(v) => *v < elimit,
            BcRow::Op(_) | BcRow::Group(_) => true,
        })
    }

    /// True when a claimed row covers one exact operation name: the
    /// row holds the exact name or one containing effect set.
    pub(crate) fn row_has_name(&self, row: &[BcRow], name: &str) -> bool {
        row.iter().any(|elem| match elem {
            BcRow::Op(idx) => self
                .bundle
                .op_name(*idx)
                .is_some_and(|text| self.bundle.row_name_included(name, text)),
            BcRow::Group(idx) => self
                .bundle
                .group_name(*idx)
                .is_some_and(|text| self.bundle.row_name_included(name, text)),
            BcRow::Var(_) => false,
        })
    }

    /// Convert one manifest type into a universe type index. The core
    /// enums must be present when a signature names them.
    pub(crate) fn abi_ty(&self, t: lm_abi::AbiType) -> Result<u32, String> {
        match t {
            lm_abi::AbiType::Primitive(primitive) => match primitive {
                lm_abi::AbiPrimitive::Unit => Ok(TY_UNIT),
                lm_abi::AbiPrimitive::Never => Err("Never has no runtime value".to_string()),
                lm_abi::AbiPrimitive::Bool => Ok(TY_BOOL),
                lm_abi::AbiPrimitive::Int => Ok(TY_INT),
                lm_abi::AbiPrimitive::Float => Ok(self.intern(BcType::Float)),
                lm_abi::AbiPrimitive::String => Ok(TY_STR),
                lm_abi::AbiPrimitive::Bytes => Ok(self.intern(BcType::Bytes)),
                lm_abi::AbiPrimitive::VmSnapshot => Ok(self.intern(BcType::VmSnapshot)),
                lm_abi::AbiPrimitive::Fault => Ok(self.intern(BcType::Fault)),
            },
            lm_abi::AbiType::Core(core) => {
                let (slot, name) = match core {
                    lm_abi::AbiCore::Text => (self.core.text, "Text"),
                    lm_abi::AbiCore::Substring => (self.core.substring, "Substring"),
                    lm_abi::AbiCore::Char => (self.core.char_value, "Char"),
                    lm_abi::AbiCore::StringBuilder => (self.core.string_builder, "StringBuilder"),
                    lm_abi::AbiCore::ByteBuffer => (self.core.byte_buffer, "ByteBuffer"),
                    lm_abi::AbiCore::OpenOptions => (self.core.open_options, "OpenOptions"),
                    lm_abi::AbiCore::SeekFrom => (self.core.seek_from, "SeekFrom"),
                    lm_abi::AbiCore::FileKind => (self.core.file_kind, "FileKind"),
                    lm_abi::AbiCore::FileInfo => (self.core.file_info, "FileInfo"),
                    lm_abi::AbiCore::DirEntry => (self.core.dir_entry, "DirEntry"),
                    lm_abi::AbiCore::RenameMode => (self.core.rename_mode, "RenameMode"),
                    lm_abi::AbiCore::IoError => (self.core.io_error, "IoError"),
                    lm_abi::AbiCore::FsError => (self.core.fs_error, "FsError"),
                    lm_abi::AbiCore::EnvError => (self.core.env_error, "EnvError"),
                    lm_abi::AbiCore::EntropyError => (self.core.entropy_error, "EntropyError"),
                    lm_abi::AbiCore::SnapshotError => (self.core.snapshot_error, "SnapshotError"),
                    lm_abi::AbiCore::IpAddress => (self.core.ip_address, "IpAddress"),
                    lm_abi::AbiCore::SocketAddress => (self.core.socket_address, "SocketAddress"),
                    lm_abi::AbiCore::NetError => (self.core.net_error, "NetError"),
                    lm_abi::AbiCore::TcpRead => (self.core.tcp_read, "TcpRead"),
                    lm_abi::AbiCore::Shutdown => (self.core.shutdown, "Shutdown"),
                    lm_abi::AbiCore::TlsError => (self.core.tls_error, "TlsError"),
                    lm_abi::AbiCore::Artifact => (self.core.artifact, "Artifact"),
                    lm_abi::AbiCore::CompileEnv => (self.core.compile_env, "CompileEnv"),
                    lm_abi::AbiCore::CompileOptions => {
                        (self.core.compile_options, "CompileOptions")
                    }
                    lm_abi::AbiCore::CompileErrors => (self.core.compile_errors, "CompileErrors"),
                    lm_abi::AbiCore::DynValue => (self.core.dyn_value, "DynValue"),
                    lm_abi::AbiCore::SyntaxTree => (self.core.syntax_tree, "SyntaxTree"),
                    lm_abi::AbiCore::SyntaxElement => (self.core.syntax_element, "SyntaxElement"),
                    lm_abi::AbiCore::SyntaxNode => (self.core.syntax_node, "SyntaxNode"),
                    lm_abi::AbiCore::SyntaxToken => (self.core.syntax_token, "SyntaxToken"),
                    lm_abi::AbiCore::SyntaxTrivia => (self.core.syntax_trivia, "SyntaxTrivia"),
                    lm_abi::AbiCore::SyntaxBuilder => (self.core.syntax_builder, "SyntaxBuilder"),
                    lm_abi::AbiCore::SyntaxParse => (self.core.syntax_parse, "SyntaxParse"),
                    lm_abi::AbiCore::StdStream => (self.core.std_stream, "StdStream"),
                    lm_abi::AbiCore::TtySize => (self.core.tty_size, "TtySize"),
                    lm_abi::AbiCore::TtyError => (self.core.tty_error, "TtyError"),
                    lm_abi::AbiCore::SignalKind => (self.core.signal_kind, "SignalKind"),
                    lm_abi::AbiCore::SignalError => (self.core.signal_error, "SignalError"),
                    lm_abi::AbiCore::PipeError => (self.core.pipe_error, "PipeError"),
                    lm_abi::AbiCore::ChildInput => (self.core.child_input, "ChildInput"),
                    lm_abi::AbiCore::ChildOutput => (self.core.child_output, "ChildOutput"),
                    lm_abi::AbiCore::ChildEnv => (self.core.child_env, "ChildEnv"),
                    lm_abi::AbiCore::ExecSpec => (self.core.exec_spec, "ExecSpec"),
                    lm_abi::AbiCore::ChildStatus => (self.core.child_status, "ChildStatus"),
                    lm_abi::AbiCore::ExecError => (self.core.exec_error, "ExecError"),
                    lm_abi::AbiCore::UdpDatagram => (self.core.udp_datagram, "UdpDatagram"),
                };
                self.plain_inst(slot, name)
            }
            lm_abi::AbiType::Native(native) => match native {
                lm_abi::AbiNative::FileHandle => Ok(self.intern(BcType::FileHandle)),
                lm_abi::AbiNative::TcpResource => {
                    self.plain_inst(self.core.tcp_resource, "TcpResource")
                }
                lm_abi::AbiNative::TcpStream => self.plain_inst(self.core.tcp_stream, "TcpStream"),
                lm_abi::AbiNative::TcpListener => {
                    self.plain_inst(self.core.tcp_listener, "TcpListener")
                }
                lm_abi::AbiNative::TlsStream => self.plain_inst(self.core.tls_stream, "TlsStream"),
                lm_abi::AbiNative::RawMode => self.plain_inst(self.core.raw_mode, "RawMode"),
                lm_abi::AbiNative::SignalStream => {
                    self.plain_inst(self.core.signal_stream, "SignalStream")
                }
                lm_abi::AbiNative::PipeEnd => self.plain_inst(self.core.pipe_end, "PipeEnd"),
                lm_abi::AbiNative::PipeReader => {
                    self.plain_inst(self.core.pipe_reader, "PipeReader")
                }
                lm_abi::AbiNative::PipeWriter => {
                    self.plain_inst(self.core.pipe_writer, "PipeWriter")
                }
                lm_abi::AbiNative::Child => self.plain_inst(self.core.child, "Child"),
                lm_abi::AbiNative::UdpSocket => self.plain_inst(self.core.udp_socket, "UdpSocket"),
            },
            lm_abi::AbiType::Var(index) => Err(format!(
                "the fixed ABI type names generic parameter {index}"
            )),
            lm_abi::AbiType::List(element) => {
                let element = self.abi_ty(*element)?;
                Ok(self.intern(BcType::List(element)))
            }
            lm_abi::AbiType::Map(key, value) => {
                let key = self.abi_ty(*key)?;
                let value = self.abi_ty(*value)?;
                Ok(self.intern(BcType::Map(key, value)))
            }
            lm_abi::AbiType::Tuple(elements) => {
                let mut types = Vec::with_capacity(elements.len());
                for element in elements {
                    types.push(self.abi_ty(*element)?);
                }
                Ok(self.intern(BcType::Tuple(types)))
            }
            lm_abi::AbiType::Apply(constructor, arguments) => {
                if arguments.len() != constructor.arity() {
                    return Err(format!(
                        "the ABI type {} has the wrong generic arity",
                        t.text()
                    ));
                }
                let class = match constructor {
                    lm_abi::AbiConstructor::Option => self.core.option,
                    lm_abi::AbiConstructor::Result => self.core.result,
                }
                .ok_or_else(|| {
                    format!(
                        "the module does not carry the pinned core {} definition",
                        constructor.text()
                    )
                })?;
                let mut types = Vec::with_capacity(arguments.len());
                for argument in arguments {
                    types.push(self.abi_ty(*argument)?);
                }
                Ok(self.intern(BcType::Inst(class, types)))
            }
            lm_abi::AbiType::Resource(_) => Ok(self.intern(BcType::HostResource)),
        }
    }

    /// The function type of one fixed operation as a universe index.
    pub(crate) fn fixed_sig_type(&self, op: u32) -> Result<u32, String> {
        let def = self
            .bundle
            .op(op)
            .ok_or_else(|| format!("operation slot {op} is outside the ABI bundle"))?;
        let mut params = Vec::with_capacity(def.params.len());
        for p in &def.params {
            params.push(self.abi_ty(*p)?);
        }
        let ret = self.abi_ty(def.reply)?;
        let muts = vec![false; params.len()];
        Ok(self.intern(BcType::Fn(params, muts, ret, vec![])))
    }

    /// The argument-view type of one fixed operation: unit for a
    /// zero-parameter operation, a tuple otherwise.
    pub(crate) fn op_args_view(&self, op: u32) -> Result<u32, String> {
        let def = self
            .bundle
            .op(op)
            .ok_or_else(|| format!("operation slot {op} is outside the ABI bundle"))?;
        if def.params.is_empty() {
            return Ok(TY_UNIT);
        }
        let mut elems = Vec::with_capacity(def.params.len());
        for p in &def.params {
            elems.push(self.abi_ty(*p)?);
        }
        Ok(self.intern(BcType::Tuple(elems)))
    }

    /// One VM event instance type, for example `StepEvent[t]`.
    /// The instance type of one core family without type parameters.
    pub(crate) fn plain_inst(&self, parent: Option<u32>, what: &str) -> Result<u32, String> {
        let Some(parent) = parent else {
            return Err(format!(
                "the module does not carry the pinned core {what} definition"
            ));
        };
        Ok(self.intern(BcType::Class(parent)))
    }

    /// One `Result[ok, error]` instance type.
    pub(crate) fn result_inst(&self, ok: u32, error: u32) -> Result<u32, String> {
        let Some(family) = self.core.result else {
            return Err("the module does not carry the pinned core Result definition".to_string());
        };
        Ok(self.intern(BcType::Inst(family, vec![ok, error])))
    }

    /// The mailbox message type of one proc instance type. `None` when
    /// the type is not an instance of a subclass of the core class
    /// `Proc`.
    pub(crate) fn proc_mailbox(&self, ty: u32) -> Option<u32> {
        let proc = self.core.proc_class?;
        let (class, args) = self.as_instance(ty)?;
        let found = self.ancestor_args(class, &args, proc)?;
        found.first().copied()
    }

    pub(crate) fn event_inst(
        &self,
        parent: Option<u32>,
        what: &str,
        arg: u32,
    ) -> Result<u32, String> {
        let Some(parent) = parent else {
            return Err(format!(
                "the module does not carry the pinned core {what} definition"
            ));
        };
        Ok(self.intern(BcType::Inst(parent, vec![arg])))
    }
}
