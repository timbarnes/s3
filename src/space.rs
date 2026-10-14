//! Compact objects (plan/11, G1-G2): pairs, closures and records, in
//! pages of their own, where an `Obj` slot took 80 bytes and a closure's
//! free variables or a record's fields a separate allocation besides.
//!
//! - **A pair** is its car and cdr: 16 bytes.
//! - **A closure or a record** is `Headed`: a header word (its template or
//!   record type, and how many fields follow), then its fields, inline.
//! - **Size classes** are multiples of 16 bytes, one `Space` each, up to
//!   `MAX_UNITS`. A larger object gets a page of its own.
//! - **Pages are aligned to 64KB**, so an object's page, and the page's
//!   header, are found by masking the object's address. The header holds a
//!   mark bit per 16-byte unit, and in poison mode a bit per unit that is
//!   poisoned (never reused) and one per unit that is free.
//! - **Free slots are a list** threaded through their first words, in
//!   address order, so consecutive allocations fill a page from its start.
//! - **A collection** clears every mark bit, marks what it reaches, then
//!   rebuilds the free lists from the unmarked slots. These objects own
//!   nothing, so nothing is dropped.
//!
//! Nothing here moves, like every other object (`heap.rs`).

use std::alloc::{Layout, alloc_zeroed, dealloc, handle_alloc_error};
use std::cell::Cell;
use std::ptr::{self, NonNull};

use crate::heap::GcRef;
use crate::value::Value;

pub struct Pair {
    pub car: Cell<Value>,
    pub cdr: Cell<Value>,
}

/// A closure or a record: the header, then the fields.
#[repr(C)]
pub struct Headed {
    /// The template or record type's address in the low 48 bits, the
    /// number of fields in the top 16.
    header: u64,
}

impl Headed {
    /// The template (a closure's) or record type (a record's).
    #[inline(always)]
    pub fn link(&self) -> GcRef {
        GcRef::from_addr(self.header & ((1 << 48) - 1))
    }

    #[inline(always)]
    pub fn fields<'a>(&self) -> &'a [Cell<Value>] {
        // SAFETY: the header counts the fields that follow it.
        unsafe { std::slice::from_raw_parts((self as *const Headed).add(1).cast(), (self.header >> 48) as usize) }
    }
}

/// The most fields a closure or record can have.
pub const MAX_FIELDS: usize = u16::MAX as usize;

const UNIT: usize = 16;
const PAGE_BYTES: usize = 1 << 16;
const UNITS: usize = PAGE_BYTES / UNIT;
const WORDS: usize = UNITS / 64;
/// The largest size class, in units; a larger object has a page of its own.
const MAX_UNITS: usize = 16;

/// The start of every page.
#[repr(C)]
struct Header {
    mark: [u64; WORDS],
    poisoned: [u64; WORDS],
    free: [u64; WORDS],
    /// The page's slot size, in units.
    units: usize,
}

/// The first unit after the header.
const FIRST: usize = size_of::<Header>().div_ceil(UNIT);

const _: () = assert!(size_of::<Pair>() == UNIT && UNITS % 64 == 0 && FIRST + MAX_UNITS < UNITS);

/// The header of the page `p` is in, and `p`'s unit number there.
fn locate(p: *const u8) -> (*mut Header, usize) {
    let addr = p as usize;
    let base = addr & !(PAGE_BYTES - 1);
    (base as *mut Header, (addr - base) / UNIT)
}

/// Mark the object at `p`; false if it was marked already.
#[inline]
pub fn mark(p: *const u8) -> bool {
    let (h, i) = locate(p);
    // SAFETY: every object here is a slot of a page, whose header is at
    // its base.
    let word = unsafe { &mut (*h).mark[i / 64] };
    let bit = 1u64 << (i % 64);
    let unmarked = *word & bit == 0;
    *word |= bit;
    unmarked
}

/// A `Headed` object's size in units.
fn headed_units(fields: usize) -> usize {
    (8 + 8 * fields).div_ceil(UNIT)
}

fn bit(h: &[u64; WORDS], i: usize) -> bool {
    h[i / 64] & (1 << (i % 64)) != 0
}

fn set(h: &mut [u64; WORDS], i: usize) {
    h[i / 64] |= 1 << (i % 64);
}

/// A page of `bytes`, aligned to 64KB, its header zeroed.
fn new_page(bytes: usize, units: usize) -> NonNull<Header> {
    let layout = Layout::from_size_align(bytes, PAGE_BYTES).unwrap();
    // SAFETY: the layout isn't zero-sized.
    let page = NonNull::new(unsafe { alloc_zeroed(layout) }.cast::<Header>()).unwrap_or_else(|| handle_alloc_error(layout));
    // SAFETY: the page starts with its header.
    unsafe { (*page.as_ptr()).units = units };
    page
}

fn unit(page: NonNull<Header>, i: usize) -> *mut u64 {
    // SAFETY: i is inside the page.
    unsafe { page.as_ptr().cast::<u8>().add(i * UNIT).cast() }
}

/// Fill a dead object with `freed`: both words of a pair, the fields of a
/// headed object (its header is kept, so its size is still known).
fn poison_slot(p: *mut u64, is_pair: bool, freed: Value) {
    // SAFETY: a slot that held an object nothing reaches.
    unsafe {
        if is_pair {
            p.cast::<Pair>().write(Pair { car: Cell::new(freed), cdr: Cell::new(freed) });
        } else {
            for f in (*p.cast::<Headed>()).fields() {
                f.set(freed);
            }
        }
    }
}

/// The pages of one size class.
struct Space {
    units: usize,
    /// Pairs or headed objects: what poisoning writes.
    pairs: bool,
    pages: Vec<NonNull<Header>>,
    /// The first free slot; each free slot's first word holds the next
    /// one's address, null at the end.
    free: *mut u64,
    /// Objects in use: allocated since the last collection, or alive at it.
    count: usize,
}

impl Space {
    fn new(units: usize, pairs: bool) -> Space {
        Space { units, pairs, pages: Vec::new(), free: ptr::null_mut(), count: 0 }
    }

    #[inline]
    fn alloc(&mut self) -> *mut u64 {
        if self.free.is_null() {
            self.grow();
        }
        let p = self.free;
        // SAFETY: a free slot, whose first word links to the next.
        self.free = unsafe { *p } as *mut u64;
        self.count += 1;
        p
    }

    /// Add a page, all of its slots free.
    #[cold]
    fn grow(&mut self) {
        let page = new_page(PAGE_BYTES, self.units);
        self.pages.push(page);
        let slots = (UNITS - FIRST) / self.units;
        for k in (0..slots).rev() {
            let p = unit(page, FIRST + k * self.units);
            // SAFETY: a slot of the new page.
            unsafe { *p = self.free as u64 };
            self.free = p;
        }
    }

    fn sweep(&mut self, poison: bool, freed: Value) {
        if poison {
            // Slots free before the collection are free still; the other
            // unmarked ones were in use, and are poisoned.
            let mut p = self.free;
            while !p.is_null() {
                let (h, i) = locate(p.cast());
                // SAFETY: a page's header; `p` a free slot.
                unsafe {
                    set(&mut (*h).free, i);
                    p = *p as *mut u64;
                }
            }
        }
        let slots = (UNITS - FIRST) / self.units;
        let mut free = ptr::null_mut();
        let mut live = 0;
        for &page in self.pages.iter().rev() {
            // SAFETY: a page's header, not otherwise referenced here.
            let h = unsafe { &mut *page.as_ptr() };
            for k in (0..slots).rev() {
                let i = FIRST + k * self.units;
                let p = unit(page, i);
                if bit(&h.mark, i) {
                    live += 1;
                } else if bit(&h.poisoned, i) {
                } else if poison && !bit(&h.free, i) {
                    set(&mut h.poisoned, i);
                    poison_slot(p, self.pairs, freed);
                } else {
                    // SAFETY: an unused slot.
                    unsafe { *p = free as u64 };
                    free = p;
                }
            }
            h.free = [0; WORDS];
        }
        self.free = free;
        self.count = live;
    }
}

/// Every compact object.
pub struct Small {
    pairs: Space,
    /// `headed[u]`: headed objects of `u` units, for u up to MAX_UNITS.
    headed: Vec<Space>,
    /// Larger headed objects, a page each.
    large: Vec<NonNull<Header>>,
    /// Of those, the ones in use (not poisoned).
    large_count: usize,
}

impl Default for Small {
    fn default() -> Small {
        Small { pairs: Space::new(1, true), headed: (0..=MAX_UNITS).map(|u| Space::new(u.max(1), false)).collect(), large: Vec::new(), large_count: 0 }
    }
}

/// The layout of a page for a large object of `units`.
fn large_layout(units: usize) -> Layout {
    Layout::from_size_align((FIRST + units) * UNIT, PAGE_BYTES).unwrap()
}

impl Small {
    pub fn cons(&mut self, car: Value, cdr: Value) -> NonNull<Pair> {
        let p = self.pairs.alloc().cast::<Pair>();
        // SAFETY: a free slot of a pair page.
        unsafe {
            p.write(Pair { car: Cell::new(car), cdr: Cell::new(cdr) });
            NonNull::new_unchecked(p)
        }
    }

    /// A closure or record linking to `link`, with `fields`; at most
    /// MAX_FIELDS of them.
    pub fn headed(&mut self, link: GcRef, fields: impl ExactSizeIterator<Item = Value>) -> NonNull<Headed> {
        let n = fields.len();
        assert!(n <= MAX_FIELDS, "too many fields: {}", n);
        let units = headed_units(n);
        let p = if units <= MAX_UNITS {
            self.headed[units].alloc()
        } else {
            let page = new_page(large_layout(units).size(), units);
            self.large.push(page);
            self.large_count += 1;
            unit(page, FIRST)
        };
        // SAFETY: a free slot of `units`, room for the header and fields.
        unsafe {
            p.write(link.addr() | (n as u64) << 48);
            for (k, v) in fields.enumerate() {
                p.add(1 + k).cast::<Cell<Value>>().write(Cell::new(v));
            }
            NonNull::new_unchecked(p.cast())
        }
    }

    /// Objects in use.
    pub fn count(&self) -> usize {
        self.pairs.count + self.headed.iter().map(|s| s.count).sum::<usize>() + self.large_count
    }

    /// The bytes the pages take.
    #[cfg(any(test, feature = "gcstats"))]
    pub fn bytes(&self) -> usize {
        let pages = self.pairs.pages.len() + self.headed.iter().map(|s| s.pages.len()).sum::<usize>();
        // SAFETY: each a large page's header.
        pages * PAGE_BYTES + self.large.iter().map(|p| large_layout(unsafe { (*p.as_ptr()).units }).size()).sum::<usize>()
    }

    /// Clear every mark, before a collection marks again.
    pub fn clear_marks(&mut self) {
        let spaces = std::iter::once(&self.pairs).chain(&self.headed);
        for page in spaces.flat_map(|s| &s.pages).chain(&self.large) {
            // SAFETY: a page's header.
            unsafe { (*page.as_ptr()).mark = [0; WORDS] };
        }
    }

    /// Free everything the collection didn't mark, or in poison mode fill
    /// it with `freed` (the symbol `<<FREED>>`) and never reuse it.
    pub fn sweep(&mut self, poison: bool, freed: Value) {
        self.pairs.sweep(poison, freed);
        self.headed.iter_mut().for_each(|s| s.sweep(poison, freed));
        let mut live = 0;
        self.large.retain(|&page| {
            // SAFETY: a large page's header, its object at FIRST.
            let h = unsafe { &mut *page.as_ptr() };
            if bit(&h.mark, FIRST) {
                live += 1;
                return true;
            }
            if bit(&h.poisoned, FIRST) {
                return true;
            }
            if poison {
                set(&mut h.poisoned, FIRST);
                poison_slot(unit(page, FIRST), false, freed);
                return true;
            }
            // SAFETY: allocated in `headed` with this layout.
            unsafe { dealloc(page.as_ptr().cast(), large_layout(h.units)) };
            false
        });
        self.large_count = live;
    }
}

impl Drop for Small {
    fn drop(&mut self) {
        let spaces = std::iter::once(&self.pairs).chain(&self.headed);
        for &page in spaces.flat_map(|s| &s.pages) {
            // SAFETY: allocated in `grow` with this layout.
            unsafe { dealloc(page.as_ptr().cast(), Layout::from_size_align(PAGE_BYTES, PAGE_BYTES).unwrap()) };
        }
        for &page in &self.large {
            // SAFETY: allocated in `headed` with this layout.
            unsafe { dealloc(page.as_ptr().cast(), large_layout((*page.as_ptr()).units)) };
        }
    }
}
