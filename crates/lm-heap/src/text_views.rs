//! Compact canonical storage for immutable text views.

use crate::shared::{TextOwner, TextRef, TextView, TextViewBatch, TextViewBatchOwner};
use crate::OwnedArray;
use lm_value::{ObjRef, Value};

const PAGE_SLOTS: usize = 1024;
const DEAD_ROOT: u32 = u32::MAX;
const GENERATION_MASK: u32 = 0x7fff_ffff;

/// This generation bit selects the compact text-view table.
pub const TEXT_VIEW_GENERATION_TAG: u32 = 0x8000_0000;
/// Shift from a text-view slot to its page index.
pub const JIT_TEXT_VIEW_PAGE_SHIFT: u32 = 10;
/// Mask from a text-view slot to its page offset.
pub const JIT_TEXT_VIEW_PAGE_MASK: u32 = (PAGE_SLOTS as u32) - 1;

#[repr(C)]
struct TextViewEntry {
    generation: u32,
    root: u32,
    view: TextView,
}

impl TextViewEntry {
    fn dead(generation: u32) -> TextViewEntry {
        TextViewEntry {
            generation,
            root: DEAD_ROOT,
            view: TextView::empty(),
        }
    }

    fn is_live(&self) -> bool {
        self.root != DEAD_ROOT
    }

    fn replace(&mut self, root: u32, view: TextView) {
        debug_assert!(!self.is_live());
        self.root = root;
        self.view = view;
    }

    fn take(&mut self) -> Option<u32> {
        if !self.is_live() {
            return None;
        }
        let root = std::mem::replace(&mut self.root, DEAD_ROOT);
        self.view = TextView::empty();
        Some(root)
    }
}

struct TextViewPage {
    entries: Box<[TextViewEntry]>,
}

impl std::ops::Deref for TextViewPage {
    type Target = [TextViewEntry];

    fn deref(&self) -> &[TextViewEntry] {
        &self.entries
    }
}

impl std::ops::DerefMut for TextViewPage {
    fn deref_mut(&mut self) -> &mut [TextViewEntry] {
        &mut self.entries
    }
}

struct TextViewRoot {
    owner: TextOwner,
    references: usize,
}

/// Compact text-view table state.
#[derive(Default)]
pub(crate) struct TextViewTable {
    pages: Vec<TextViewPage>,
    page_addresses: Vec<usize>,
    generations: Vec<u32>,
    slots: usize,
    free: OwnedArray<u32>,
    live: usize,
    roots: Vec<Option<TextViewRoot>>,
    free_roots: Vec<u32>,
}

/// Result of one compact-table sweep.
pub(crate) struct TextViewSweep {
    pub(crate) objects: usize,
    pub(crate) bytes: usize,
}

impl TextViewTable {
    pub(crate) fn is_reference(reference: ObjRef) -> bool {
        reference.generation & TEXT_VIEW_GENERATION_TAG != 0
    }

    pub(crate) fn slot_count(&self) -> usize {
        self.slots
    }

    pub(crate) fn free_count(&self) -> usize {
        self.free.len()
    }

    pub(crate) fn page_count(&self) -> usize {
        self.pages.len()
    }

    #[cfg(test)]
    pub(crate) fn root_count(&self) -> usize {
        self.roots.iter().filter(|root| root.is_some()).count()
    }

    pub(crate) fn page_addresses(&self) -> *const usize {
        self.page_addresses.as_ptr()
    }

    fn entry(&self, slot: u32) -> Option<&TextViewEntry> {
        self.pages
            .get(slot as usize / PAGE_SLOTS)?
            .get(slot as usize % PAGE_SLOTS)
    }

    fn entry_mut(&mut self, slot: u32) -> &mut TextViewEntry {
        &mut self.pages[slot as usize / PAGE_SLOTS][slot as usize % PAGE_SLOTS]
    }

    fn try_add_page(&mut self) -> bool {
        if self.pages.try_reserve(1).is_err()
            || self.page_addresses.try_reserve(1).is_err()
            || self.free.try_reserve(PAGE_SLOTS).is_err()
        {
            return false;
        }
        let page_start = self.pages.len().saturating_mul(PAGE_SLOTS);
        let Some(page_end) = page_start.checked_add(PAGE_SLOTS) else {
            return false;
        };
        if page_end > u32::MAX as usize {
            return false;
        }
        if self
            .generations
            .try_reserve(page_end.saturating_sub(self.generations.len()))
            .is_err()
        {
            return false;
        }
        let mut entries = Vec::new();
        if entries.try_reserve_exact(PAGE_SLOTS).is_err() {
            return false;
        }
        for slot in page_start..page_end {
            entries.push(TextViewEntry::dead(
                self.generations.get(slot).copied().unwrap_or(0),
            ));
        }
        self.generations.resize(page_end, 0);
        let page = TextViewPage {
            entries: entries.into_boxed_slice(),
        };
        self.page_addresses.push(page.as_ptr() as usize);
        self.pages.push(page);
        true
    }

    fn add_page(&mut self) {
        assert!(self.try_add_page(), "a text-view page allocation failed");
    }

    fn available_slots(&self) -> usize {
        let unused = self
            .pages
            .len()
            .saturating_mul(PAGE_SLOTS)
            .saturating_sub(self.slots);
        self.free.len().saturating_add(unused)
    }

    pub(crate) fn try_reserve_batch(&mut self, count: usize, needs_root: bool) -> bool {
        if count == 0 {
            return true;
        }
        if needs_root {
            if self.roots.len() >= u32::MAX as usize && self.free_roots.is_empty() {
                return false;
            }
            if self.roots.try_reserve(1).is_err() || self.free_roots.try_reserve(1).is_err() {
                return false;
            }
        }
        while self.available_slots() < count {
            if !self.try_add_page() {
                return false;
            }
        }
        true
    }

    fn install_root(&mut self, owner: TextOwner, references: usize) -> u32 {
        let root = TextViewRoot { owner, references };
        if let Some(index) = self.free_roots.pop() {
            debug_assert!(self.roots[index as usize].is_none());
            self.roots[index as usize] = Some(root);
            return index;
        }
        let index = self.roots.len() as u32;
        self.roots.push(Some(root));
        index
    }

    fn existing_root(&self, reference: ObjRef) -> Option<u32> {
        if !Self::is_reference(reference) {
            return None;
        }
        let entry = self.entry(reference.slot)?;
        let generation = reference.generation & GENERATION_MASK;
        if !entry.is_live() || entry.generation != generation {
            return None;
        }
        self.roots.get(entry.root as usize)?.as_ref()?;
        Some(entry.root)
    }

    pub(crate) fn can_install_batch(&self, batch: &TextViewBatch) -> bool {
        match &batch.owner {
            TextViewBatchOwner::New(_) => true,
            TextViewBatchOwner::Existing(reference) => self.existing_root(*reference).is_some(),
        }
    }

    pub(crate) fn install_batch(&mut self, batch: TextViewBatch, values: &mut Vec<Value>) {
        let TextViewBatch { owner, views, .. } = batch;
        let count = views.len();
        if count == 0 {
            return;
        }
        debug_assert!(self.available_slots() >= count);
        debug_assert!(values.capacity().saturating_sub(values.len()) >= count);
        let root = match owner {
            TextViewBatchOwner::New(owner) => self.install_root(owner, count),
            TextViewBatchOwner::Existing(reference) => {
                let root = self
                    .existing_root(reference)
                    .expect("a validated compact owner stays live");
                let owner = self.roots[root as usize]
                    .as_mut()
                    .expect("a compact source owner is live");
                owner.references = owner
                    .references
                    .checked_add(count)
                    .expect("the text-view reference count fits");
                root
            }
        };
        for view in views {
            let slot = if let Some(slot) = self.free.pop() {
                slot
            } else {
                if self.slots == self.pages.len() * PAGE_SLOTS {
                    self.add_page();
                }
                let slot = self.slots as u32;
                self.slots += 1;
                slot
            };
            let entry = self.entry_mut(slot);
            let generation = entry.generation;
            entry.replace(root, view);
            values.push(Value::Obj(ObjRef {
                slot,
                generation: generation | TEXT_VIEW_GENERATION_TAG,
            }));
        }
        self.live += count;
    }

    pub(crate) fn get(&self, reference: ObjRef) -> Option<TextRef<'_>> {
        if !Self::is_reference(reference) {
            return None;
        }
        let entry = self.entry(reference.slot)?;
        let generation = reference.generation & GENERATION_MASK;
        if !entry.is_live() || entry.generation != generation {
            return None;
        }
        let root = self.roots.get(entry.root as usize)?.as_ref()?;
        Some(TextRef::compact(&root.owner, &entry.view))
    }

    fn release_root(&mut self, root: u32, count: usize) -> usize {
        let entry = self.roots[root as usize]
            .as_mut()
            .expect("a text-view root is live");
        entry.references = entry
            .references
            .checked_sub(count)
            .expect("a text-view root has enough references");
        let key = entry.owner.allocation_key();
        if entry.references == 0 {
            self.roots[root as usize] = None;
            self.free_roots.push(root);
        }
        key
    }

    pub(crate) fn free(&mut self, reference: ObjRef) -> usize {
        assert!(
            Self::is_reference(reference),
            "a text-view reference is required"
        );
        let entry = self.entry_mut(reference.slot);
        let generation = reference.generation & GENERATION_MASK;
        assert_eq!(entry.generation, generation, "stale text-view reference");
        let root = entry.take().expect("a text-view reference is live");
        entry.generation = entry.generation.wrapping_add(1) & GENERATION_MASK;
        self.generations[reference.slot as usize] = entry.generation;
        self.free.push(reference.slot);
        self.live -= 1;
        self.release_root(root, 1)
    }

    pub(crate) fn sweep(
        &mut self,
        mut keep: impl FnMut(ObjRef) -> bool,
        mut release_shared: impl FnMut(usize, usize) -> usize,
    ) -> TextViewSweep {
        let mut objects = 0usize;
        let mut bytes = 0usize;
        let Self {
            pages,
            generations,
            free,
            live,
            roots,
            free_roots,
            ..
        } = self;
        let mut free = free.vector();
        for (page_index, page) in pages.iter_mut().enumerate() {
            for (index, entry) in page.iter_mut().enumerate() {
                if !entry.is_live() {
                    continue;
                }
                let reference = ObjRef {
                    slot: (page_index * PAGE_SLOTS + index) as u32,
                    generation: entry.generation | TEXT_VIEW_GENERATION_TAG,
                };
                if keep(reference) {
                    continue;
                }
                let root = entry.take().expect("a text-view entry is live");
                entry.generation = entry.generation.wrapping_add(1) & GENERATION_MASK;
                generations[reference.slot as usize] = entry.generation;
                free.push(reference.slot);
                let root_index = root as usize;
                let root_entry = roots[root_index]
                    .as_mut()
                    .expect("a text-view root is live");
                root_entry.references = root_entry
                    .references
                    .checked_sub(1)
                    .expect("a text-view root has enough references");
                let key = root_entry.owner.allocation_key();
                if root_entry.references == 0 {
                    roots[root_index] = None;
                    free_roots.push(root);
                }
                bytes += release_shared(key, 1);
                objects += 1;
            }
        }
        *live -= objects;
        TextViewSweep { objects, bytes }
    }

    pub(crate) fn trim_free_pages(&mut self) {
        while self
            .pages
            .last()
            .is_some_and(|page| page.iter().all(|entry| !entry.is_live()))
        {
            self.pages.pop();
            self.page_addresses.pop();
        }
        self.slots = self.slots.min(self.pages.len() * PAGE_SLOTS);
        self.free.retain(|slot| (*slot as usize) < self.slots);
        self.free.shrink_to_fit();
    }

    pub(crate) fn for_each_live(&self, mut visit: impl FnMut(ObjRef, TextRef<'_>)) {
        for (page_index, page) in self.pages.iter().enumerate() {
            for (index, entry) in page.iter().enumerate() {
                if !entry.is_live() {
                    continue;
                }
                let Some(root) = self.roots[entry.root as usize].as_ref() else {
                    continue;
                };
                visit(
                    ObjRef {
                        slot: (page_index * PAGE_SLOTS + index) as u32,
                        generation: entry.generation | TEXT_VIEW_GENERATION_TAG,
                    },
                    TextRef::compact(&root.owner, &entry.view),
                );
            }
        }
    }
}

/// Size of one compact text-view entry.
pub const JIT_TEXT_VIEW_ENTRY_SIZE: usize = std::mem::size_of::<TextViewEntry>();
/// Byte offset of the compact entry generation.
pub const JIT_TEXT_VIEW_GENERATION_OFFSET: usize = std::mem::offset_of!(TextViewEntry, generation);
/// Byte offset of the compact entry root index.
pub const JIT_TEXT_VIEW_ROOT_OFFSET: usize = std::mem::offset_of!(TextViewEntry, root);
/// Byte offset of the compact text payload.
pub const JIT_TEXT_VIEW_PAYLOAD_OFFSET: usize = std::mem::offset_of!(TextViewEntry, view);

const _: () = assert!(JIT_TEXT_VIEW_GENERATION_OFFSET == 0);
const _: () = assert!(JIT_TEXT_VIEW_ROOT_OFFSET == std::mem::size_of::<u32>());
const _: () = assert!(
    JIT_TEXT_VIEW_ENTRY_SIZE == JIT_TEXT_VIEW_PAYLOAD_OFFSET + std::mem::size_of::<TextView>()
);
