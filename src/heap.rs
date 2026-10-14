//! The heap: object layout, allocation and the symbol table (plan/03 §3-4).
//!
//! Every object is one slot of `Slots`: pages of same-sized slots, with a
//! free list (heap step 2, plan/09), except pairs, closures and records,
//! which have compact slots of their own (`space.rs`, plan/11 G1-G2).
//! Objects never
//! move, so a `GcRef` stays valid for as long as the object is alive.
//! Mutable fields are `Cell`s (or a `RefCell` for strings), so all access is
//! through shared references, unlike s1's `get_value_mut(&self)`.
//!
//! Garbage collection is non-moving mark-sweep (D3), adapted from s1:
//!
//! - **Epoch marks.** Each collection bumps `epoch`, and an object is marked
//!   when its `mark` equals it, so there is no unmark pass.
//! - **An iterative worklist**, so deep structures don't overflow the Rust
//!   stack.
//! - **Scheduling in proportion to the live heap** (`needs_gc`).
//! - **Allocation never collects.** The VM calls `collect` at its
//!   safepoints, passing the roots it holds; the heap adds the symbol table
//!   (a root, not weak: the simplest choice). A `GcRef` is valid while its
//!   object is reachable from the roots at every safepoint.
//!
//! Two debugging modes, read from the environment by `Heap::new`:
//! `S3_GC_STRESS` collects at every safepoint, and `S3_GC_POISON` turns
//! freed objects into the symbol `<<FREED>>` (and leaks them) instead of
//! freeing them, so a premature free shows up in output.

use std::cell::{Cell, RefCell};
use std::ptr::NonNull;

use num_bigint::BigInt;
use num_rational::BigRational;
use num_traits::{One, ToPrimitive};
use rustc_hash::FxHashMap;

use crate::builtins::{ErrorKind, Primitive};
use crate::compile::syntax_rules::SyntaxRules;
use crate::globals::{Env, env_of};
use crate::io::PortKind;
use crate::libraries::Libraries;
use crate::space::{self, Small};
use crate::sstring::SString;
use crate::value::Value;
use crate::vm::{Frame, Template};

/// A reference to a heap object. Compared and hashed by identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GcRef(NonNull<GcObject>);

impl GcRef {
    /// The object. Valid while the object is alive: in P0 that is until the
    /// heap is dropped, and from P3, while it is reachable from the roots.
    /// The lifetime is unbounded because the GC, not the borrow checker,
    /// decides how long that is.
    pub fn get<'a>(self) -> &'a GcObject {
        // Safety: see above; the heap owns the allocation and never moves it.
        unsafe { self.0.as_ref() }
    }

    /// The object's address, for `Value`'s encoding.
    pub(crate) fn addr(self) -> u64 {
        self.0.as_ptr() as u64
    }

    /// The `GcRef` at `addr`, which must have come from `addr`.
    #[inline]
    pub(crate) fn from_addr(addr: u64) -> GcRef {
        debug_assert!(addr != 0, "a GcRef is never null");
        // SAFETY: `addr` came from a GcRef, which is never null.
        GcRef(unsafe { NonNull::new_unchecked(addr as *mut GcObject) })
    }
}

/// A heap object.
pub struct GcObject {
    /// The GC epoch in which the object was last marked.
    pub mark: Cell<u64>,
    pub kind: Obj,
}

// An object is one 80-byte allocation. The large, rarely made variants
// (templates, macros, environments, ports, ratios) are boxed: unboxed, a
// template made every pair 128 bytes. The largest left is Escape;
// ponytail: boxing its frame would make 48, but changing its code in the
// dispatch loop made calls 10-20% slower (plan/09).
const _: () = assert!(std::mem::size_of::<GcObject>() == 80);

/// The kinds of heap object.
/// Pairs, closures and records are not among them (`space.rs`).
pub enum Obj {
    /// An interned symbol. Its name never changes.
    Symbol(Box<str>),
    Str(RefCell<SString>),
    Vector(Box<[Cell<Value>]>),
    Bytevector(Box<[Cell<u8>]>),
    /// An exact integer outside the fixnum range. Never one that fits.
    BigInt(BigInt),
    /// An exact non-integer, in lowest terms.
    Rational(Box<BigRational>),
    /// A top-level binding cell (D7): a global variable. Holds
    /// `Value::UNDEFINED` until the variable is defined.
    Global { name: GcRef, value: Cell<Value> },
    /// An identifier introduced by a macro expansion (plan/03 §12): it
    /// renames `original` (a symbol, or another alias), and if nothing in
    /// the expansion binds it, it means what `original` meant where the
    /// macro was defined: in the first `depth` of the expander's scope
    /// frames (0 for a top-level macro), over the top-level environment
    /// `env`. Prints as its symbol's name.
    Alias { original: Value, depth: u32, env: GcRef },
    /// A top-level environment (`globals.rs`).
    Environment(Box<RefCell<Env>>),
    /// A `syntax-rules` transformer, bound by `define-syntax`,
    /// `let-syntax` or `letrec-syntax`. Exists only at compile time.
    Macro(Box<SyntaxRules>),
    /// A variable that is both captured and assigned, or used before its
    /// `letrec` init has run (03 §7, `analyze::boxed_variables`). Closures
    /// copy the box, so they share the variable. Never seen by Scheme code.
    Box(Cell<Value>),
    /// An input or output port (R7RS §6.13). Port procedures change it in
    /// place, so every reference sees the same position and state.
    Port(Box<RefCell<PortKind>>),
    /// A record type made by `define-record-type`: its name, and its field
    /// names in order.
    RecordType { name: Value, fields: Box<[Value]> },
    /// A continuation captured by `%call/cc` (D8): copies of the value and
    /// frame stacks. Invoking it copies them back and returns its argument
    /// to the topmost frame.
    Continuation { stack: Box<[Value]>, frames: Box<[Frame]> },
    /// An escape-only continuation, from `%call/ec`: the stack heights to
    /// cut back to and the frame to return into (None for the top level).
    /// Valid only while that frame is still on the frame stack.
    Escape { stack_len: usize, frames_len: usize, frame: Option<Frame> },
    /// The result of `values` with zero or several values (one value is
    /// never wrapped).
    Values(Box<[Value]>),
    /// An error object (R7RS §6.11): made by `error`, or by the VM when a
    /// builtin or an instruction fails.
    Error { kind: ErrorKind, message: Value, irritants: Value },
    /// A procedure written in Rust.
    Primitive(&'static Primitive),
    /// Compiled code (`vm/op.rs`). Templates are heap objects so that a
    /// template's constant pool can hold the templates of its inner lambdas.
    Template(Box<Template>),
}

/// The minimum number of allocations between collections. 100,000 rather
/// than s1's 20,000: a small heap was collected so often that re-marking
/// what the system itself holds showed (deriv 1.11x, paraffins and guard
/// 1.09x), for a peak a few MB larger (plan/11, G3).
pub(crate) const GC_THRESHOLD: usize = 100_000;

/// How many allocations, per object that survived the last collection,
/// come before the next one. 2 rather than 1: a program with a large live
/// heap re-marks it half as often (`gcbench` 1.33x, `mperm` 1.12x), for a
/// peak heap up to about 1.4 times larger (plan/09). Measured again with
/// compact objects: 1 saves a fifth to a half of the peak, and is 5-27%
/// slower (plan/11, G3).
const GC_GROWTH: usize = 2;

/// Owns every heap object.
pub struct Heap {
    slots: Slots,
    small: Small,
    symbols: FxHashMap<Box<str>, GcRef>,
    /// Allocations since the last collection.
    allocations: usize,
    /// How many objects survived the last collection.
    live_after_gc: usize,
    epoch: u64,
    /// The mark worklist, kept between collections to reuse its capacity.
    worklist: Vec<Value>,
    /// Collect at every safepoint.
    pub stress: bool,
    /// Poison freed objects instead of freeing them.
    pub poison: bool,
    /// Statistics, for tests: collections run, and objects marked by all
    /// of them together.
    pub collections: usize,
    pub marked: usize,
    /// The system and interaction environments (`globals.rs`), and the
    /// registered libraries. They live here, beside the symbol table, so
    /// the compiler and the builtins can reach them, and they are roots.
    pub system: Option<GcRef>,
    pub interaction: Option<GcRef>,
    pub libraries: Libraries,
    /// Values Rust code holds across a run of the VM, which must survive
    /// its collections: a top-level form waiting to be expanded again
    /// after a library file it imports has been loaded.
    pub pinned: Vec<Value>,
    #[cfg(feature = "gcstats")]
    pub gc_stats: crate::gcstats::GcStats,
}

impl Heap {
    /// The number of objects on the heap, live or not yet collected.
    #[cfg(test)]
    pub fn object_count(&self) -> usize {
        self.slots.count + self.small.count()
    }

    pub fn new() -> Heap {
        Heap {
            slots: Slots::default(),
            small: Small::default(),
            symbols: FxHashMap::default(),
            allocations: 0,
            live_after_gc: 0,
            epoch: 0,
            worklist: Vec::new(),
            stress: std::env::var_os("S3_GC_STRESS").is_some(),
            poison: std::env::var_os("S3_GC_POISON").is_some(),
            collections: 0,
            marked: 0,
            system: None,
            interaction: None,
            libraries: Libraries::default(),
            pinned: Vec::new(),
            #[cfg(feature = "gcstats")]
            gc_stats: Default::default(),
        }
    }

    fn alloc(&mut self, kind: Obj) -> Value {
        self.allocations += 1;
        #[cfg(feature = "gcstats")]
        self.gc_stats.alloc(&kind);
        Value::obj(GcRef(self.slots.alloc(kind)))
    }

    /// The number of objects in the heap, live or not yet collected.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.slots.count + self.small.count()
    }

    /// Whether the next safepoint should collect: in stress mode always;
    /// otherwise once there have been more allocations than the threshold
    /// *and* than `GC_GROWTH` times the objects that survived the last
    /// collection. The second condition keeps collection time proportional
    /// to allocation, however large the live heap grows. With a fixed
    /// interval, a computation whose live data keeps growing (a deep
    /// non-tail recursion) re-marks all of it every interval, which is
    /// quadratic.
    #[inline]
    pub fn needs_gc(&self) -> bool {
        self.stress || self.allocations > GC_THRESHOLD.max(GC_GROWTH * self.live_after_gc)
    }

    /// Collect: mark everything reachable from `roots`, the symbol table,
    /// the system and interaction environments and the libraries, then free
    /// the rest.
    pub fn collect(&mut self, roots: impl IntoIterator<Item = Value>) {
        if self.poison {
            // What freed compact objects hold. Interned before marking, so
            // the symbol table keeps it.
            self.intern("<<FREED>>");
        }
        self.epoch += 1;
        #[cfg(feature = "gcstats")]
        let t0 = std::time::Instant::now();
        self.small.clear_marks();
        let mut m = Marker { epoch: self.epoch, grey: std::mem::take(&mut self.worklist), count: 0, #[cfg(feature = "gcstats")] young: 0 };
        roots.into_iter().for_each(|v| m.value(v));
        self.symbols.values().for_each(|&r| m.obj(r));
        self.system.iter().chain(&self.interaction).for_each(|&r| m.obj(r));
        self.libraries.trace(|r| m.obj(r));
        self.pinned.iter().for_each(|&v| m.value(v));
        m.trace();
        self.worklist = m.grey;
        self.marked += m.count;
        self.collections += 1;
        #[cfg(feature = "gcstats")]
        let t1 = std::time::Instant::now();

        self.slots.sweep(self.epoch, self.poison, #[cfg(feature = "gcstats")] &mut self.gc_stats);
        let freed = self.symbols.get("<<FREED>>").map_or(Value::UNDEFINED, |&r| Value::obj(r));
        self.small.sweep(self.poison, freed);
        #[cfg(feature = "gcstats")]
        {
            let st = &mut self.gc_stats;
            st.mark += t1 - t0;
            st.sweep += t1.elapsed();
            st.young += self.allocations as u64;
            st.young_survived += m.young as u64;
            st.old_marked += (m.count - m.young) as u64;
            let slot = std::mem::size_of::<GcObject>();
            st.peak_live = st.peak_live.max(self.slots.count * slot);
            st.peak_slots = st.peak_slots.max(self.slots.pages.len() * PAGE_SLOTS * slot + self.small.bytes());
        }
        self.allocations = 0;
        self.live_after_gc = self.slots.count + self.small.count();
    }

    /// Print the collector's statistics (`gcstats.rs`), before the process
    /// exits.
    #[cfg(feature = "gcstats")]
    pub fn report_gc_stats(&self) {
        self.gc_stats.report(self.collections, self.marked, std::mem::size_of::<GcObject>());
        eprintln!("gc: compact pages at exit {:.1}MB", self.small.bytes() as f64 / (1 << 20) as f64);
    }

    /// Write out every file output port's buffer: before the process
    /// exits, which runs no destructors.
    pub fn flush_ports(&self) {
        for r in self.slots.objects() {
            if let Obj::Port(port) = &r.kind
                && let Ok(mut port) = port.try_borrow_mut()
                && let PortKind::FileOutput { file, .. } = &mut *port
            {
                std::io::Write::flush(file).ok();
            }
        }
    }

    pub fn cons(&mut self, car: Value, cdr: Value) -> Value {
        self.allocations += 1;
        #[cfg(feature = "gcstats")]
        self.gc_stats.alloc_pair();
        Value::pair(self.small.cons(car, cdr))
    }

    /// The symbol named `name`, the same object every time.
    pub fn intern(&mut self, name: &str) -> Value {
        if let Some(&r) = self.symbols.get(name) {
            return Value::obj(r);
        }
        let v = self.alloc(Obj::Symbol(name.into()));
        self.symbols.insert(name.into(), v.as_ref().unwrap());
        v
    }

    pub fn string(&mut self, s: &str) -> Value {
        self.alloc(Obj::Str(RefCell::new(SString::from(s))))
    }

    pub fn vector(&mut self, items: Vec<Value>) -> Value {
        self.alloc(Obj::Vector(items.into_iter().map(Cell::new).collect()))
    }

    pub fn bytevector(&mut self, bytes: Vec<u8>) -> Value {
        self.alloc(Obj::Bytevector(bytes.into_iter().map(Cell::new).collect()))
    }

    /// An exact integer: a fixnum if it fits, otherwise a bignum. Every
    /// integer therefore has exactly one representation.
    pub fn integer(&mut self, n: BigInt) -> Value {
        match n.to_i64().and_then(Value::fixnum) {
            Some(v) => v,
            None => self.alloc(Obj::BigInt(n)),
        }
    }

    /// An exact rational, as an integer if its denominator is 1.
    pub fn rational(&mut self, r: BigRational) -> Value {
        if r.denom().is_one() {
            self.integer(r.to_integer())
        } else {
            self.alloc(Obj::Rational(Box::new(r)))
        }
    }

    /// A new, unbound binding cell for the symbol `name`.
    pub fn global(&mut self, name: GcRef) -> GcRef {
        let v = self.alloc(Obj::Global { name, value: Cell::new(Value::UNDEFINED) });
        v.as_ref().unwrap()
    }

    pub fn primitive(&mut self, p: &'static Primitive) -> Value {
        self.alloc(Obj::Primitive(p))
    }

    pub fn template(&mut self, t: Template) -> GcRef {
        self.alloc(Obj::Template(Box::new(t))).as_ref().unwrap()
    }

    pub fn alias(&mut self, original: Value, depth: u32, env: GcRef) -> Value {
        self.alloc(Obj::Alias { original, depth, env })
    }

    pub fn environment(&mut self, env: Env) -> GcRef {
        self.alloc(Obj::Environment(Box::new(RefCell::new(env)))).as_ref().unwrap()
    }

    pub fn make_macro(&mut self, rules: SyntaxRules) -> GcRef {
        self.alloc(Obj::Macro(Box::new(rules))).as_ref().unwrap()
    }

    pub fn port(&mut self, port: PortKind) -> Value {
        self.alloc(Obj::Port(Box::new(RefCell::new(port))))
    }

    pub fn record_type(&mut self, name: Value, fields: Vec<Value>) -> Value {
        self.alloc(Obj::RecordType { name, fields: fields.into() })
    }

    pub fn record(&mut self, rtype: GcRef, fields: Vec<Value>) -> Value {
        self.allocations += 1;
        #[cfg(feature = "gcstats")]
        self.gc_stats.alloc_compact(2, "record", fields.len());
        Value::record(self.small.headed(rtype, fields.into_iter()))
    }

    pub fn continuation(&mut self, stack: Vec<Value>, frames: Vec<Frame>) -> Value {
        self.alloc(Obj::Continuation { stack: stack.into(), frames: frames.into() })
    }

    pub fn escape(&mut self, stack_len: usize, frames_len: usize, frame: Option<Frame>) -> Value {
        self.alloc(Obj::Escape { stack_len, frames_len, frame })
    }

    /// `values` of `items`: the item itself if there is exactly one.
    pub fn values(&mut self, items: Vec<Value>) -> Value {
        if items.len() == 1 { items[0] } else { self.alloc(Obj::Values(items.into())) }
    }

    pub fn error_object(&mut self, kind: ErrorKind, message: Value, irritants: Value) -> Value {
        self.alloc(Obj::Error { kind, message, irritants })
    }

    pub fn make_box(&mut self, v: Value) -> Value {
        self.alloc(Obj::Box(Cell::new(v)))
    }

    pub fn closure(&mut self, template: GcRef, free: impl ExactSizeIterator<Item = Value>) -> Value {
        self.allocations += 1;
        #[cfg(feature = "gcstats")]
        self.gc_stats.alloc_compact(1, "closure", free.len());
        Value::closure(self.small.headed(template, free))
    }

    /// The list of `items`.
    pub fn list(&mut self, items: Vec<Value>) -> Value {
        items.into_iter().rev().fold(Value::NIL, |tail, v| self.cons(v, tail))
    }
}

/// The mark phase of one collection. An object is marked when it is pushed
/// on `grey`, so an object reachable by several paths is traced once.
struct Marker {
    epoch: u64,
    grey: Vec<Value>,
    count: usize,
    /// Of `count`, the objects never marked before: allocated since the
    /// last collection.
    #[cfg(feature = "gcstats")]
    young: usize,
}

impl Marker {
    fn value(&mut self, v: Value) {
        if let Some(r) = v.as_ref() {
            self.obj(r);
        } else if let Some(p) = v.compact_addr()
            && space::mark(p)
        {
            self.count += 1;
            self.grey.push(v);
        }
    }

    fn obj(&mut self, r: GcRef) {
        let mark = &r.get().mark;
        if mark.get() != self.epoch {
            #[cfg(feature = "gcstats")]
            {
                self.young += (mark.get() == 0) as usize;
            }
            mark.set(self.epoch);
            self.count += 1;
            self.grey.push(Value::obj(r));
        }
    }

    /// Mark everything reachable from the grey objects. The match has no
    /// `_` arm, so a new kind of object must say what it refers to.
    fn trace(&mut self) {
        while let Some(v) = self.grey.pop() {
            let Some(r) = v.as_ref() else {
                if let Some(p) = v.as_pair() {
                    self.value(p.car.get());
                    self.value(p.cdr.get());
                } else {
                    let h = v.as_closure().or(v.as_record()).unwrap();
                    self.obj(h.link());
                    h.fields().iter().for_each(|f| self.value(f.get()));
                }
                continue;
            };
            match &r.get().kind {
                Obj::Vector(items) => items.iter().for_each(|c| self.value(c.get())),
                Obj::Box(value) => self.value(value.get()),
                Obj::Continuation { stack, frames } => {
                    stack.iter().for_each(|&v| self.value(v));
                    frames.iter().for_each(|f| self.value(Value::closure(f.closure)));
                }
                Obj::Escape { frame, .. } => frame.iter().for_each(|f| self.value(Value::closure(f.closure))),
                Obj::Values(items) => items.iter().for_each(|&v| self.value(v)),
                Obj::Error { message, irritants, .. } => {
                    self.value(*message);
                    self.value(*irritants);
                }
                Obj::RecordType { name, fields } => {
                    self.value(*name);
                    fields.iter().for_each(|&f| self.value(f));
                }
                Obj::Alias { original, env, .. } => {
                    self.value(*original);
                    self.obj(*env);
                }
                Obj::Environment(_) => env_of(r).borrow().trace(|x| self.obj(x)),
                Obj::Macro(m) => {
                    self.obj(m.env);
                    m.ellipsis.iter().chain(&m.literals).for_each(|&r| self.obj(r));
                    m.rules.iter().for_each(|&(p, t)| {
                        self.value(p);
                        self.value(t);
                    });
                }
                Obj::Global { name, value } => {
                    self.obj(*name);
                    self.value(value.get());
                }
                Obj::Template(t) => t.consts.iter().for_each(|&k| self.value(k)),
                Obj::Symbol(_) | Obj::Str(_) | Obj::Bytevector(_) | Obj::BigInt(_) | Obj::Rational(_) | Obj::Primitive(_) | Obj::Port(_) => {}
            }
        }
    }
}

/// `eqv?`: `eq?`, or numbers with the same exactness and value.
pub fn eqv(a: Value, b: Value) -> bool {
    a.eq(b)
        || match (a.as_obj().map(|o| &o.kind), b.as_obj().map(|o| &o.kind)) {
            (Some(Obj::BigInt(x)), Some(Obj::BigInt(y))) => x == y,
            (Some(Obj::Rational(x)), Some(Obj::Rational(y))) => x == y,
            _ => false,
        }
}

/// `equal?`: `eqv?`, or pairs, strings, vectors and bytevectors with equal
/// contents. Terminates on cyclic data, as R7RS requires: after many steps
/// it starts recording which pairs of nodes are being compared, and treats
/// meeting one again as equal (that comparison is already under way higher
/// up). Forked from s1's `gc::equal`.
pub fn equal(a: Value, b: Value) -> bool {
    equal_with(a, b, &mut EqualState { steps: 0, seen: None })
}

/// Cycle detection for `equal`.
struct EqualState {
    /// Node pairs compared so far.
    steps: usize,
    /// Node pairs under comparison; only kept once `steps` is large.
    seen: Option<rustc_hash::FxHashSet<(Value, Value)>>,
}

/// Steps after which `equal` starts tracking visited node pairs.
const EQUAL_TRACKING_AFTER: usize = 10_000;

impl EqualState {
    /// Note a visit to the node pair (a, b); false if it was seen before.
    fn first_visit(&mut self, a: Value, b: Value) -> bool {
        self.steps += 1;
        if self.steps > EQUAL_TRACKING_AFTER {
            let seen = self.seen.get_or_insert_with(Default::default);
            return seen.insert((a, b));
        }
        true
    }
}

/// `equal` with explicit state; loops on the cdr so long lists don't
/// recurse.
fn equal_with(mut a: Value, mut b: Value, st: &mut EqualState) -> bool {
    loop {
        if let (Some(p), Some(q)) = (a.as_pair(), b.as_pair()) {
            if !st.first_visit(a, b) {
                return true;
            }
            if !equal_with(p.car.get(), q.car.get(), st) {
                return false;
            }
            a = p.cdr.get();
            b = q.cdr.get();
            continue;
        }
        match (a.as_obj().map(|o| &o.kind), b.as_obj().map(|o| &o.kind)) {
            (Some(Obj::Vector(x)), Some(Obj::Vector(y))) => {
                if x.len() != y.len() {
                    return false;
                }
                if !st.first_visit(a, b) {
                    return true;
                }
                return x.iter().zip(y.iter()).all(|(p, q)| equal_with(p.get(), q.get(), st));
            }
            (Some(Obj::Str(x)), Some(Obj::Str(y))) => return *x.borrow() == *y.borrow(),
            (Some(Obj::Bytevector(x)), Some(Obj::Bytevector(y))) => return x == y,
            _ => return eqv(a, b),
        }
    }
}

/// The elements of a proper list, or None if `v` is not one.
pub fn list_to_vec(mut v: Value) -> Option<Vec<Value>> {
    let mut items = Vec::new();
    while let Some(p) = v.as_pair() {
        items.push(p.car.get());
        v = p.cdr.get();
    }
    v.is_nil().then_some(items)
}

/// Where objects live: pages of `PAGE_SLOTS` slots of one `GcObject`
/// each. A slot's `mark` says what it holds: an epoch for an object (live
/// if it is the current one), `FREE` for a slot on the free list, whose
/// `kind` is not initialised, or `POISONED` for an object poison mode
/// keeps, which is never swept again or reused. Pages are never moved or
/// returned to the system.
/// ponytail: memory stays at its high-water mark; give empty pages back if
/// that matters.
#[derive(Default)]
struct Slots {
    pages: Vec<NonNull<GcObject>>,
    free: Vec<NonNull<GcObject>>,
    /// Objects in use (poisoned ones not counted).
    count: usize,
}

const PAGE_SLOTS: usize = 1024;
const FREE: u64 = u64::MAX;
const POISONED: u64 = u64::MAX - 1;

impl Slots {
    fn page_layout() -> std::alloc::Layout {
        std::alloc::Layout::array::<GcObject>(PAGE_SLOTS).unwrap()
    }

    /// A slot holding a new object of kind `kind`, unmarked.
    fn alloc(&mut self, kind: Obj) -> NonNull<GcObject> {
        let p = match self.free.pop() {
            Some(p) => p,
            None => self.new_page(),
        };
        // SAFETY: a free slot is valid memory whose `kind` holds nothing
        // to drop.
        unsafe { p.as_ptr().write(GcObject { mark: Cell::new(0), kind }) };
        self.count += 1;
        p
    }

    /// Add a page, its slots free, and take its first.
    #[cold]
    fn new_page(&mut self) -> NonNull<GcObject> {
        let layout = Self::page_layout();
        // SAFETY: the layout isn't zero-sized.
        let base = NonNull::new(unsafe { std::alloc::alloc(layout) } as *mut GcObject).unwrap_or_else(|| std::alloc::handle_alloc_error(layout));
        self.pages.push(base);
        // Free in reverse, so the page is used from its start.
        for i in (0..PAGE_SLOTS).rev() {
            // SAFETY: slot i of the page; only its mark is written.
            let p = unsafe { base.add(i) };
            unsafe { std::ptr::addr_of_mut!((*p.as_ptr()).mark).write(Cell::new(FREE)) };
            self.free.push(p);
        }
        self.free.pop().unwrap()
    }

    /// Every slot of every page.
    fn all(&self) -> impl Iterator<Item = NonNull<GcObject>> + '_ {
        // SAFETY: each is a slot of an allocated page.
        self.pages.iter().flat_map(|&base| (0..PAGE_SLOTS).map(move |i| unsafe { base.add(i) }))
    }

    /// The objects in use.
    fn objects(&self) -> impl Iterator<Item = &GcObject> + '_ {
        // SAFETY: a slot whose mark is an epoch holds an object.
        self.all().filter(|p| unsafe { (*p.as_ptr()).mark.get() } < POISONED).map(|p| unsafe { &*p.as_ptr() })
    }

    /// Free every object not marked in `epoch`, or in poison mode turn it
    /// into the symbol `<<FREED>>` and keep it.
    fn sweep(&mut self, epoch: u64, poison: bool, #[cfg(feature = "gcstats")] st: &mut crate::gcstats::GcStats) {
        for i in 0..self.pages.len() {
            let base = self.pages[i];
            for j in 0..PAGE_SLOTS {
                // SAFETY: slot j of a page. A slot with an old epoch holds
                // an object unreachable from the roots, so nothing that
                // will be used again refers to it (a stray reference is
                // the bug poison mode exists to show).
                unsafe {
                    let p = base.add(j).as_ptr();
                    let mark = (*p).mark.get();
                    if mark == epoch || mark >= POISONED {
                        continue;
                    }
                    self.count -= 1;
                    #[cfg(feature = "gcstats")]
                    {
                        st.freed += 1;
                        st.freed_boxed += crate::gcstats::GcStats::boxed(&(*p).kind) as u64;
                    }
                    if poison {
                        (*p).kind = Obj::Symbol("<<FREED>>".into());
                        (*p).mark.set(POISONED);
                    } else {
                        std::ptr::drop_in_place(std::ptr::addr_of_mut!((*p).kind));
                        (*p).mark.set(FREE);
                        self.free.push(NonNull::new_unchecked(p));
                    }
                }
            }
        }
    }
}

impl Drop for Slots {
    fn drop(&mut self) {
        // Values outliving the heap dangle, which s3's single long-lived
        // heap never does.
        for p in self.all().collect::<Vec<_>>() {
            // SAFETY: an object in use, or poisoned, is dropped once.
            unsafe {
                if (*p.as_ptr()).mark.get() != FREE {
                    std::ptr::drop_in_place(std::ptr::addr_of_mut!((*p.as_ptr()).kind));
                }
            }
        }
        for &base in &self.pages {
            // SAFETY: allocated in new_page with this layout.
            unsafe { std::alloc::dealloc(base.as_ptr().cast(), Self::page_layout()) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::{FIXNUM_MAX, FIXNUM_MIN};

    #[test]
    fn integers_normalise() {
        let mut heap = Heap::new();
        assert_eq!(heap.integer(BigInt::from(FIXNUM_MAX)).as_fixnum(), Some(FIXNUM_MAX));
        assert_eq!(heap.integer(BigInt::from(FIXNUM_MIN)).as_fixnum(), Some(FIXNUM_MIN));
        let big = heap.integer(BigInt::from(FIXNUM_MAX) + 1);
        assert!(matches!(big.as_obj().map(|o| &o.kind), Some(Obj::BigInt(_))));
        let six = heap.rational(BigRational::new(BigInt::from(12), BigInt::from(2)));
        assert_eq!(six.as_fixnum(), Some(6));
    }

    #[test]
    fn symbols_are_interned() {
        let mut heap = Heap::new();
        let a = heap.intern("abc");
        let b = heap.intern("abc");
        let c = heap.intern("abd");
        assert!(a.eq(b));
        assert!(!a.eq(c));
    }

    #[test]
    fn collect_frees_only_the_unreachable() {
        let mut heap = Heap::new();
        let sym = heap.intern("s");
        let kept = heap.list(vec![sym, Value::fixnum(1).unwrap()]);
        let cycle = heap.cons(Value::NIL, Value::NIL);
        cycle.as_pair().unwrap().cdr.set(cycle);
        heap.string("garbage");
        heap.collect([kept]);
        assert_eq!(crate::printer::print_value(kept), "(s 1)");
        // Pairs: the slots of dead ones are reused, so making as many again
        // needs no new page.
        let churn = |heap: &mut Heap| {
            for i in 0..10_000 {
                heap.cons(Value::fixnum(i).unwrap(), Value::NIL);
            }
            heap.collect([kept]);
            heap.small.bytes()
        };
        let bytes = churn(&mut heap);
        // Poison mode never reuses a slot.
        if !heap.poison {
            assert_eq!((churn(&mut heap), churn(&mut heap)), (bytes, bytes));
        }
        // Empty pages are given back once a cycle allocates less.
        for i in 0..100_000 {
            heap.cons(Value::fixnum(i).unwrap(), Value::NIL);
        }
        heap.collect([kept]);
        let full = heap.small.bytes();
        heap.collect([kept]);
        assert!(heap.poison || heap.small.bytes() < full / 10, "{} of {} bytes kept", heap.small.bytes(), full);
        // Records of every size class, and one too large for any, which has
        // a page of its own: kept while reachable, freed when not.
        let rtype = heap.record_type(sym, vec![]).as_ref().unwrap();
        let records: Vec<Value> = [0, 1, 2, 30, 31, 40].iter().map(|&n| heap.record(rtype, (0..n).map(|i| Value::fixnum(i).unwrap()).collect())).collect();
        let all = heap.list(records);
        heap.collect([kept, all, Value::obj(rtype)]);
        let lens: Vec<usize> = list_to_vec(all).unwrap().iter().map(|r| r.as_record().unwrap().fields().len()).collect();
        assert_eq!(lens, [0, 1, 2, 30, 31, 40]);
        let last = list_to_vec(all).unwrap()[5].as_record().unwrap().fields()[39].get();
        assert_eq!(last.as_fixnum(), Some(39));
        let before = heap.small.bytes();
        heap.collect([kept]);
        assert!(heap.poison || heap.small.bytes() < before, "the large record's page is given back");
        // The symbol (a root), the two pairs of `kept`, and in poison mode
        // the symbol `<<FREED>>`; the cycle and the string are gone.
        assert_eq!(heap.len(), 3 + heap.poison as usize);
        assert_eq!(crate::printer::print_value(kept), "(s 1)");
    }
}
