//! Collection instruction helpers.

use super::*;

impl Machine {
    /// Read one list element outside the main dispatch body.
    #[inline(never)]
    pub(super) fn exec_list_at(&mut self) -> Result<(), FaultCode> {
        let idx = self.pop_int()?;
        let r = self.pop_obj()?;
        let value = match self.vm.heap.get(r) {
            Object::List { items, .. } => {
                if idx < 0 || idx as usize >= items.len() {
                    return Err(FaultCode::IndexOutOfBounds);
                }
                items[idx as usize]
            }
            _ => return Err(BAD_TYPE),
        };
        self.push(value)
    }

    /// Insert one map entry outside the base dispatch body.
    #[inline(never)]
    pub(super) fn exec_map_put(
        &mut self,
        module: &NamespaceRuntime,
        envs: &mut TypeEnvs,
        ty: u32,
        discard: bool,
    ) -> Result<(), FaultCode> {
        self.exec_map_put_inner(module, envs, ty, discard, false)
    }

    /// Insert into a String map through one borrowed Text key.
    #[inline(never)]
    pub(super) fn exec_map_put_text(
        &mut self,
        module: &NamespaceRuntime,
        envs: &mut TypeEnvs,
        ty: u32,
        discard: bool,
    ) -> Result<(), FaultCode> {
        self.exec_map_put_inner(module, envs, ty, discard, true)
    }

    fn exec_map_put_inner(
        &mut self,
        module: &NamespaceRuntime,
        envs: &mut TypeEnvs,
        ty: u32,
        discard: bool,
        own_text_key: bool,
    ) -> Result<(), FaultCode> {
        let value = self.pop()?;
        let mut key = self.pop()?;
        let r = self.pop_obj()?;
        self.frozen_guard(r)?;
        let pos = self.map_lookup(r, key)?;
        let previous = match pos {
            Some(pos) => match self.vm.heap.get_mut(r) {
                Object::Map { entries, .. } => {
                    let entry = entries.get_mut(pos).ok_or(BAD_STATE)?;
                    if discard {
                        entry.value = value;
                        None
                    } else {
                        Some(std::mem::replace(&mut entry.value, value))
                    }
                }
                _ => return Err(BAD_TYPE),
            },
            None => {
                let hash = self.key_semantic_hash(key)?;
                let owned_key = if own_text_key {
                    self.owned_string_map_key(key)?
                } else {
                    None
                };
                let key_cost = owned_key
                    .as_ref()
                    .map(|object| self.vm.heap.allocation_cost(object))
                    .unwrap_or(0);
                let growth = 40usize.checked_add(key_cost).ok_or(FaultCode::HeapLimit)?;
                self.reserve(growth, &[Value::Obj(r), key, value])?;
                if let Some(object) = owned_key {
                    key = Value::Obj(self.vm.heap.alloc(object));
                }
                match self.vm.heap.get_mut(r) {
                    Object::Map { entries, index } => {
                        index.epoch.bump()?;
                        let position = entries.len() as u32;
                        entries.push(MapEntry {
                            key,
                            value,
                            semantic_hash: hash,
                        });
                        index.push_live(Self::map_index_hash(hash), position);
                    }
                    _ => return Err(BAD_TYPE),
                }
                self.vm.heap.recharge(r);
                None
            }
        };
        if !discard {
            match previous {
                Some(previous) => self.push(previous)?,
                None => {
                    let ty = self.close_option_family(module, envs, ty)?;
                    self.push(Value::EmptyCase { ty, arm: 1 })?;
                }
            }
        }
        Ok(())
    }

    /// Execute one native option collection operation.
    #[inline(never)]
    pub(super) fn exec_option_collection(
        &mut self,
        module: &NamespaceRuntime,
        envs: &mut TypeEnvs,
        op: OptionCollectionOp,
    ) -> Result<(), FaultCode> {
        match op {
            OptionCollectionOp::OptionNone(ty) => {
                let ty = self.close_option_family(module, envs, ty)?;
                self.push(Value::EmptyCase { ty, arm: 1 })?;
            }
            OptionCollectionOp::OptionPayload(ty) => {
                let value = *self.vm.operands.last().ok_or(BAD_STATE)?;
                let family = self.close_option_family(module, envs, ty)?;
                if matches!(value, Value::EmptyCase { ty, arm: 1 } if ty == family) {
                    return Err(BAD_TYPE);
                }
            }
            OptionCollectionOp::ListGet(ty) => {
                let idx = self.pop_int()?;
                let r = self.pop_obj()?;
                let value = match self.vm.heap.get(r) {
                    Object::List { items, .. } if idx >= 0 => items.get(idx as usize).copied(),
                    Object::List { .. } => None,
                    _ => return Err(BAD_TYPE),
                };
                match value {
                    Some(value) => self.push(value)?,
                    None => {
                        let ty = self.close_option_family(module, envs, ty)?;
                        self.push(Value::EmptyCase { ty, arm: 1 })?;
                    }
                }
            }
            OptionCollectionOp::MapGet(ty) => {
                let key = self.pop()?;
                let r = self.pop_obj()?;
                let value = match self.map_lookup(r, key)? {
                    Some(pos) => match self.vm.heap.get(r) {
                        Object::Map { entries, .. } => {
                            Some(entries.get(pos).ok_or(BAD_STATE)?.value)
                        }
                        _ => return Err(BAD_TYPE),
                    },
                    None => None,
                };
                match value {
                    Some(value) => self.push(value)?,
                    None => {
                        let ty = self.close_option_family(module, envs, ty)?;
                        self.push(Value::EmptyCase { ty, arm: 1 })?;
                    }
                }
            }
        }
        Ok(())
    }

    /// Execute one collection traversal operation.
    #[inline(never)]
    pub(super) fn exec_collection_iteration(
        &mut self,
        op: CollectionIterationOp,
    ) -> Result<(), FaultCode> {
        match op {
            CollectionIterationOp::ListEpoch => {
                let r = self.pop_obj()?;
                let epoch = match self.vm.heap.get_mut(r) {
                    Object::List { epoch, .. } => epoch.observe(),
                    _ => return Err(BAD_TYPE),
                };
                self.push(Value::Int(i64::from(epoch)))?;
            }
            CollectionIterationOp::ListIterLen => {
                let expected = self.pop_int()?;
                let r = self.pop_obj()?;
                let (len, epoch) = match self.vm.heap.get(r) {
                    Object::List { items, epoch } => (items.len(), epoch.0),
                    _ => return Err(BAD_TYPE),
                };
                if expected < 0 || epoch != expected as u32 {
                    return Err(FaultCode::CollectionModified);
                }
                self.push(Value::Int(len as i64))?;
            }
            CollectionIterationOp::MapEpoch => {
                let r = self.pop_obj()?;
                let epoch = match self.vm.heap.get_mut(r) {
                    Object::Map { index, .. } => index.epoch.observe(),
                    _ => return Err(BAD_TYPE),
                };
                self.push(Value::Int(i64::from(epoch)))?;
            }
            CollectionIterationOp::MapIterLen => {
                let expected = self.pop_int()?;
                let r = self.pop_obj()?;
                let (len, epoch) = match self.vm.heap.get(r) {
                    Object::Map { index, .. } => (index.live_len(), index.epoch.0),
                    _ => return Err(BAD_TYPE),
                };
                if expected < 0 || epoch != expected as u32 {
                    return Err(FaultCode::CollectionModified);
                }
                self.push(Value::Int(len as i64))?;
            }
            CollectionIterationOp::MapNextIndex => {
                let expected = self.pop_int()?;
                let cursor = self.pop_int()?;
                let r = self.pop_obj()?;
                let Object::Map { entries, index } = self.vm.heap.get(r) else {
                    return Err(BAD_TYPE);
                };
                if expected < 0 || index.epoch.0 != expected as u32 {
                    return Err(FaultCode::CollectionModified);
                }
                let cursor = usize::try_from(cursor).map_err(|_| FaultCode::IndexOutOfBounds)?;
                let next = entries
                    .get(cursor..)
                    .and_then(|tail| tail.iter().position(MapEntry::is_live))
                    .map(|offset| cursor + offset)
                    .map_or(-1, |position| position as i64);
                self.push(Value::Int(next))?;
            }
            CollectionIterationOp::MapEntry { value } => {
                let index = self.pop_int()?;
                let r = self.pop_obj()?;
                let entry = match self.vm.heap.get(r) {
                    Object::Map { entries, .. } if index >= 0 => entries
                        .get(index as usize)
                        .filter(|entry| entry.is_live())
                        .copied(),
                    Object::Map { .. } => None,
                    _ => return Err(BAD_TYPE),
                }
                .ok_or(FaultCode::IndexOutOfBounds)?;
                let value = if value { entry.value } else { entry.key };
                self.push(value)?;
            }
        }
        Ok(())
    }

    /// Execute one extended collection operation outside the hot dispatch body.
    #[inline(never)]
    pub(super) fn exec_collection_extension(
        &mut self,
        module: &NamespaceRuntime,
        envs: &mut TypeEnvs,
        op: CollectionExtensionOp,
    ) -> Result<(), FaultCode> {
        match op {
            CollectionExtensionOp::ListCapacity => {
                let r = self.pop_obj()?;
                let capacity = match self.vm.heap.get(r) {
                    Object::List { items, .. } => items.capacity(),
                    _ => return Err(BAD_TYPE),
                };
                let capacity = i64::try_from(capacity).map_err(|_| FaultCode::HeapLimit)?;
                self.push(Value::Int(capacity))?;
            }
            CollectionExtensionOp::ListSet => {
                let value = self.pop()?;
                let index = self.pop_int()?;
                let r = self.pop_obj()?;
                self.frozen_guard(r)?;
                let item = match self.vm.heap.get_mut(r) {
                    Object::List { items, .. } if index >= 0 => items.get_mut(index as usize),
                    Object::List { .. } => None,
                    _ => return Err(BAD_TYPE),
                }
                .ok_or(FaultCode::IndexOutOfBounds)?;
                *item = value;
                self.push(Value::Unit)?;
            }
            CollectionExtensionOp::ListPop(ty) => {
                let r = self.pop_obj()?;
                self.frozen_guard(r)?;
                let value = match self.vm.heap.get_mut(r) {
                    Object::List { items, epoch } if !items.is_empty() => {
                        epoch.bump()?;
                        items.pop()
                    }
                    Object::List { .. } => None,
                    _ => return Err(BAD_TYPE),
                };
                if value.is_some() {
                    self.vm.heap.recharge(r);
                }
                match value {
                    Some(value) => self.push(value)?,
                    None => {
                        let ty = self.close_option_family(module, envs, ty)?;
                        self.push(Value::EmptyCase { ty, arm: 1 })?;
                    }
                }
            }
            CollectionExtensionOp::ListInsert => {
                let value = self.pop()?;
                let index = self.pop_int()?;
                let r = self.pop_obj()?;
                self.frozen_guard(r)?;
                let length = match self.vm.heap.get(r) {
                    Object::List { items, .. } => items.len(),
                    _ => return Err(BAD_TYPE),
                };
                if index < 0 || index as usize > length {
                    return Err(FaultCode::IndexOutOfBounds);
                }
                self.reserve(16, &[Value::Obj(r), value])?;
                match self.vm.heap.get_mut(r) {
                    Object::List { items, epoch } => {
                        epoch.bump()?;
                        items.insert(index as usize, value);
                    }
                    _ => return Err(BAD_TYPE),
                }
                self.vm.heap.recharge(r);
                self.push(Value::Unit)?;
            }
            CollectionExtensionOp::ListRemove { swap } => {
                let index = self.pop_int()?;
                let r = self.pop_obj()?;
                self.frozen_guard(r)?;
                let value = match self.vm.heap.get_mut(r) {
                    Object::List { items, epoch }
                        if index >= 0 && (index as usize) < items.len() =>
                    {
                        epoch.bump()?;
                        if swap {
                            items.swap_remove(index as usize)
                        } else {
                            items.remove(index as usize)
                        }
                    }
                    Object::List { .. } => return Err(FaultCode::IndexOutOfBounds),
                    _ => return Err(BAD_TYPE),
                };
                self.vm.heap.recharge(r);
                self.push(value)?;
            }
            CollectionExtensionOp::ListReserve => {
                let additional = self.pop_int()?;
                let r = self.pop_obj()?;
                self.frozen_guard(r)?;
                let additional =
                    usize::try_from(additional).map_err(|_| FaultCode::IndexOutOfBounds)?;
                let growth = additional.checked_mul(16).ok_or(FaultCode::HeapLimit)?;
                if let Object::List { items, epoch } = self.vm.heap.get(r) {
                    if additional > items.capacity().saturating_sub(items.len()) {
                        epoch.ensure_bumpable()?;
                    }
                } else {
                    return Err(BAD_TYPE);
                }
                self.reserve(growth, &[Value::Obj(r)])?;
                let changed = match self.vm.heap.get_mut(r) {
                    Object::List { items, .. } => {
                        let before = items.capacity();
                        items
                            .try_reserve(additional)
                            .map_err(|_| FaultCode::HeapLimit)?;
                        items.capacity() != before
                    }
                    _ => return Err(BAD_TYPE),
                };
                if changed {
                    match self.vm.heap.get_mut(r) {
                        Object::List { epoch, .. } => epoch.bump()?,
                        _ => return Err(BAD_TYPE),
                    }
                }
                self.push(Value::Unit)?;
            }
            CollectionExtensionOp::ListTruncate => {
                let length = self.pop_int()?;
                let r = self.pop_obj()?;
                self.frozen_guard(r)?;
                let length = usize::try_from(length).map_err(|_| FaultCode::IndexOutOfBounds)?;
                let changed = match self.vm.heap.get_mut(r) {
                    Object::List { items, epoch } if length < items.len() => {
                        epoch.bump()?;
                        items.truncate(length);
                        true
                    }
                    Object::List { .. } => false,
                    _ => return Err(BAD_TYPE),
                };
                if changed {
                    self.vm.heap.recharge(r);
                }
                self.push(Value::Unit)?;
            }
            CollectionExtensionOp::ListContains => {
                let needle = self.pop()?;
                let r = self.pop_obj()?;
                let items = match self.vm.heap.get(r) {
                    Object::List { items, .. } => items,
                    _ => return Err(BAD_TYPE),
                };
                let mut found = false;
                for item in items {
                    if self.values_equal(module, *item, needle)? {
                        found = true;
                        break;
                    }
                }
                self.push(Value::Bool(found))?;
            }
            CollectionExtensionOp::ListReorder => {
                let r = self.pop_obj()?;
                self.frozen_guard(r)?;
                match self.vm.heap.get_mut(r) {
                    Object::List { epoch, .. } => epoch.bump()?,
                    _ => return Err(BAD_TYPE),
                }
                self.push(Value::Unit)?;
            }
            CollectionExtensionOp::MapRemove(ty) => {
                let key = self.pop()?;
                let r = self.pop_obj()?;
                self.frozen_guard(r)?;
                let position = self.map_lookup(r, key)?;
                let value = match position {
                    Some(position) => Some(self.remove_map_entry(r, position)?),
                    None => None,
                };
                match value {
                    Some(value) => self.push(value)?,
                    None => {
                        let ty = self.close_option_family(module, envs, ty)?;
                        self.push(Value::EmptyCase { ty, arm: 1 })?;
                    }
                }
            }
            CollectionExtensionOp::MapClear => {
                let r = self.pop_obj()?;
                self.frozen_guard(r)?;
                let changed = match self.vm.heap.get_mut(r) {
                    Object::Map { entries, index } if index.live_len() > 0 => {
                        index.epoch.bump()?;
                        entries.clear();
                        index.reset();
                        true
                    }
                    Object::Map { .. } => false,
                    _ => return Err(BAD_TYPE),
                };
                if changed {
                    self.vm.heap.recharge(r);
                }
                self.push(Value::Unit)?;
            }
            CollectionExtensionOp::MapReserve => {
                let additional = self.pop_int()?;
                let r = self.pop_obj()?;
                self.frozen_guard(r)?;
                let additional =
                    usize::try_from(additional).map_err(|_| FaultCode::IndexOutOfBounds)?;
                let growth = additional.checked_mul(40).ok_or(FaultCode::HeapLimit)?;
                if let Object::Map { entries, index, .. } = self.vm.heap.get(r) {
                    if additional > entries.capacity().saturating_sub(entries.len()) {
                        index.epoch.ensure_bumpable()?;
                    }
                } else {
                    return Err(BAD_TYPE);
                }
                self.reserve(growth, &[Value::Obj(r)])?;
                let changed = match self.vm.heap.get_mut(r) {
                    Object::Map { entries, .. } => {
                        let before = entries.capacity();
                        entries
                            .try_reserve(additional)
                            .map_err(|_| FaultCode::HeapLimit)?;
                        entries.capacity() != before
                    }
                    _ => return Err(BAD_TYPE),
                };
                if changed {
                    match self.vm.heap.get_mut(r) {
                        Object::Map { index, .. } => index.epoch.bump()?,
                        _ => return Err(BAD_TYPE),
                    }
                }
                self.push(Value::Unit)?;
            }
        }
        Ok(())
    }
}
