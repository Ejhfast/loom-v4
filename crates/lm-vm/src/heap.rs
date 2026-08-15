//! A non-collecting page arena with a hard byte cap.
//!
//! This arena is the intentional week-1 stand-in for the final heap.
//! It sits behind a small API, so a real collector can replace it
//! without a change to the VM loop.

use lm_value::StrRef;

const PAGE_SIZE: usize = 64 * 1024;
/// Bookkeeping cost charged for each string entry.
const ENTRY_COST: usize = 16;

/// The VM heap. It stores immutable strings in bump-allocated pages.
pub struct Heap {
    pages: Vec<Vec<u8>>,
    /// `(page, offset, length)` for each allocated string.
    table: Vec<(u32, u32, u32)>,
    cap_bytes: usize,
    used_bytes: usize,
}

impl Heap {
    pub fn new(cap_bytes: usize) -> Heap {
        Heap {
            pages: Vec::new(),
            table: Vec::new(),
            cap_bytes,
            used_bytes: 0,
        }
    }

    /// Bytes charged against the cap so far.
    pub fn used_bytes(&self) -> usize {
        self.used_bytes
    }

    /// Copy a string into the arena. Return `None` when the hard cap
    /// would be exceeded.
    pub fn alloc_str(&mut self, text: &str) -> Option<StrRef> {
        let len = text.len();
        let cost = len + ENTRY_COST;
        if self.used_bytes + cost > self.cap_bytes {
            return None;
        }
        let fits_last = self
            .pages
            .last()
            .map(|page| page.capacity() - page.len() >= len)
            .unwrap_or(false);
        if !fits_last {
            self.pages.push(Vec::with_capacity(PAGE_SIZE.max(len)));
        }
        let page_idx = (self.pages.len() - 1) as u32;
        let page = self.pages.last_mut().expect("a page exists");
        let offset = page.len() as u32;
        page.extend_from_slice(text.as_bytes());
        self.used_bytes += cost;
        let idx = self.table.len() as u32;
        self.table.push((page_idx, offset, len as u32));
        Some(StrRef(idx))
    }

    /// Read a string back. The reference must come from this heap.
    pub fn get_str(&self, sref: StrRef) -> &str {
        let (page, offset, len) = self.table[sref.0 as usize];
        let bytes = &self.pages[page as usize][offset as usize..(offset + len) as usize];
        std::str::from_utf8(bytes).expect("the arena stores valid UTF-8")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_strings() {
        let mut heap = Heap::new(1 << 20);
        let a = heap.alloc_str("hello").unwrap();
        let b = heap.alloc_str("world").unwrap();
        assert_eq!(heap.get_str(a), "hello");
        assert_eq!(heap.get_str(b), "world");
    }

    #[test]
    fn enforces_hard_cap() {
        let mut heap = Heap::new(64);
        assert!(heap.alloc_str("0123456789").is_some());
        assert!(heap.alloc_str("0123456789").is_some());
        assert!(heap.alloc_str("0123456789").is_none());
    }

    #[test]
    fn large_string_gets_its_own_page() {
        let mut heap = Heap::new(1 << 21);
        let big = "x".repeat(PAGE_SIZE + 10);
        let sref = heap.alloc_str(&big).unwrap();
        assert_eq!(heap.get_str(sref).len(), PAGE_SIZE + 10);
    }
}
