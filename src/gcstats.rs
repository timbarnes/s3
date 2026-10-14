//! Collector statistics, with `--features gcstats` (plan/11, G0): what is
//! allocated, by kind and bytes; how long marking and sweeping take; how
//! many objects survive their first collection (the generational
//! hypothesis), and how much of each mark re-marks objects that had
//! survived before. Printed to standard error at exit. A separate feature
//! from `stats`, whose per-instruction counting would distort the times.

use std::time::{Duration, Instant};

use crate::heap::Obj;

/// More than the number of `Obj` variants.
const KINDS: usize = 32;

pub struct GcStats {
    start: Instant,
    /// By kind: its name, objects allocated, and bytes outside the slot.
    kinds: [(&'static str, u64, u64); KINDS],
    pub mark: Duration,
    pub sweep: Duration,
    /// Objects (not pairs) allocated since the collection before, at each
    /// collection, summed: what a minor collection would sweep.
    pub young: u64,
    /// Of those, the ones marked: survivors of their first collection.
    pub young_survived: u64,
    /// Objects marked that had survived a collection before: the marking a
    /// minor collection would skip.
    pub old_marked: u64,
    /// Objects freed, and how many of them had something boxed to free.
    pub freed: u64,
    pub freed_boxed: u64,
    /// Bytes in the slots of live `Obj`s, and in all the pages.
    pub peak_live: usize,
    pub peak_slots: usize,
}

impl Default for GcStats {
    fn default() -> GcStats {
        GcStats {
            start: Instant::now(),
            kinds: [("", 0, 0); KINDS],
            mark: Duration::ZERO,
            sweep: Duration::ZERO,
            young: 0,
            young_survived: 0,
            old_marked: 0,
            freed: 0,
            freed_boxed: 0,
            peak_live: 0,
            peak_slots: 0,
        }
    }
}

/// A kind's number and name, and the bytes it holds outside its slot.
fn describe(kind: &Obj) -> (usize, &'static str, u64) {
    use std::mem::size_of;
    let (i, name, bytes) = match kind {
        Obj::Symbol(s) => (1, "symbol", s.len()),
        Obj::Str(s) => (2, "string", s.borrow().as_str().len()),
        Obj::Vector(v) => (3, "vector", v.len() * 8),
        Obj::Bytevector(v) => (4, "bytevector", v.len()),
        Obj::BigInt(n) => (5, "bignum", n.bits().div_ceil(8) as usize),
        Obj::Rational(r) => (6, "ratio", size_of::<num_rational::BigRational>() + (r.numer().bits() + r.denom().bits()).div_ceil(8) as usize),
        Obj::Global { .. } => (7, "global", 0),
        Obj::Alias { .. } => (8, "alias", 0),
        Obj::Environment(_) => (9, "environment", size_of::<crate::globals::Env>()),
        Obj::Macro(_) => (10, "macro", size_of::<crate::compile::syntax_rules::SyntaxRules>()),
        Obj::Box(_) => (11, "box", 0),
        Obj::Port(_) => (12, "port", size_of::<crate::io::PortKind>()),
        Obj::RecordType { fields, .. } => (13, "record type", fields.len() * 8),
        Obj::Continuation { stack, frames } => (15, "continuation", stack.len() * 8 + frames.len() * size_of::<crate::vm::Frame>()),
        Obj::Escape { .. } => (16, "escape", 0),
        Obj::Values(v) => (17, "values", v.len() * 8),
        Obj::Error { .. } => (18, "error", 0),
        Obj::Primitive(_) => (19, "primitive", 0),
        Obj::Template(t) => (20, "template", size_of::<crate::vm::Template>() + t.code.len() * 8 + t.consts.len() * 8),
    };
    (i, name, bytes as u64)
}

impl GcStats {
    pub fn alloc_pair(&mut self) {
        let k = &mut self.kinds[0];
        *k = ("pair", k.1 + 1, 0);
    }

    /// A closure (kind 1 here) or record (2) of `fields`, all in its slot.
    pub fn alloc_compact(&mut self, i: usize, name: &'static str, fields: usize) {
        let k = &mut self.kinds[KINDS - i];
        *k = (name, k.1 + 1, k.2 + (8 + 8 * fields as u64).div_ceil(16) * 16);
    }

    pub fn alloc(&mut self, kind: &Obj) {
        let (i, name, bytes) = describe(kind);
        let k = &mut self.kinds[i];
        *k = (name, k.1 + 1, k.2 + bytes);
    }

    /// Whether freeing `kind` frees a box too.
    pub fn boxed(kind: &Obj) -> bool {
        describe(kind).2 > 0
    }

    /// `slot` is an object slot's size; a pair's is 16.
    pub fn report(&self, collections: usize, marked: usize, slot: usize) {
        let run = self.start.elapsed().as_secs_f64();
        let pct = |a: f64, b: f64| if b > 0.0 { 100.0 * a / b } else { 0.0 };
        let (mark, sweep) = (self.mark.as_secs_f64(), self.sweep.as_secs_f64());
        let objects: u64 = self.kinds.iter().map(|k| k.1).sum();
        eprintln!("gc: {} collections in {:.3}s: mark {:.3}s ({:.1}%), sweep {:.3}s ({:.1}%)", collections, run, mark, pct(mark, run), sweep, pct(sweep, run));
        eprintln!("gc: {} objects allocated; {} marked in all", objects, marked);
        eprintln!(
            "gc: objects (not pairs) young at a collection {}, survived it {} ({:.1}%); old objects re-marked {}",
            self.young,
            self.young_survived,
            pct(self.young_survived as f64, self.young as f64),
            self.old_marked,
        );
        eprintln!("gc: freed {}, {} with a box ({:.1}%)", self.freed, self.freed_boxed, pct(self.freed_boxed as f64, self.freed as f64));
        let mb = |n: f64| n / (1 << 20) as f64;
        eprintln!("gc: peak live {:.1}MB in Obj slots; peak pages {:.1}MB", mb(self.peak_live as f64), mb(self.peak_slots as f64));
        let mut rows: Vec<_> = self.kinds.iter().filter(|k| k.1 > 0).collect();
        rows.sort_by(|a, b| b.1.cmp(&a.1));
        eprintln!("gc: allocations by kind (objects, % of objects, MB in Obj slots, MB elsewhere: boxes, or compact slots for closures and records):");
        for (name, n, bytes) in rows {
            let size = match *name {
                "pair" => 16,
                "closure" | "record" => 0,
                _ => slot,
            };
            eprintln!("{:>14} {:5.1}% {:9.1} {:9.1}  {}", n, pct(*n as f64, objects as f64), mb((*n as usize * size) as f64), mb(*bytes as f64), name);
        }
    }
}
