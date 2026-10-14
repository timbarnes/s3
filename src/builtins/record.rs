//! Records (R7RS §5.5): the primitives `define-record-type` (in
//! `scheme/core.scm`) expands into. Forked from s1's `builtin/record.rs`.
//!
//! An accessor finds its field's index once, when the record type is
//! defined (`%record-index`), and then reads by index (`%record-ref`). s1
//! looked the field up by name on every call; P10 measured that at about
//! 5% of the typical program.

use super::{Error, err, Primitive, expected, kind, list_arg, prim};
use crate::heap::{Heap, Obj};
use crate::printer::print_value;
use crate::value::Value;

pub static PRIMITIVES: &[Primitive] = &[
    prim("%make-record-type", 2, Some(2), make_record_type),
    prim("%record-indexes", 2, Some(2), record_indexes),
    prim("%record-new", 2, None, record_new),
    prim("%record?", 2, Some(2), |_, a| {
        Ok(Value::bool(matches!(kind(a[0]), Some(Obj::Record { rtype, .. }) if Value::obj(*rtype).eq(a[1]))))
    }),
    prim("%record-index", 2, Some(2), |_, a| Ok(super::fixnum(field_index(a[0], a[1], "define-record-type")?))),
    prim("%record-ref", 3, Some(3), record_ref),
    prim("%record-set-index!", 4, Some(4), record_set_index),
];

/// The field `args[2]` (an index from `%record-index`) of the record
/// `args[0]`, which must be of type `args[1]`. The define-record-type
/// macro passes a right index, but the procedure can be called directly.
fn field<'a>(args: &[Value], who: &str) -> Result<&'a std::cell::Cell<Value>, Error> {
    let fields = record_fields(args[0], args[1], who)?;
    args[2].as_fixnum().and_then(|i| fields.get(usize::try_from(i).ok()?)).ok_or_else(|| err!("{}: bad field index {}", who, print_value(args[2])))
}

/// The name and field names of a record type.
fn record_type<'a>(v: Value, who: &str) -> Result<(Value, &'a [Value]), Error> {
    match kind(v) {
        Some(Obj::RecordType { name, fields }) => Ok((*name, fields)),
        _ => Err(expected(who, "a record type", v)),
    }
}

fn field_index(rtype: Value, field: Value, who: &str) -> Result<usize, Error> {
    let (name, fields) = record_type(rtype, who)?;
    fields
        .iter()
        .position(|f| f.eq(field))
        .ok_or_else(|| err!("{}: {} has no field {}", who, print_value(name), print_value(field)))
}

/// `(%record-indexes type field-names)`: the fields' indexes, a vector, for
/// a constructor to fill in (`%record-new`). Found once, when the record
/// type is defined, as an accessor's index is.
fn record_indexes(heap: &mut Heap, args: &[Value]) -> Result<Value, Error> {
    let indexes = list_arg(args[1], "define-record-type")?
        .into_iter()
        .map(|name| Ok(super::fixnum(field_index(args[0], name, "define-record-type")?)))
        .collect::<Result<Vec<_>, Error>>()?;
    Ok(heap.vector(indexes))
}

/// `(%record-new type indexes value ...)`: a record of `type` whose fields
/// at `indexes` (from `%record-indexes`) hold the values, in order; the
/// others are unspecified. What a constructor does.
fn record_new(heap: &mut Heap, args: &[Value]) -> Result<Value, Error> {
    let n = record_type(args[0], "record constructor")?.1.len();
    let Some(Obj::Vector(indexes)) = kind(args[1]) else {
        return Err(err!("record constructor: bad field indexes {}", print_value(args[1])));
    };
    let values = &args[2..];
    if indexes.len() != values.len() {
        return Err(err!("record constructor: wrong number of arguments: {}", values.len()));
    }
    let mut fields = vec![Value::UNSPECIFIED; n];
    for (i, &v) in indexes.iter().zip(values) {
        let i = i.get().as_fixnum().and_then(|i| usize::try_from(i).ok()).filter(|&i| i < n);
        fields[i.ok_or_else(|| err!("record constructor: bad field indexes {}", print_value(args[1])))?] = v;
    }
    Ok(heap.record(args[0].as_ref().unwrap(), fields))
}

/// `(%record-ref obj type index)`: what an accessor does (`Op::RecordRef`).
pub fn record_ref(_: &mut Heap, a: &[Value]) -> Result<Value, Error> {
    Ok(field(a, "record accessor")?.get())
}

/// `(%record-set-index! obj type index value)`: what a modifier does
/// (`Op::RecordSet`).
pub fn record_set_index(_: &mut Heap, a: &[Value]) -> Result<Value, Error> {
    field(a, "record modifier")?.set(a[3]);
    Ok(Value::UNSPECIFIED)
}

/// The fields of `obj`, which must be a record of type `rtype`.
fn record_fields<'a>(obj: Value, rtype: Value, who: &str) -> Result<&'a [std::cell::Cell<Value>], Error> {
    match kind(obj) {
        Some(Obj::Record { rtype: t, fields }) if Value::obj(*t).eq(rtype) => Ok(fields),
        _ => Err(err!("{}: expected a {} record, got {}", who, print_value(record_type(rtype, who)?.0), print_value(obj))),
    }
}

/// `(%make-record-type name field-specs)`: each spec is `(field ...)` or a
/// field name.
fn make_record_type(heap: &mut Heap, args: &[Value]) -> Result<Value, Error> {
    let mut fields: Vec<Value> = Vec::new();
    for spec in list_arg(args[1], "%make-record-type")? {
        let name = match kind(spec) {
            Some(Obj::Pair { car, .. }) => car.get(),
            _ => spec,
        };
        if !matches!(kind(name), Some(Obj::Symbol(_))) {
            return Err(err!("define-record-type: bad field spec {}", print_value(spec)));
        }
        if fields.iter().any(|f| f.eq(name)) {
            return Err(err!("define-record-type: duplicate field {}", print_value(name)));
        }
        fields.push(name);
    }
    Ok(heap.record_type(args[0], fields))
}

