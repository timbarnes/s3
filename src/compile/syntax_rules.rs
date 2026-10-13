//! Identifiers, aliases and `syntax-rules` (R7RS §4.3.2), forked from s1's
//! `syntax_rules.rs` and `eval/identifiers.rs`.
//!
//! An *identifier* is a symbol or an alias (`Obj::Alias`). Expanding a
//! macro use renames every identifier its template introduces to a fresh
//! alias, which records the identifier it renames and the macro's
//! definition scope. The expander (`scheme/compiler/expand.scm`) resolves aliases; this file
//! only matches and instantiates, which runs no Scheme code and needs no
//! scopes, apart from comparing literals, which the caller supplies.
//!
//! Differences from s1: values are `Value`s, the definition environment is
//! a scope depth instead of an `EnvRef`, and literal data in an expansion
//! isn't stripped of aliases here: the expander does it when it meets
//! `quote`, `quasiquote` or a vector constant.

use rustc_hash::FxHashSet;

use crate::heap::{GcRef, Heap, Obj};
use crate::value::Value;

// ---------------------------------------------------------------------------
// Identifiers
// ---------------------------------------------------------------------------

/// `v` as an identifier: a symbol or an alias.
pub fn identifier(v: Value) -> Option<GcRef> {
    matches!(kind(v), Some(Obj::Symbol(_) | Obj::Alias { .. })).then(|| v.as_ref().unwrap())
}

/// The symbol an identifier ultimately renames (itself, for a symbol).
pub fn strip(id: GcRef) -> GcRef {
    let mut id = id;
    while let Obj::Alias { original, .. } = &id.get().kind {
        id = original.as_ref().unwrap();
    }
    id
}

/// The name of the symbol an identifier renames.
pub fn name<'a>(id: GcRef) -> &'a str {
    match &strip(id).get().kind {
        Obj::Symbol(s) => s,
        _ => unreachable!("not an identifier"),
    }
}

fn kind<'a>(v: Value) -> Option<&'a Obj> {
    v.as_obj().map(|o| &o.kind)
}

/// `datum` with every alias in it replaced by its symbol: what a quoted
/// template must yield. Returns `datum` itself, allocating nothing, when it
/// contains no aliases. Cyclic data (from datum labels) is returned
/// unchanged.
pub fn strip_datum(heap: &mut Heap, datum: Value) -> Value {
    let mut seen = FxHashSet::default();
    let mut cyclic = false;
    if !contains_alias(datum, &mut seen, &mut cyclic) || cyclic {
        return datum;
    }
    copy_stripped(heap, datum)
}

fn contains_alias(datum: Value, seen: &mut FxHashSet<GcRef>, cyclic: &mut bool) -> bool {
    match kind(datum) {
        Some(Obj::Alias { .. }) => true,
        Some(Obj::Pair { .. } | Obj::Vector(_)) if !seen.insert(datum.as_ref().unwrap()) => {
            *cyclic = true;
            false
        }
        // Bitwise `|` so both sides are always walked (for `cyclic`).
        Some(Obj::Pair { car, cdr }) => contains_alias(car.get(), seen, cyclic) | contains_alias(cdr.get(), seen, cyclic),
        Some(Obj::Vector(items)) => items.iter().fold(false, |any, c| any | contains_alias(c.get(), seen, cyclic)),
        _ => false,
    }
}

fn copy_stripped(heap: &mut Heap, datum: Value) -> Value {
    match kind(datum) {
        Some(Obj::Alias { .. }) => Value::obj(strip(datum.as_ref().unwrap())),
        Some(Obj::Pair { .. }) => {
            let (items, tail) = list_parts(datum);
            let mut result = copy_stripped(heap, tail);
            for item in items.into_iter().rev() {
                let item = copy_stripped(heap, item);
                result = heap.cons(item, result);
            }
            result
        }
        Some(Obj::Vector(items)) => {
            let items: Vec<Value> = items.iter().map(|c| c.get()).collect();
            let copied = items.into_iter().map(|i| copy_stripped(heap, i)).collect();
            heap.vector(copied)
        }
        _ => datum,
    }
}

/// The elements of a (possibly improper) list, and its final cdr.
pub fn list_parts(list: Value) -> (Vec<Value>, Value) {
    let mut items = Vec::new();
    let mut rest = list;
    while let Some(Obj::Pair { car, cdr }) = kind(rest) {
        items.push(car.get());
        rest = cdr.get();
    }
    (items, rest)
}

// ---------------------------------------------------------------------------
// Transformers
// ---------------------------------------------------------------------------

/// A transformer: a parsed `syntax-rules` form.
pub struct SyntaxRules {
    /// The custom ellipsis identifier, if one was given.
    pub ellipsis: Option<GcRef>,
    /// The literal identifiers, matched by identity.
    pub literals: Vec<GcRef>,
    /// (pattern, template) pairs, in order.
    pub rules: Vec<(Value, Value)>,
    /// The definition scope: how many of the expander's frames were
    /// visible where the macro was defined (0 at top level), over the
    /// top-level environment `env`. The aliases an expansion makes resolve
    /// there.
    pub depth: u32,
    pub env: GcRef,
}

// ---------------------------------------------------------------------------
// Parsing and validation
// ---------------------------------------------------------------------------
