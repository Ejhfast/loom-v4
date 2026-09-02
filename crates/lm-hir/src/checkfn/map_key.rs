//! Closed borrowed-key rules for native maps.

use super::*;

/// How one map operation uses its key argument.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MapKeyUse {
    /// The operation compares a key but does not store it.
    Lookup,
    /// The operation can store the key after a miss.
    Insert,
}

impl MapKeyUse {
    /// Return the key use for one built-in map method.
    pub(super) fn for_method(name: &str) -> Option<Self> {
        match name {
            "has" | "at" | "get" | "remove" => Some(Self::Lookup),
            "put" => Some(Self::Insert),
            _ => None,
        }
    }
}

/// One compiler-defined borrowed-key relation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BorrowedKeyRelation {
    /// Compare Text by visible UTF-8 content.
    TextContent,
}

/// The accepted parameter type and its optional relation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct MapKeyParameter {
    pub(super) ty: TypeId,
    relation: Option<BorrowedKeyRelation>,
}

impl MapKeyParameter {
    /// Test whether the operation borrows its argument.
    pub(super) fn is_borrowed(self) -> bool {
        self.relation.is_some()
    }
}

/// Resolve the closed borrowed-key relation for one map operation.
pub(super) fn map_key_parameter(ctx: &Ctx, key: TypeId, usage: MapKeyUse) -> MapKeyParameter {
    let exact = MapKeyParameter {
        ty: key,
        relation: None,
    };
    let Some(text_class) = ctx.core_types.get("Text").copied() else {
        return exact;
    };
    let text = ctx.classes[text_class as usize].self_ty;
    let text_key = ctx.store.compatible(text, key);
    let accepts_text = match usage {
        MapKeyUse::Lookup => text_key,
        MapKeyUse::Insert => key == STRING,
    };
    if !accepts_text || text == key {
        return exact;
    }
    MapKeyParameter {
        ty: text,
        relation: Some(BorrowedKeyRelation::TextContent),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_keyed_map_method_has_one_use() {
        for name in ["has", "at", "get", "put", "remove"] {
            assert!(MapKeyUse::for_method(name).is_some(), "{name}");
        }
        for name in ["len", "clear", "reserve", "keys_list"] {
            assert!(MapKeyUse::for_method(name).is_none(), "{name}");
        }
    }
}
