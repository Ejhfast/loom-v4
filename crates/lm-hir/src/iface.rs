//! Build the exported interface items of one checked module.
//!
//! An interface item is the structural signature of one export, with
//! every class reference by qualified name. The checker knows the
//! facts an interface needs and the bytecode module does not: the
//! declared parameter names, which fields carry a default, the `init`
//! signature, and the enum arm names.

use crate::check::{ClassInfo, Ctx, FnSig, MethodSig};
use lm_bytecode::interface::{
    IfaceAssociated, IfaceClass, IfaceClassKind, IfaceConformance, IfaceConformancePremise,
    IfaceField, IfaceFn, IfaceInterface, IfaceInterfaceMethod, IfaceInterfaceUse, IfaceItem,
    IfaceMethod, IfaceRow, IfaceType, QualName,
};
use lm_types::{ClassKind, Row, RowElem, Type, TypeId, TypeStore};

/// Where one class of the checked module comes from.
pub(crate) enum ClassOrigin {
    /// A definition of the module being compiled.
    Local,
    /// A definition of the pinned core image.
    Core,
    /// An imported declaration, by providing module and export name.
    Imported(String, String),
}

/// The qualified-name view of one checked module.
pub(crate) struct Naming<'a> {
    pub(crate) ctx: &'a Ctx,
    /// The path of the module being compiled.
    pub(crate) module_path: &'a str,
}

impl Naming<'_> {
    /// The qualified key of one class: its nominal identity
    /// (specification 8.6).
    ///
    /// A core class takes the reserved module path `core`. A source
    /// module path never equals that value, so a user class never
    /// takes a core key. The interface uses the empty path for a core
    /// class instead, because an interface reader already knows that
    /// every module carries the core.
    pub(crate) fn key(&self, class: u32) -> String {
        let info = &self.ctx.classes[class as usize];
        match self.ctx.class_origin(class) {
            ClassOrigin::Local => lm_bytecode::qualified_key(self.module_path, &info.name),
            ClassOrigin::Core => lm_bytecode::qualified_key(lm_bytecode::CORE_MODULE, &info.name),
            ClassOrigin::Imported(module, name) => lm_bytecode::qualified_key(&module, &name),
        }
    }

    /// The qualified name of one class.
    pub(crate) fn qual(&self, class: u32) -> QualName {
        let info = &self.ctx.classes[class as usize];
        match self.ctx.class_origin(class) {
            ClassOrigin::Local => QualName::new(self.module_path, info.name.clone()),
            ClassOrigin::Core => QualName::new("", info.name.clone()),
            ClassOrigin::Imported(module, name) => QualName::new(module, name),
        }
    }

    /// The qualified name of one nominal interface.
    pub(crate) fn interface_qual(&self, interface: u32) -> QualName {
        let info = &self.ctx.interfaces[interface as usize];
        if self.ctx.interface_is_core(interface) {
            QualName::new("", info.name.clone())
        } else if info.imported {
            let (module, name) = info
                .origin
                .as_ref()
                .expect("an imported interface records its origin");
            QualName::new(module.clone(), name.clone())
        } else {
            QualName::new(self.module_path, info.name.clone())
        }
    }

    fn store(&self) -> &TypeStore {
        &self.ctx.store
    }

    /// Convert one checked type to its interface form.
    pub(crate) fn ty(&self, id: TypeId) -> IfaceType {
        match self.store().get(id).clone() {
            Type::Unit => IfaceType::Unit,
            Type::Bool => IfaceType::Bool,
            Type::Int => IfaceType::Int,
            Type::Float => IfaceType::Float,
            Type::String => IfaceType::Str,
            // `Never` cannot appear in a declared signature; the
            // checker widens it to the declared result type.
            Type::Never => IfaceType::Unit,
            Type::Bytes => IfaceType::Bytes,
            Type::FileHandle => IfaceType::FileHandle,
            Type::ResourceHandle => IfaceType::ResourceHandle,
            Type::HostResource => IfaceType::HostResource,
            Type::Digest => IfaceType::Digest,
            Type::Fault => IfaceType::Fault,
            Type::Request => IfaceType::Request,
            Type::PolicyTable => IfaceType::PolicyTable,
            Type::Vm => IfaceType::Vm,
            Type::VmSnapshot => IfaceType::VmSnapshot,
            Type::Class(c) => IfaceType::Named {
                class: self.qual(c.0),
                args: vec![],
            },
            Type::Inst(c, args) => IfaceType::Named {
                class: self.qual(c.0),
                args: args.iter().map(|a| self.ty(*a)).collect(),
            },
            Type::List(e) => IfaceType::List(Box::new(self.ty(e))),
            Type::Map(k, v) => IfaceType::Map(Box::new(self.ty(k)), Box::new(self.ty(v))),
            Type::Tuple(elems) => IfaceType::Tuple(elems.iter().map(|e| self.ty(*e)).collect()),
            Type::Fn(params, muts, ret, row) => IfaceType::Fn {
                params: params.iter().map(|p| self.ty(*p)).collect(),
                param_muts: muts.clone(),
                ret: Box::new(self.ty(ret)),
                row: self.row(&row),
            },
            Type::Callback(params, muts, ret, row) => IfaceType::Callback {
                params: params.iter().map(|p| self.ty(*p)).collect(),
                param_muts: muts.clone(),
                ret: Box::new(self.ty(ret)),
                row: self.row(&row),
            },
            Type::Var(i) => IfaceType::Var(i),
            Type::Projection {
                base,
                interface,
                assoc,
            } => {
                let name = self.ctx.interfaces[interface.0 as usize]
                    .associated
                    .get(assoc as usize)
                    .map(|item| item.name.clone())
                    .unwrap_or_else(|| format!("assoc{assoc}"));
                IfaceType::Projection {
                    base: Box::new(self.ty(base)),
                    interface: self.interface_qual(interface.0),
                    assoc: name,
                }
            }
            Type::Run(t) => IfaceType::Run(Box::new(self.ty(t))),
            Type::Wait(t) => IfaceType::Wait(Box::new(self.ty(t))),
            Type::RunSnapshot(t) => IfaceType::RunSnapshot(Box::new(self.ty(t))),
            Type::PendingCall(a, r) => {
                IfaceType::PendingCall(Box::new(self.ty(a)), Box::new(self.ty(r)))
            }
            Type::Handle(m, r) => IfaceType::Handle(Box::new(self.ty(m)), Box::new(self.ty(r))),
            Type::Op(op, f) => IfaceType::Op(op, Box::new(self.ty(f))),
        }
    }

    /// Convert one canonical row to its interface form.
    pub(crate) fn row(&self, row: &Row) -> Vec<IfaceRow> {
        row.iter()
            .map(|elem| match elem {
                RowElem::Op(idx) => IfaceRow::Op(self.store().row_name(*idx).to_string()),
                RowElem::Var(v) => IfaceRow::Var(*v),
            })
            .collect()
    }

    fn interface_use(&self, application: &crate::check::InterfaceUse) -> IfaceInterfaceUse {
        IfaceInterfaceUse {
            interface: self.interface_qual(application.interface),
            types: application
                .type_args
                .iter()
                .map(|ty| self.ty(*ty))
                .collect(),
            rows: application
                .row_args
                .iter()
                .map(|row| self.row(row))
                .collect(),
        }
    }

    fn bounds(&self, bounds: &[Vec<crate::check::InterfaceUse>]) -> Vec<Vec<IfaceInterfaceUse>> {
        bounds
            .iter()
            .map(|items| items.iter().map(|item| self.interface_use(item)).collect())
            .collect()
    }

    /// Convert one top-level function signature.
    pub(crate) fn func(&self, sig: &FnSig) -> IfaceFn {
        IfaceFn {
            type_params: sig.type_params.len() as u32,
            type_bounds: self.bounds(&sig.type_bounds),
            effect_params: sig.effect_params.len() as u32,
            params: sig.params.iter().map(|p| self.ty(*p)).collect(),
            param_muts: sig.param_muts.clone(),
            param_names: sig.param_names.clone(),
            ret: self.ty(sig.ret),
            row: self.row(&sig.row),
        }
    }

    /// Convert one method signature. `self` stays out of the lists.
    fn method(&self, sig: &MethodSig, class: &ClassInfo) -> IfaceMethod {
        let class_type_params = class.type_params.len() as u32;
        let mut type_bounds = self.bounds(&sig.class_type_bounds);
        type_bounds.extend(self.bounds(&sig.own_type_bounds));
        IfaceMethod {
            name: sig.name.clone(),
            mut_self: sig.mut_self,
            sig: IfaceFn {
                // A method carries the class type parameters first and
                // its own after them. The interface repeats the same
                // convention, so the count is the total.
                type_params: class_type_params + sig.own_type_params.len() as u32,
                type_bounds,
                effect_params: sig.own_effect_params.len() as u32,
                params: sig.params.iter().map(|p| self.ty(*p)).collect(),
                param_muts: sig.param_muts.clone(),
                param_names: sig.param_names.clone(),
                ret: self.ty(sig.ret),
                row: self.row(&sig.row),
            },
        }
    }

    /// Convert one class or enum declaration.
    pub(crate) fn class(&self, idx: u32) -> IfaceClass {
        let info: &ClassInfo = &self.ctx.classes[idx as usize];
        let type_params = info.type_params.len() as u32;
        let kind = match info.kind {
            ClassKind::Normal => IfaceClassKind::Normal,
            ClassKind::EnumParent => IfaceClassKind::EnumParent,
            ClassKind::EnumCase => IfaceClassKind::EnumCase,
        };
        let fields = info
            .field_names
            .iter()
            .zip(info.field_tys.iter())
            .zip(info.has_default.iter())
            .map(|((name, ty), has_default)| IfaceField {
                name: name.clone(),
                ty: self.ty(*ty),
                has_default: *has_default,
            })
            .collect();
        IfaceClass {
            kind,
            is_final: info.is_final,
            is_frozen: info.is_frozen,
            type_params,
            type_bounds: self.bounds(&info.type_bounds),
            conformances: info
                .conformances
                .iter()
                .map(|conformance| IfaceConformance {
                    application: self.interface_use(&conformance.application),
                    premises: conformance
                        .premises
                        .iter()
                        .map(|premise| IfaceConformancePremise {
                            param: premise.param,
                            bounds: premise
                                .bounds
                                .iter()
                                .map(|bound| self.interface_use(bound))
                                .collect(),
                        })
                        .collect(),
                    associated: conformance
                        .associated
                        .iter()
                        .map(|ty| self.ty(*ty))
                        .collect(),
                    method_overrides: conformance.method_overrides.clone(),
                })
                .collect(),
            parent: info.parent.map(|p| self.qual(p)),
            fields,
            own_start: info.own_start as u32,
            methods: info.methods.iter().map(|m| self.method(m, info)).collect(),
            init: info.init.as_ref().map(|m| IfaceFn {
                type_params,
                type_bounds: self.bounds(&info.type_bounds),
                effect_params: m.own_effect_params.len() as u32,
                params: m.params.iter().map(|p| self.ty(*p)).collect(),
                param_muts: m.param_muts.clone(),
                param_names: m.param_names.clone(),
                ret: IfaceType::Unit,
                row: self.row(&m.row),
            }),
            arms: info
                .arms
                .iter()
                .map(|a| self.ctx.classes[*a as usize].arm_short.clone())
                .collect(),
            arm_short: info.arm_short.clone(),
            family: info.family.map(|f| self.qual(f)),
        }
    }

    /// Convert one nominal interface declaration.
    pub(crate) fn interface(&self, idx: u32) -> IfaceInterface {
        let info = &self.ctx.interfaces[idx as usize];
        IfaceInterface {
            type_params: info.type_params.len() as u32,
            effect_params: info.effect_params.len() as u32,
            generic_is_effect: info.generic_is_effect.clone(),
            parents: info
                .parents
                .iter()
                .map(|parent| self.interface_use(parent))
                .collect(),
            type_bounds: self.bounds(&info.type_bounds),
            associated: info
                .associated
                .iter()
                .map(|associated| IfaceAssociated {
                    name: associated.name.clone(),
                    bounds: associated
                        .bounds
                        .iter()
                        .map(|bound| self.interface_use(bound))
                        .collect(),
                })
                .collect(),
            methods: info
                .methods
                .iter()
                .map(|method| IfaceInterfaceMethod {
                    name: method.name.clone(),
                    mut_self: method.mut_self,
                    type_params: method.own_type_params.len() as u32,
                    type_bounds: self.bounds(&method.own_type_bounds),
                    effect_params: method.own_effect_params.len() as u32,
                    premises: method
                        .premises
                        .iter()
                        .map(|premise| lm_bytecode::interface::IfaceTypePremise {
                            subject: self.ty(premise.subject),
                            bounds: premise
                                .bounds
                                .iter()
                                .map(|bound| self.interface_use(bound))
                                .collect(),
                        })
                        .collect(),
                    params: method.params.iter().map(|ty| self.ty(*ty)).collect(),
                    param_muts: method.param_muts.clone(),
                    param_names: method.param_names.clone(),
                    ret: self.ty(method.ret),
                    row: self.row(&method.row),
                    default: method.default_binding.clone(),
                })
                .collect(),
        }
    }

    /// The interface item of one export.
    pub(crate) fn item(&self, kind: lm_bytecode::ExportKind, def: u32) -> IfaceItem {
        if kind.is_class() {
            IfaceItem::Class(self.class(def))
        } else if kind.is_interface() {
            IfaceItem::Interface(self.interface(def))
        } else {
            IfaceItem::Func(self.func(&self.ctx.sigs[def as usize]))
        }
    }
}
