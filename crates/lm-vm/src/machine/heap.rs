//! Roots, allocation, maps, and structural value operations.

use super::*;

/// One String-map query that can borrow visible text content.
#[derive(Clone, Copy)]
pub(crate) enum BorrowedStringKey<'a> {
    /// Read visible text from one live guest value.
    Value(Value),
    /// Read visible text from runtime-owned metadata.
    Text(&'a SharedText),
}

impl BorrowedStringKey<'_> {
    /// Get the stable semantic hash for this query.
    pub(crate) fn semantic_hash(self, machine: &Machine) -> Result<i64, FaultCode> {
        match self {
            Self::Value(Value::Obj(reference)) => machine
                .vm
                .heap
                .text(reference)
                .map(|text| text.semantic_hash() as i64)
                .ok_or(BAD_TYPE),
            Self::Value(_) => Err(BAD_TYPE),
            Self::Text(text) => Ok(text.semantic_hash() as i64),
        }
    }

    /// Get the private lookup hash for this query.
    fn index_hash(self, machine: &Machine) -> Result<u64, FaultCode> {
        match self {
            Self::Value(Value::Obj(reference)) => machine
                .vm
                .heap
                .text(reference)
                .map(|text| text.lookup_hash())
                .ok_or(BAD_TYPE),
            Self::Value(_) => Err(BAD_TYPE),
            Self::Text(text) => Ok(text.lookup_hash()),
        }
    }

    /// Compare this query with one stored String key.
    fn matches(self, machine: &Machine, stored: Value) -> Result<bool, FaultCode> {
        let Value::Obj(stored) = stored else {
            return Err(BAD_TYPE);
        };
        let Some(Object::Str(stored)) = machine.vm.heap.try_get(stored) else {
            return Err(BAD_TYPE);
        };
        match self {
            Self::Value(Value::Obj(reference)) => machine
                .vm
                .heap
                .text(reference)
                .map(|text| stored.as_str() == text.as_str())
                .ok_or(BAD_TYPE),
            Self::Value(_) => Err(BAD_TYPE),
            Self::Text(text) => Ok(stored == text),
        }
    }

    /// Prepare one owned String object after a lookup miss.
    pub(crate) fn owned_object(self, machine: &Machine) -> Result<Option<Object>, FaultCode> {
        let text = match self {
            Self::Value(Value::Obj(reference)) => {
                if matches!(machine.vm.heap.try_get(reference), Some(Object::Str(_))) {
                    return Ok(None);
                }
                machine.vm.heap.text(reference).ok_or(BAD_TYPE)?
            }
            Self::Value(_) => return Err(BAD_TYPE),
            Self::Text(text) => text.text_ref(),
        };
        text.try_bounded()
            .map(Object::Str)
            .map(Some)
            .map_err(|_| FaultCode::HeapLimit)
    }
}

impl Machine {
    /// Collect garbage now. `extra` holds additional roots that are
    /// not yet stored in the arenas.
    pub fn collect_garbage(&mut self, extra: &[ObjRef]) {
        let roots = self.gc_roots(extra);
        lm_graph::collect(&mut self.vm.heap, roots);
        self.execution_metrics.collections = self.execution_metrics.collections.saturating_add(1);
    }

    /// Every collection root this machine holds outside its heap.
    ///
    /// A boundary transfer into this machine reads the list before it
    /// borrows the heap, because a destination collection during the
    /// copy needs the same roots.
    pub fn gc_roots(&self, extra: &[ObjRef]) -> Vec<ObjRef> {
        let mut roots: Vec<ObjRef> = Vec::new();
        if let Some(continuation) = &self.native_continuation {
            continuation.extend_gc_roots(&mut roots);
        }
        for value in self.vm.locals.iter().chain(self.vm.operands.iter()) {
            if let Value::Obj(r) = value {
                roots.push(*r);
            }
        }
        self.extend_non_arena_gc_roots(&mut roots);
        roots.extend_from_slice(extra);
        roots
    }

    /// Build roots while native code owns the active frame arenas.
    pub(super) fn native_gc_roots(
        &self,
        base_local: usize,
        base_operand: usize,
        active: &[ObjRef],
    ) -> Vec<ObjRef> {
        let mut roots = Vec::new();
        for value in self.vm.locals[..base_local]
            .iter()
            .chain(self.vm.operands[..base_operand].iter())
        {
            if let Value::Obj(reference) = value {
                roots.push(*reference);
            }
        }
        roots.extend_from_slice(active);
        self.extend_non_arena_gc_roots(&mut roots);
        roots
    }

    pub(super) fn extend_non_arena_gc_roots(&self, roots: &mut Vec<ObjRef>) {
        for frame in &self.vm.frames {
            if let Some(FrameCapture::Closure(reference)) = frame.closure {
                roots.push(reference);
            }
        }
        for slot in &self.callbacks {
            if let Some(descriptor) = &slot.descriptor {
                for value in &descriptor.captures {
                    if let Value::Obj(reference) = value {
                        roots.push(*reference);
                    }
                }
            }
        }
        if let Some(pending) = &self.vm.pending {
            for value in &pending.args {
                if let Value::Obj(r) = value {
                    roots.push(*r);
                }
            }
        }
        for entry in self.vm.waits.values() {
            if let WaitSource::Operation {
                ready: Some(Value::Obj(reference)),
                ..
            } = &entry.source
            {
                roots.push(*reference);
            }
        }
        if let Some(Terminal::Done(Value::Obj(r))) = &self.vm.terminal {
            roots.push(*r);
        }
        // An accepted message lives in this machine's heap until
        // `receive` delivers it, so the queue is a collection root.
        for value in &self.vm.mailbox.queue {
            if let Value::Obj(r) = value {
                roots.push(*r);
            }
        }
        for action in self.table.actions() {
            if let Action::Mock(reference) = action {
                roots.push(reference);
            }
        }
        // The proc body waits for the constructor frame to return.
        if let Some(r) = self.start_body {
            roots.push(r);
        }
        if let Some(reference) = self.pending_regex_compile {
            roots.push(reference);
        }
        if let Some(pending) = self.pending_decompression {
            roots.push(pending.input);
            roots.push(pending.output);
        }
        // Interned literals stay alive for the machine lifetime.
        roots.extend(self.vm.literals.iter().filter_map(|value| value.as_obj()));
    }

    /// The canonical snapshot roots of this machine, in canonical
    /// order.
    ///
    /// The order is the one declaration point of snapshot
    /// reachability: frame closures, locals, operands, pending
    /// arguments, the terminal value, the mailbox queue, the proc
    /// body, and the interned literals.
    ///
    /// The list is the collection roots minus the policy-table
    /// entries. Specification 17.2 excludes policy tables from a
    /// snapshot, so a machine or an object that only a table-held mock
    /// closure names is not snapshot content. `machine_references` and
    /// `snapshot_preflight` read this list, so the closed set and the
    /// encoder agree on what the world holds.
    pub fn snapshot_roots(&self) -> Vec<ObjRef> {
        let mut roots: Vec<ObjRef> = Vec::new();
        for frame in &self.vm.frames {
            if let Some(FrameCapture::Closure(reference)) = frame.closure {
                roots.push(reference);
            }
        }
        for slot in &self.callbacks {
            if let Some(descriptor) = &slot.descriptor {
                for value in &descriptor.captures {
                    if let Value::Obj(reference) = value {
                        roots.push(*reference);
                    }
                }
            }
        }
        for value in self.vm.locals.iter().chain(self.vm.operands.iter()) {
            if let Value::Obj(r) = value {
                roots.push(*r);
            }
        }
        if let Some(pending) = &self.vm.pending {
            for value in &pending.args {
                if let Value::Obj(r) = value {
                    roots.push(*r);
                }
            }
        }
        if let Some(Terminal::Done(Value::Obj(r))) = &self.vm.terminal {
            roots.push(*r);
        }
        for value in &self.vm.mailbox.queue {
            if let Value::Obj(r) = value {
                roots.push(*r);
            }
        }
        if let Some(r) = self.start_body {
            roots.push(r);
        }
        roots.extend(self.vm.literals.iter().filter_map(|value| value.as_obj()));
        roots
    }

    /// Return active callbacks in canonical root order.
    pub fn snapshot_callbacks(&self) -> Vec<CallbackRef> {
        let mut callbacks = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let push = |value: Value,
                    callbacks: &mut Vec<CallbackRef>,
                    seen: &mut std::collections::HashSet<CallbackRef>| {
            if let Value::Callback(reference) = value {
                if seen.insert(reference) {
                    callbacks.push(reference);
                }
            }
        };
        for frame in &self.vm.frames {
            if let Some(FrameCapture::Callback(reference)) = frame.closure {
                push(Value::Callback(reference), &mut callbacks, &mut seen);
            }
        }
        for value in self.vm.locals.iter().chain(self.vm.operands.iter()) {
            push(*value, &mut callbacks, &mut seen);
        }
        if let Some(pending) = &self.vm.pending {
            for value in &pending.args {
                push(*value, &mut callbacks, &mut seen);
            }
        }
        if let Some(Terminal::Done(value)) = &self.vm.terminal {
            push(*value, &mut callbacks, &mut seen);
        }
        for value in &self.vm.mailbox.queue {
            push(*value, &mut callbacks, &mut seen);
        }
        let mut cursor = 0;
        while cursor < callbacks.len() {
            let reference = callbacks[cursor];
            cursor += 1;
            if let Ok(descriptor) = self.callback(reference) {
                for value in &descriptor.captures {
                    push(*value, &mut callbacks, &mut seen);
                }
            }
        }
        callbacks
    }

    /// Allocate one object. When the cap would be exceeded, collect
    /// first. The children of the new object are roots during the
    /// collection because they are not yet reachable from the arenas.
    pub fn alloc(&mut self, object: Object) -> Result<Value, FaultCode> {
        let mut cost = self.vm.heap.allocation_cost(&object);
        if self.vm.heap.collection_due(cost) {
            let mut extra = Vec::new();
            object.children(&mut extra);
            self.collect_garbage(&extra);
            cost = self.vm.heap.allocation_cost(&object);
            if self.vm.heap.would_exceed(cost) {
                return Err(FaultCode::HeapLimit);
            }
        }
        Ok(Value::Obj(self.vm.heap.alloc(object)))
    }

    /// Allocate while native code owns the active frame arenas.
    pub(crate) fn alloc_native(
        &mut self,
        object: Object,
        base_local: usize,
        base_operand: usize,
        active_roots: &[ObjRef],
    ) -> Result<Value, FaultCode> {
        let mut cost = self.vm.heap.allocation_cost(&object);
        if self.vm.heap.collection_due(cost) {
            let mut roots = self.native_gc_roots(base_local, base_operand, active_roots);
            object.children(&mut roots);
            lm_graph::collect(&mut self.vm.heap, roots);
            self.execution_metrics.collections =
                self.execution_metrics.collections.saturating_add(1);
            cost = self.vm.heap.allocation_cost(&object);
            if self.vm.heap.would_exceed(cost) {
                return Err(FaultCode::HeapLimit);
            }
        }
        Ok(Value::Obj(self.vm.heap.alloc(object)))
    }

    /// Reserve growth while native code owns the active frame arenas.
    pub(crate) fn reserve_native(
        &mut self,
        delta: usize,
        base_local: usize,
        base_operand: usize,
        active_roots: &[ObjRef],
    ) -> Result<(), FaultCode> {
        if self.vm.heap.collection_due(delta) {
            let roots = self.native_gc_roots(base_local, base_operand, active_roots);
            lm_graph::collect(&mut self.vm.heap, roots);
            self.execution_metrics.collections =
                self.execution_metrics.collections.saturating_add(1);
            if self.vm.heap.would_exceed_growth(delta) {
                return Err(FaultCode::HeapLimit);
            }
        }
        Ok(())
    }

    /// Make room for `delta` more bytes of growth on an existing
    /// object. `temps` holds values already popped from the arenas.
    pub(super) fn reserve(&mut self, delta: usize, temps: &[Value]) -> Result<(), FaultCode> {
        if self.vm.heap.collection_due(delta) {
            let extra: Vec<ObjRef> = temps.iter().filter_map(|v| v.as_obj()).collect();
            self.collect_garbage(&extra);
            if self.vm.heap.would_exceed_growth(delta) {
                return Err(FaultCode::HeapLimit);
            }
        }
        Ok(())
    }

    /// Compare two map keys. Scalars compare by value; strings by
    /// content.
    pub(crate) fn key_eq(&self, a: Value, b: Value) -> bool {
        match (a, b) {
            (Value::Int(x), Value::Int(y)) => x == y,
            (Value::Float(x), Value::Float(y)) => float_eq(x, y),
            (Value::Bool(x), Value::Bool(y)) => x == y,
            (Value::Char(x), Value::Char(y)) => x == y,
            (Value::Obj(x), Value::Obj(y)) => {
                if x == y {
                    return true;
                }
                if let (Some(left), Some(right)) = (self.vm.heap.text(x), self.vm.heap.text(y)) {
                    return left == right;
                }
                if self.vm.heap.is_compact_text(x) || self.vm.heap.is_compact_text(y) {
                    return false;
                }
                match (self.vm.heap.get(x), self.vm.heap.get(y)) {
                    (Object::Bytes(b1), Object::Bytes(b2)) => b1 == b2,
                    _ => false,
                }
            }
            _ => false,
        }
    }

    /// Get the stable semantic hash of one native map key.
    pub(crate) fn key_semantic_hash(&self, key: Value) -> Result<i64, FaultCode> {
        match key {
            Value::Bool(value) => Ok(i64::from(value)),
            Value::Int(value) => Ok(value),
            Value::Float(bits) => Ok(float_hash(bits)),
            Value::Char(value) => Ok(i64::from(u32::from(value))),
            Value::Obj(r) => {
                if let Some(text) = self.vm.heap.text(r) {
                    return Ok(text.semantic_hash() as i64);
                }
                match self.vm.heap.get(r) {
                    Object::Bytes(bytes) => Ok(bytes.semantic_hash() as i64),
                    _ => Err(BAD_TYPE),
                }
            }
            _ => Err(BAD_TYPE),
        }
    }

    /// Mix one semantic hash with the private process hash key.
    pub(crate) fn map_index_hash(hash: i64) -> u64 {
        process_lookup_hash(hash)
    }

    /// Mix one integer with stable wrapping arithmetic.
    pub(super) fn stable_hash_mix(value: u64) -> u64 {
        let mut value = value;
        value ^= value >> 30;
        value = value.wrapping_mul(0xbf58_476d_1ce4_e5b9);
        value ^= value >> 27;
        value = value.wrapping_mul(0x94d0_49bb_1331_11eb);
        value ^ (value >> 31)
    }

    /// Get the private index hash of one native map key.
    pub(super) fn key_index_hash(&self, key: Value) -> Result<u64, FaultCode> {
        match key {
            Value::Bool(value) => Ok(Self::map_index_hash(i64::from(value))),
            Value::Int(value) => Ok(Self::map_index_hash(value)),
            Value::Float(bits) => Ok(Self::map_index_hash(float_hash(bits))),
            Value::Char(value) => Ok(Self::map_index_hash(i64::from(u32::from(value)))),
            Value::Obj(r) => {
                if let Some(text) = self.vm.heap.text(r) {
                    return Ok(text.lookup_hash());
                }
                match self.vm.heap.get(r) {
                    Object::Bytes(bytes) => Ok(bytes.lookup_hash()),
                    _ => Err(BAD_TYPE),
                }
            }
            _ => Err(BAD_TYPE),
        }
    }

    /// Extend one map index through every stored entry.
    pub(crate) fn ensure_map_index(&mut self, r: ObjRef) -> Result<(), FaultCode> {
        let (built, len) = match self.vm.heap.get(r) {
            Object::Map { entries, index } => (index.built as usize, entries.len()),
            _ => return Err(BAD_TYPE),
        };
        if built == len {
            return Ok(());
        }
        let mut mixed = Vec::with_capacity(len - built);
        for position in built..len {
            let entry = match self.vm.heap.get(r) {
                Object::Map { entries, .. } => entries[position],
                _ => return Err(BAD_TYPE),
            };
            mixed.push(
                entry
                    .is_live()
                    .then(|| Self::map_index_hash(entry.semantic_hash)),
            );
        }
        if let Object::Map { index, .. } = self.vm.heap.get_mut(r) {
            for (offset, hash) in mixed.into_iter().enumerate() {
                let position = (built + offset) as u32;
                match hash {
                    Some(hash) => index.insert(hash, position),
                    None => index.skip_tombstone(position),
                }
            }
            Ok(())
        } else {
            Err(BAD_TYPE)
        }
    }

    /// Find the entry position of a key in the map object `r` through
    /// the hash index. The index is a cache: the call first indexes
    /// the entries appended since the last lookup.
    pub(crate) fn map_lookup(&mut self, r: ObjRef, key: Value) -> Result<Option<usize>, FaultCode> {
        self.ensure_map_index(r)?;
        let hash = self.key_index_hash(key)?;
        self.map_lookup_indexed(r, hash, |machine, stored| Ok(machine.key_eq(stored, key)))
            .map(|found| found.map(|(position, _)| position))
    }

    /// Find one String entry through a borrowed text query.
    pub(crate) fn map_lookup_borrowed_string(
        &mut self,
        r: ObjRef,
        key: BorrowedStringKey<'_>,
    ) -> Result<Option<(usize, Value)>, FaultCode> {
        self.ensure_map_index(r)?;
        let hash = key.index_hash(self)?;
        self.map_lookup_indexed(r, hash, |machine, stored| key.matches(machine, stored))
    }

    /// Search one prepared map-index bucket.
    fn map_lookup_indexed(
        &self,
        r: ObjRef,
        hash: u64,
        mut matches: impl FnMut(&Self, Value) -> Result<bool, FaultCode>,
    ) -> Result<Option<(usize, Value)>, FaultCode> {
        let (entries, candidates, dense) = match self.vm.heap.get(r) {
            Object::Map { entries, index, .. } => (
                entries,
                index.candidates(hash),
                index.live_len() == entries.len(),
            ),
            _ => return Err(FaultCode::TypeMismatch),
        };
        if dense {
            for i in candidates {
                let Some(entry) = entries.get(i as usize) else {
                    continue;
                };
                if matches(self, entry.key)? {
                    return Ok(Some((i as usize, entry.key)));
                }
            }
            return Ok(None);
        }
        for i in candidates {
            let k = match entries.get(i as usize) {
                Some(entry) if entry.is_live() => entry.key,
                None => continue,
                Some(_) => continue,
            };
            if matches(self, k)? {
                return Ok(Some((i as usize, k)));
            }
        }
        Ok(None)
    }

    /// Resolve one live probe token to its map entry.
    pub(crate) fn map_token_entry(
        &self,
        r: ObjRef,
        token: i64,
    ) -> Result<Option<usize>, FaultCode> {
        let (epoch, slot) = map_probe_parts(token)?;
        let Object::Map { entries, index, .. } = self.vm.heap.get(r) else {
            return Err(BAD_TYPE);
        };
        if index.epoch.0 != epoch {
            return Err(FaultCode::CollectionModified);
        }
        let Some(slot) = slot else {
            return Ok(None);
        };
        let entry = index.entry_at(slot).ok_or(BAD_STATE)? as usize;
        if entry >= entries.len() || !entries[entry].is_live() {
            return Err(BAD_STATE);
        }
        Ok(Some(entry))
    }

    /// Remove one map entry and compact excess tombstones.
    pub(crate) fn remove_map_entry(&mut self, r: ObjRef, entry: usize) -> Result<Value, FaultCode> {
        let value = match self.vm.heap.get_mut(r) {
            Object::Map { entries, index } => {
                index.epoch.bump()?;
                let value = entries.get_mut(entry).ok_or(BAD_STATE)?.remove();
                index.record_removal();
                if index.needs_compaction(entries.len()) {
                    entries.retain(MapEntry::is_live);
                    index.record_compaction();
                }
                value
            }
            _ => return Err(BAD_TYPE),
        };
        self.vm.heap.recharge(r);
        Ok(value)
    }

    /// Execute one verified interface-backed map operation.
    pub(super) fn exec_hashable_map_instr(
        &mut self,
        instr: ExtendedInstr,
    ) -> Result<(), FaultCode> {
        match instr {
            ExtendedInstr::MapProbe => {
                let prior = self.pop_int()?;
                let semantic = self.pop_int()?;
                let r = self.pop_obj()?;
                self.ensure_map_index(r)?;
                let (epoch, mut prior_slot) = if prior == 0 {
                    let epoch = match self.vm.heap.get_mut(r) {
                        Object::Map { index, .. } => index.epoch.observe(),
                        _ => return Err(BAD_TYPE),
                    };
                    (epoch, None)
                } else {
                    let (epoch, slot) = map_probe_parts(prior)?;
                    let current = match self.vm.heap.get(r) {
                        Object::Map { index, .. } => index.epoch.0,
                        _ => return Err(BAD_TYPE),
                    };
                    if current != epoch {
                        return Err(FaultCode::CollectionModified);
                    }
                    if slot.is_none() {
                        self.push(Value::Int(prior))?;
                        return Ok(());
                    }
                    (epoch, slot)
                };
                let hash = Self::map_index_hash(semantic);
                let slot = loop {
                    let found = match self.vm.heap.get(r) {
                        Object::Map { index, .. } => index.probe(hash, prior_slot),
                        _ => return Err(BAD_TYPE),
                    };
                    let Some((slot, entry)) = found else {
                        break None;
                    };
                    let live = match self.vm.heap.get(r) {
                        Object::Map { entries, .. } => {
                            entries.get(entry as usize).is_some_and(MapEntry::is_live)
                        }
                        _ => return Err(BAD_TYPE),
                    };
                    if live {
                        break Some(slot);
                    }
                    prior_slot = Some(slot);
                };
                self.push(Value::Int(map_probe_token(epoch, slot)?))?;
            }
            ExtendedInstr::MapProbeFound => {
                let token = self.pop_int()?;
                let (_, slot) = map_probe_parts(token)?;
                self.push(Value::Bool(slot.is_some()))?;
            }
            ExtendedInstr::MapProbeKey | ExtendedInstr::MapProbeValue => {
                let token = self.pop_int()?;
                let r = self.pop_obj()?;
                let entry = self
                    .map_token_entry(r, token)?
                    .ok_or(FaultCode::MissingKey)?;
                let value = match self.vm.heap.get(r) {
                    Object::Map { entries, .. } => {
                        let pair = entries.get(entry).ok_or(BAD_STATE)?;
                        if matches!(instr, ExtendedInstr::MapProbeKey) {
                            pair.key
                        } else {
                            pair.value
                        }
                    }
                    _ => return Err(BAD_TYPE),
                };
                self.push(value)?;
            }
            ExtendedInstr::MapProbeSetValue => {
                let value = self.pop()?;
                let token = self.pop_int()?;
                let r = self.pop_obj()?;
                self.frozen_guard(r)?;
                let entry = self.map_token_entry(r, token)?.ok_or(BAD_STATE)?;
                match self.vm.heap.get_mut(r) {
                    Object::Map { entries, .. } => {
                        entries.get_mut(entry).ok_or(BAD_STATE)?.value = value;
                    }
                    _ => return Err(BAD_TYPE),
                }
                self.push(Value::Unit)?;
            }
            ExtendedInstr::MapProbeRemove => {
                let token = self.pop_int()?;
                let r = self.pop_obj()?;
                self.frozen_guard(r)?;
                let entry = self.map_token_entry(r, token)?.ok_or(BAD_STATE)?;
                let value = self.remove_map_entry(r, entry)?;
                self.push(value)?;
            }
            ExtendedInstr::MapInsertHashed => {
                let token = self.pop_int()?;
                let semantic = self.pop_int()?;
                let value = self.pop()?;
                let key = self.pop()?;
                let r = self.pop_obj()?;
                self.frozen_guard(r)?;
                if self.map_token_entry(r, token)?.is_some() {
                    return Err(BAD_STATE);
                }
                if let Value::Obj(key) = key {
                    lm_graph::verify_frozen(&mut self.vm.heap, key, &self.config.graph).map_err(
                        |fault| match fault {
                            FaultCode::UnsendableValue => FaultCode::MutableMapKey,
                            other => other,
                        },
                    )?;
                }
                self.reserve(40, &[Value::Obj(r), key, value])?;
                match self.vm.heap.get_mut(r) {
                    Object::Map { entries, index } => {
                        index.epoch.bump()?;
                        let entry = entries.len() as u32;
                        entries.push(MapEntry {
                            key,
                            value,
                            semantic_hash: semantic,
                        });
                        index.push_live(Self::map_index_hash(semantic), entry);
                    }
                    _ => return Err(BAD_TYPE),
                }
                self.vm.heap.recharge(r);
                self.push(Value::Unit)?;
            }
            ExtendedInstr::MapWriteGuard => {
                let r = self.pop_obj()?;
                self.frozen_guard(r)?;
                self.push(Value::Unit)?;
            }
            _ => unreachable!("the hashable map dispatcher receives one map instruction"),
        }
        Ok(())
    }

    pub(super) fn frozen_guard(&self, r: ObjRef) -> Result<(), FaultCode> {
        if self.vm.heap.is_frozen(r) {
            Err(FaultCode::FrozenWrite)
        } else {
            Ok(())
        }
    }

    /// Allocate one syntax view with shared immutable backing.
    pub(super) fn alloc_syntax_view(
        &mut self,
        class: u32,
        source: Value,
        records: Value,
        index: u32,
    ) -> Result<Value, FaultCode> {
        let value = self.alloc(Object::Instance {
            class,
            fields: vec![source, records, Value::Int(i64::from(index))].into(),
            env: Witness::EMPTY,
        })?;
        let reference = value.as_obj().ok_or(FaultCode::MalformedState)?;
        self.vm.heap.set_frozen(reference);
        Ok(value)
    }

    /// Allocate one frozen syntax tree with immutable backing.
    pub(super) fn alloc_syntax_tree(
        &mut self,
        class: u32,
        source: Value,
        records: Value,
    ) -> Result<Value, FaultCode> {
        let value = self.alloc(Object::Instance {
            class,
            fields: vec![source, records].into(),
            env: Witness::EMPTY,
        })?;
        let reference = value.as_obj().ok_or(FaultCode::MalformedState)?;
        self.vm.heap.set_frozen(reference);
        Ok(value)
    }

    /// Allocate one verified definition record.
    pub(super) fn alloc_definition_spec(
        &mut self,
        module: &NamespaceRuntime,
        code: &lm_heap::PortableCode,
        decoded: &CompiledModulePayload,
        module_identity: &lm_bytecode::identity::ModuleIdentity,
    ) -> Result<Value, FaultCode> {
        let info = portable_definition_info_payload(code, decoded, module_identity)?;
        let selected = portable_definition_index(code, decoded)?;
        let identity_class = module.core_roles[lm_bytecode::corepin::ROLE_DEFINITION_IDENTITY];
        let spec_class = module.core_roles[lm_bytecode::corepin::ROLE_DEFINITION_SPEC];
        if identity_class == lm_bytecode::NO_ROLE || spec_class == lm_bytecode::NO_ROLE {
            return Err(BAD_STATE);
        }

        let root = self.vm.operands.len();
        let module_name =
            SharedText::try_from_string(info.module_name).map_err(|_| FaultCode::HeapLimit)?;
        let module_name = self.alloc(Object::Str(module_name))?;
        self.push(module_name)?;
        let qualified_key =
            SharedText::try_from_string(info.qualified_key).map_err(|_| FaultCode::HeapLimit)?;
        let qualified_key = self.alloc(Object::Str(qualified_key))?;
        self.push(qualified_key)?;
        let contract_hash = self.alloc(Object::NativeDigest(info.hashes.contract))?;
        self.push(contract_hash)?;
        let implementation_hash = self.alloc(Object::NativeDigest(info.hashes.implementation))?;
        self.push(implementation_hash)?;
        let definition_identity = self.alloc(Object::Instance {
            class: identity_class,
            fields: vec![
                module_name,
                qualified_key,
                contract_hash,
                implementation_hash,
            ]
            .into(),
            env: Witness::EMPTY,
        })?;
        let reference = definition_identity.as_obj().ok_or(BAD_STATE)?;
        self.vm.heap.set_frozen(reference);
        self.push(definition_identity)?;

        let module_hash = self.alloc(Object::NativeDigest(module_identity.semantic_hash))?;
        self.push(module_hash)?;
        let mut slot_values = Vec::new();
        slot_values
            .try_reserve_exact(decoded.slots.len())
            .map_err(|_| FaultCode::HeapLimit)?;
        for (index, slot) in decoded.slots.iter().enumerate() {
            let matches = match (code.kind, slot.initial) {
                (
                    lm_heap::PortableCodeKind::Function,
                    Some(lm_bytecode::SlotTarget::Function(target)),
                ) => target == selected,
                (
                    lm_heap::PortableCodeKind::Class,
                    Some(lm_bytecode::SlotTarget::Class { class, .. }),
                ) => class == selected,
                (
                    lm_heap::PortableCodeKind::Class,
                    Some(lm_bytecode::SlotTarget::Function(target)),
                ) => info.related_functions.contains(&target),
                _ => false,
            };
            if !matches {
                continue;
            }
            let index = u32::try_from(index).map_err(|_| BAD_STATE)?;
            let value = self.alloc(Object::NativeCode(Box::new(lm_heap::PortableCode {
                kind: lm_heap::PortableCodeKind::SlotSpec,
                bytes: code.bytes.clone(),
                slot: Some(index),
                origin: None,
            })))?;
            self.push(value)?;
            slot_values.push(value);
        }
        if slot_values.is_empty() {
            return Err(BAD_STATE);
        }
        let slots = self.alloc(Object::List {
            items: slot_values.into(),
            epoch: StructuralEpoch::default(),
        })?;
        let reference = slots.as_obj().ok_or(BAD_STATE)?;
        self.vm.heap.set_frozen(reference);
        self.push(slots)?;
        let value = self.alloc(Object::Instance {
            class: spec_class,
            fields: vec![definition_identity, module_hash, slots].into(),
            env: Witness::EMPTY,
        })?;
        let reference = value.as_obj().ok_or(BAD_STATE)?;
        self.vm.heap.set_frozen(reference);
        self.vm.operands.truncate(root);
        Ok(value)
    }

    /// Build one public source location for a compact fault coordinate.
    /// Resolve one trace site to a code origin, then allocate it.
    pub(super) fn alloc_fault_location(
        &mut self,
        module: &NamespaceRuntime,
        envs: &mut TypeEnvs,
        debug: &lm_bytecode::debug::DebugInfo,
        identity: &lm_bytecode::identity::ModuleIdentity,
        site: FaultSite,
    ) -> Result<Value, FaultCode> {
        let origin = code_origin(module, debug, identity, site)?;
        self.alloc_code_location(module, envs, origin)
    }

    /// Allocate one `CodeLocation` for a resolved origin.
    ///
    /// `module` supplies the core roles of this machine. The origin
    /// can come from the code of another machine.
    pub(crate) fn alloc_code_location(
        &mut self,
        module: &NamespaceRuntime,
        envs: &mut TypeEnvs,
        origin: CodeOrigin,
    ) -> Result<Value, FaultCode> {
        let range_class = module.core_roles[lm_bytecode::corepin::ROLE_SOURCE_RANGE];
        let location_class = module.core_roles[lm_bytecode::corepin::ROLE_CODE_LOCATION];
        if range_class == lm_bytecode::NO_ROLE || location_class == lm_bytecode::NO_ROLE {
            return Err(BAD_STATE);
        }
        let location_fields = &module
            .classes
            .get(location_class as usize)
            .ok_or(BAD_STATE)?
            .fields;
        if location_fields.len() != 4 {
            return Err(BAD_STATE);
        }
        let path_ty = location_fields[0].1;
        let range_ty = location_fields[1].1;

        let root = self.vm.operands.len();
        let (path, range) = match origin.source {
            Some((path, (lo, hi))) => {
                let path = SharedText::try_from_string(path).map_err(|_| FaultCode::HeapLimit)?;
                let path = self.alloc(Object::Str(path))?;
                self.push(path)?;
                let range = self.alloc(Object::Instance {
                    class: range_class,
                    fields: vec![Value::Int(i64::from(lo)), Value::Int(i64::from(hi))].into(),
                    env: Witness::EMPTY,
                })?;
                let range_ref = range.as_obj().ok_or(BAD_STATE)?;
                self.vm.heap.set_frozen(range_ref);
                self.push(range)?;
                (path, range)
            }
            None => {
                let path = Value::EmptyCase {
                    ty: self.close_option_family(module, envs, path_ty)?,
                    arm: 1,
                };
                let range = Value::EmptyCase {
                    ty: self.close_option_family(module, envs, range_ty)?,
                    arm: 1,
                };
                (path, range)
            }
        };
        let digest = self.alloc(Object::NativeDigest(origin.digest))?;
        self.push(digest)?;
        let location = self.alloc(Object::Instance {
            class: location_class,
            fields: vec![path, range, digest, Value::Int(origin.offset)].into(),
            env: Witness::EMPTY,
        })?;
        let location_ref = location.as_obj().ok_or(BAD_STATE)?;
        self.vm.heap.set_frozen(location_ref);
        self.vm.operands.truncate(root);
        Ok(location)
    }

    /// Test one value against a class type.
    pub(super) fn value_matches_class(
        &self,
        module: &NamespaceRuntime,
        envs: &mut TypeEnvs,
        value: Value,
        ty: u32,
    ) -> Result<bool, FaultCode> {
        let target = match module.types.get(ty as usize).ok_or(BAD_STATE)? {
            lm_bytecode::BcType::Class(class) | lm_bytecode::BcType::Inst(class, _) => *class,
            _ => return Err(BAD_STATE),
        };
        let option = module.core_roles[lm_bytecode::corepin::ROLE_OPTION];
        let some = module.core_roles[lm_bytecode::corepin::ROLE_OPTION_SOME];
        let none = module.core_roles[lm_bytecode::corepin::ROLE_OPTION_NONE];
        if target == option || target == some || target == none {
            let family = self.close_option_family(module, envs, ty)?;
            let is_none = matches!(
                value,
                Value::EmptyCase { ty, arm: 1 } if ty == family
            );
            return Ok(target == option || (target == none) == is_none);
        }
        let r = value.as_obj().ok_or(BAD_TYPE)?;
        self.instance_matches(module, r, ty)
    }

    /// Return true when the instance class equals or extends the target.
    pub(super) fn instance_matches(
        &self,
        module: &NamespaceRuntime,
        r: ObjRef,
        ty: u32,
    ) -> Result<bool, FaultCode> {
        let target = match module.types.get(ty as usize).ok_or(BAD_STATE)? {
            lm_bytecode::BcType::Class(c) | lm_bytecode::BcType::Inst(c, _) => *c,
            _ => return Err(BAD_STATE),
        };
        let mut class = self.virtual_class(module, Value::Obj(r))?;
        // The class chain of a verified module is acyclic, and the
        // step bound holds whatever built the state, so the walk never
        // spins on a hand-built table.
        for _ in 0..=module.classes.len() {
            if class == target {
                return Ok(true);
            }
            match module.classes.get(class as usize).and_then(|c| c.parent()) {
                Some(p) => class = p,
                None => return Ok(false),
            }
        }
        Err(BAD_STATE)
    }

    /// Create one machine-local callback descriptor.
    pub(super) fn alloc_callback(
        &mut self,
        func: u32,
        captures: Vec<Value>,
        env: TypeEnvId,
    ) -> Result<Value, FaultCode> {
        let owner_depth = u32::try_from(self.vm.frames.len()).map_err(|_| FaultCode::StackLimit)?;
        self.alloc_callback_native(func, captures, env, owner_depth)
    }

    /// Create one callback at an exact native frame depth.
    pub(crate) fn alloc_callback_native(
        &mut self,
        func: u32,
        captures: Vec<Value>,
        env: TypeEnvId,
        owner_depth: u32,
    ) -> Result<Value, FaultCode> {
        if owner_depth == 0 || owner_depth > self.config.max_frames {
            return Err(FaultCode::StackLimit);
        }
        let descriptor = CallbackDescriptor {
            func,
            captures,
            env,
            owner_depth,
        };
        if let Some((slot, entry)) = self
            .callbacks
            .iter_mut()
            .enumerate()
            .find(|(_, entry)| entry.descriptor.is_none())
        {
            entry.descriptor = Some(descriptor);
            return Ok(Value::Callback(CallbackRef {
                slot: slot as u32,
                generation: entry.generation,
            }));
        }
        self.callbacks
            .try_reserve(1)
            .map_err(|_| FaultCode::StackLimit)?;
        let slot = self.callbacks.len() as u32;
        self.callbacks.push(CallbackSlot {
            generation: 0,
            descriptor: Some(descriptor),
        });
        Ok(Value::Callback(CallbackRef {
            slot,
            generation: 0,
        }))
    }

    /// Release callbacks that cannot remain after one frame return.
    pub(super) fn collect_callbacks(&mut self) {
        let depth = self.vm.frames.len() as u32;
        if !self.callbacks.iter().any(|slot| {
            slot.descriptor
                .as_ref()
                .is_some_and(|descriptor| descriptor.owner_depth >= depth)
        }) {
            return;
        }
        let mut marked = vec![false; self.callbacks.len()];
        let mut work = Vec::new();
        let mut mark_value = |value: Value| {
            if let Value::Callback(reference) = value {
                work.push(reference);
            }
        };
        for value in self.vm.locals.iter().chain(self.vm.operands.iter()) {
            mark_value(*value);
        }
        for frame in &self.vm.frames {
            if let Some(FrameCapture::Callback(reference)) = frame.closure {
                mark_value(Value::Callback(reference));
            }
        }
        while let Some(reference) = work.pop() {
            let Some(slot) = self.callbacks.get(reference.slot as usize) else {
                continue;
            };
            if slot.generation != reference.generation
                || marked.get(reference.slot as usize).copied().unwrap_or(true)
            {
                continue;
            }
            marked[reference.slot as usize] = true;
            if let Some(descriptor) = &slot.descriptor {
                for value in &descriptor.captures {
                    if let Value::Callback(child) = value {
                        work.push(*child);
                    }
                }
            }
        }
        for (index, slot) in self.callbacks.iter_mut().enumerate() {
            let candidate = slot
                .descriptor
                .as_ref()
                .is_some_and(|descriptor| descriptor.owner_depth >= depth);
            if candidate && !marked[index] {
                slot.descriptor = None;
                slot.generation = slot.generation.wrapping_add(1);
            }
        }
    }

    /// Compare references under the function identity rule.
    /// Structural equality of two enum values (specification 6.4).
    ///
    /// Two values are equal when they hold the same case and every
    /// field pair is equal. A field takes the rule of its own form: a
    /// scalar, text, or bytes field compares by value, a nested enum
    /// or tuple field compares structurally, and every other object
    /// compares by reference.
    ///
    /// The walk keeps an explicit work stack. An enum value can nest
    /// as deeply as its construction, and a deep value must not grow
    /// the host stack.
    ///
    /// The body stays out of the dispatch loop. Every instruction
    /// pays for the size of that loop, and this comparison runs on
    /// one instruction alone.
    #[inline(never)]
    pub(crate) fn values_equal(
        &self,
        module: &NamespaceRuntime,
        a: Value,
        b: Value,
    ) -> Result<bool, FaultCode> {
        let mut work: Vec<(Value, Value)> = vec![(a, b)];
        while let Some((left, right)) = work.pop() {
            let equal = match (left, right) {
                (Value::Unit, Value::Unit) => true,
                (Value::Bool(x), Value::Bool(y)) => x == y,
                (Value::Int(x), Value::Int(y)) => x == y,
                (Value::Char(x), Value::Char(y)) => x == y,
                (Value::Op(x), Value::Op(y)) => x == y,
                (Value::EmptyCase { ty: xt, arm: xa }, Value::EmptyCase { ty: yt, arm: ya }) => {
                    xt == yt && xa == ya
                }
                (Value::Obj(x), Value::Obj(y)) => {
                    if x == y {
                        continue;
                    }
                    if let (Some(left), Some(right)) = (self.vm.heap.text(x), self.vm.heap.text(y))
                    {
                        left == right
                    } else if self.vm.heap.is_compact_text(x) || self.vm.heap.is_compact_text(y) {
                        false
                    } else {
                        match (self.vm.heap.get(x), self.vm.heap.get(y)) {
                            (Object::Bytes(s), Object::Bytes(t)) => s == t,
                            (Object::NativeDigest(s), Object::NativeDigest(t)) => s == t,
                            (
                                Object::Instance {
                                    class: ac,
                                    fields: af,
                                    ..
                                },
                                Object::Instance {
                                    class: bc,
                                    fields: bf,
                                    ..
                                },
                            ) => {
                                // An ordinary class keeps reference
                                // identity, so only an enum case walks.
                                let is_case = module
                                    .classes
                                    .get(*ac as usize)
                                    .map(|c| c.kind == lm_bytecode::BcClassKind::Case)
                                    .unwrap_or(false);
                                if !is_case || ac != bc || af.len() != bf.len() {
                                    false
                                } else {
                                    for (x, y) in af.iter().zip(bf.iter()) {
                                        if matches!(x, Value::Uninit) || matches!(y, Value::Uninit)
                                        {
                                            return Err(FaultCode::UninitializedField);
                                        }
                                        work.push((*x, *y));
                                    }
                                    continue;
                                }
                            }
                            (Object::Tuple { items: ai }, Object::Tuple { items: bi }) => {
                                if ai.len() != bi.len() {
                                    false
                                } else {
                                    for (x, y) in ai.iter().zip(bi.iter()) {
                                        work.push((*x, *y));
                                    }
                                    continue;
                                }
                            }
                            _ => self.references_equal(module, x, y),
                        }
                    }
                }
                _ => false,
            };
            if !equal {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub(super) fn references_equal(&self, module: &NamespaceRuntime, a: ObjRef, b: ObjRef) -> bool {
        if a == b {
            return true;
        }
        let (
            Object::Closure {
                func: a_func,
                captures: a_captures,
                env: a_env,
            },
            Object::Closure {
                func: b_func,
                captures: b_captures,
                env: b_env,
            },
        ) = (self.vm.heap.get(a), self.vm.heap.get(b))
        else {
            return false;
        };
        a_func == b_func
            && a_captures.is_empty()
            && b_captures.is_empty()
            && a_env.env() == TypeEnvId::EMPTY
            && b_env.env() == TypeEnvId::EMPTY
            && module
                .bindings
                .iter()
                .any(|binding| binding.func == *a_func)
    }
}
