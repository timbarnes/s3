//! Pairs (plan/11, G1): 16-byte slots holding only the car and the cdr,
//! in pages of their own, where an `Obj` slot took 80 bytes.
//!
//! - **Pages are aligned to their size**, so a pair's page, and the page's
//!   header, are found by masking the pair's address.
//! - **The header** is the page's first slots: a mark bit per slot, and in
//!   poison mode a bit per slot that is poisoned (never reused) and one
//!   per slot that is free.
//! - **Free slots are a list** threaded through their cars, in address
//!   order, so consecutive conses fill a page from its start.
//! - **A collection** clears every mark bit, marks the pairs it reaches,
//!   then rebuilds the free list from the unmarked slots. A pair owns
//!   nothing, so nothing is dropped.
//!
//! Pairs never move, like every other object (`heap.rs`).

use std::alloc::{Layout, alloc_zeroed, dealloc, handle_alloc_error};
use std::cell::Cell;
use std::ptr::{self, NonNull};

use crate::value::Value;

pub struct Pair {
    pub car: Cell<Value>,
    pub cdr: Cell<Value>,
}

const PAGE_BYTES: usize = 1 << 16;
const SLOTS: usize = PAGE_BYTES / size_of::<Pair>();
const WORDS: usize = SLOTS / 64;

/// The start of every page.
#[repr(C)]
struct Header {
    mark: [u64; WORDS],
    poisoned: [u64; WORDS],
    free: [u64; WORDS],
}

/// The first slot after the header.
const FIRST: usize = size_of::<Header>().div_ceil(size_of::<Pair>());

const _: () = assert!(size_of::<Pair>() == 16 && SLOTS % 64 == 0 && FIRST < SLOTS);

/// The header of the page `p` is in, and `p`'s slot number there.
fn locate(p: *const Pair) -> (*mut Header, usize) {
    let addr = p as usize;
    let base = addr & !(PAGE_BYTES - 1);
    (base as *mut Header, (addr - base) / size_of::<Pair>())
}

/// Mark `p`; false if it was marked already.
#[inline]
pub fn mark(p: &Pair) -> bool {
    let (h, i) = locate(p);
    // SAFETY: every pair is a slot of a page, whose header is at its base.
    let word = unsafe { &mut (*h).mark[i / 64] };
    let bit = 1u64 << (i % 64);
    let unmarked = *word & bit == 0;
    *word |= bit;
    unmarked
}

pub struct Pairs {
    pages: Vec<NonNull<Header>>,
    /// The first free slot; each free slot's car holds the next one's
    /// address, null at the end.
    free: *mut Pair,
    /// Pairs in use: allocated since the last collection, or alive at it.
    pub count: usize,
}

impl Default for Pairs {
    fn default() -> Pairs {
        Pairs { pages: Vec::new(), free: ptr::null_mut(), count: 0 }
    }
}

fn layout() -> Layout {
    Layout::from_size_align(PAGE_BYTES, PAGE_BYTES).unwrap()
}

/// The address a free slot's car holds.
fn next_free(p: *mut Pair) -> *mut Pair {
    // SAFETY: `p` is a free slot, whose car was set by `link`.
    unsafe { (*p).car.get().bits() as *mut Pair }
}

/// Put `p` at the front of a free list starting at `next`.
fn link(p: *mut Pair, next: *mut Pair) {
    // SAFETY: `p` is a slot of a page, and not in use.
    unsafe { ptr::addr_of_mut!((*p).car).write(Cell::new(Value::from_bits(next as u64))) };
}

fn slot(page: NonNull<Header>, i: usize) -> *mut Pair {
    // SAFETY: i < SLOTS, inside the page.
    unsafe { page.as_ptr().cast::<Pair>().add(i) }
}

impl Pairs {
    pub fn alloc(&mut self, car: Value, cdr: Value) -> NonNull<Pair> {
        if self.free.is_null() {
            self.new_page();
        }
        let p = self.free;
        self.free = next_free(p);
        // SAFETY: a free slot of a page.
        unsafe { p.write(Pair { car: Cell::new(car), cdr: Cell::new(cdr) }) };
        self.count += 1;
        // SAFETY: slots are never null.
        unsafe { NonNull::new_unchecked(p) }
    }

    /// Add a page, all of its slots free.
    #[cold]
    fn new_page(&mut self) {
        // SAFETY: the layout isn't zero-sized. Zeroed, the header has no
        // marks and nothing poisoned.
        let page = NonNull::new(unsafe { alloc_zeroed(layout()) }.cast::<Header>()).unwrap_or_else(|| handle_alloc_error(layout()));
        self.pages.push(page);
        for i in (FIRST..SLOTS).rev() {
            let p = slot(page, i);
            link(p, self.free);
            self.free = p;
        }
    }

    /// The bytes the pages take.
    #[cfg(any(test, feature = "gcstats"))]
    pub fn bytes(&self) -> usize {
        self.pages.len() * PAGE_BYTES
    }

    /// Clear every mark, before a collection marks again.
    pub fn clear_marks(&mut self) {
        for page in &self.pages {
            // SAFETY: a page's header.
            unsafe { (*page.as_ptr()).mark = [0; WORDS] };
        }
    }

    /// Free every pair the collection didn't mark, or in poison mode fill
    /// it with `freed` (the symbol `<<FREED>>`) and never reuse it.
    pub fn sweep(&mut self, poison: bool, freed: Value) {
        if poison {
            // Slots free before the collection are free still; the others
            // unmarked were in use, and are poisoned.
            let mut p = self.free;
            while !p.is_null() {
                let (h, i) = locate(p);
                // SAFETY: a page's header.
                unsafe { (*h).free[i / 64] |= 1 << (i % 64) };
                p = next_free(p);
            }
        }
        let mut free = ptr::null_mut();
        let mut live = 0;
        for &page in self.pages.iter().rev() {
            // SAFETY: a page's header, not otherwise referenced here.
            let h = unsafe { &mut *page.as_ptr() };
            for i in (FIRST..SLOTS).rev() {
                let (w, bit) = (i / 64, 1u64 << (i % 64));
                if h.mark[w] & bit != 0 {
                    live += 1;
                } else if h.poisoned[w] & bit != 0 {
                } else if poison && h.free[w] & bit == 0 {
                    h.poisoned[w] |= bit;
                    let p = slot(page, i);
                    // SAFETY: a slot that held a pair nothing reaches.
                    unsafe { p.write(Pair { car: Cell::new(freed), cdr: Cell::new(freed) }) };
                } else {
                    let p = slot(page, i);
                    link(p, free);
                    free = p;
                }
            }
            h.free = [0; WORDS];
        }
        self.free = free;
        self.count = live;
    }
}

impl Drop for Pairs {
    fn drop(&mut self) {
        for &page in &self.pages {
            // SAFETY: allocated in new_page with this layout.
            unsafe { dealloc(page.as_ptr().cast(), layout()) };
        }
    }
}
