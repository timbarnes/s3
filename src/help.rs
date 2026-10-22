//! `help` and `apropos` (plan/12, T3).
//!
//! The help for the standard procedures and syntax is s3's documentation,
//! which is built in and read on the first `help`. A procedure or form is
//! documented by a line that starts with its signature, either a bullet,
//!
//! ```text
//! * `(memq obj list)`, `(memv obj list)`, `(member obj list [compare])`: the first ...
//! ```
//!
//! or a paragraph, as the syntax is (`` `(cond clause ...)` ``). Every
//! signature such a line starts with documents its name with the whole
//! line, and a paragraph of signatures alone with the paragraph after it
//! too. A name that no line starts with is documented by the first line
//! that shows it later on, as `(or test ...)` is in the paragraph about
//! `and`. Fix a gap in the documentation, not here: a test checks that
//! every name the standard libraries export has help.

use std::sync::OnceLock;

use rustc_hash::FxHashMap;

use crate::builtins::{Error, Primitive, kind, prim};
use crate::globals::{TopBinding, cell_value, env_of};
use crate::heap::{GcRef, Heap, Obj};
use crate::value::Value;
use crate::vm::Template;

/// Where the documentation is published.
const SITE: &str = "https://timbarnes.github.io/s3/";

/// The documentation's pages: each one's name and its text.
const PAGES: &[(&str, &str)] = &[
    ("booleans", include_str!("../docs/booleans.md")),
    ("bytevectors", include_str!("../docs/bytevectors.md")),
    ("characters", include_str!("../docs/characters.md")),
    ("control-features", include_str!("../docs/control-features.md")),
    ("derived-expressions", include_str!("../docs/derived-expressions.md")),
    ("environments", include_str!("../docs/environments.md")),
    ("equivalence-predicates", include_str!("../docs/equivalence-predicates.md")),
    ("exceptions", include_str!("../docs/exceptions.md")),
    ("extensions", include_str!("../docs/extensions.md")),
    ("input-and-output", include_str!("../docs/input-and-output.md")),
    ("lexical-syntax", include_str!("../docs/lexical-syntax.md")),
    ("libraries", include_str!("../docs/libraries.md")),
    ("macros", include_str!("../docs/macros.md")),
    ("numbers", include_str!("../docs/numbers.md")),
    ("pairs-and-lists", include_str!("../docs/pairs-and-lists.md")),
    ("primitive-expressions", include_str!("../docs/primitive-expressions.md")),
    ("program-structure", include_str!("../docs/program-structure.md")),
    ("records", include_str!("../docs/records.md")),
    ("strings", include_str!("../docs/strings.md")),
    ("symbols", include_str!("../docs/symbols.md")),
    ("system-interface", include_str!("../docs/system-interface.md")),
    ("vectors", include_str!("../docs/vectors.md")),
];

/// The documentation of one name.
struct Entry {
    /// The line that documents it, without its Markdown.
    text: String,
    /// The page's title and the heading the line is under, and the URL
    /// of that section.
    place: String,
    url: String,
    /// Whether the line starts with the name's signature, rather than only
    /// showing it.
    leading: bool,
}

/// Every documented name's entry.
fn entries() -> &'static FxHashMap<String, Entry> {
    static ENTRIES: OnceLock<FxHashMap<String, Entry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        let mut entries = FxHashMap::default();
        for &(page, text) in PAGES {
            read_page(page, text, &mut entries);
        }
        entries
    })
}

fn read_page(page: &str, text: &str, entries: &mut FxHashMap<String, Entry>) {
    let mut title = page.to_string();
    let mut section: Option<String> = None;
    let lines: Vec<&str> = text.lines().collect();
    for (i, &line) in lines.iter().enumerate() {
        if let Some(h) = line.strip_prefix("# ") {
            title = plain(h);
            continue;
        }
        if let Some(h) = line.strip_prefix("## ").or_else(|| line.strip_prefix("### ")) {
            section = Some(h.to_string());
            continue;
        }
        let body = line.strip_prefix("* ").unwrap_or(line);
        if !body.starts_with("`(") {
            continue;
        }
        // A paragraph of signatures alone: the next one explains them.
        let mut body = body.to_string();
        if !line.starts_with("* ")
            && line.ends_with('`')
            && let Some(next) = lines[i + 1..].iter().find(|l| !l.trim().is_empty())
            && !next.starts_with(['#', '*', '`', '|', '>'])
        {
            body.push(' ');
            body.push_str(next);
        }
        let (leading, shown) = signatures(&body);
        let body = plain(&body);
        let (place, url) = match &section {
            Some(h) => (format!("{}, {}", title, plain(h)), format!("{}{}/#{}", SITE, page, slug(h))),
            None => (title.clone(), format!("{}{}/", SITE, page)),
        };
        let entry = |leading| Entry { text: body.clone(), place: place.clone(), url: url.clone(), leading };
        for name in leading {
            if entries.get(&name).is_none_or(|e: &Entry| !e.leading) {
                entries.insert(name, entry(true));
            }
        }
        for name in shown {
            entries.entry(name).or_insert_with(|| entry(false));
        }
    }
}

/// The names of the signatures `line` starts with (`(name ...)` code
/// spans, separated by commas), and of those it shows after them.
fn signatures(line: &str) -> (Vec<String>, Vec<String>) {
    let mut leading = Vec::new();
    let mut rest = line;
    while let Some(span) = rest.strip_prefix('`').and_then(|r| r.split_once('`')) {
        match signature_name(span.0) {
            Some(name) => leading.push(name),
            None => break,
        }
        match span.1.strip_prefix(", ") {
            Some(r) => rest = r,
            None => {
                rest = span.1;
                break;
            }
        }
    }
    let shown = code_spans(rest).into_iter().filter_map(signature_name).collect();
    (leading, shown)
}

/// The code spans in `text`: `code`, or ``code`` when it holds a backtick.
fn code_spans(text: &str) -> Vec<&str> {
    let mut spans = Vec::new();
    let mut rest = text;
    while let Some(i) = rest.find('`') {
        let fence = if rest[i..].starts_with("``") { "``" } else { "`" };
        let inside = &rest[i + fence.len()..];
        let Some(end) = inside.find(fence) else { break };
        spans.push(inside[..end].trim());
        rest = &inside[end + fence.len()..];
    }
    spans
}

/// The name a code span such as `(member obj list [compare])` is the
/// signature of, if it is one.
fn signature_name(span: &str) -> Option<String> {
    let name = span.strip_prefix('(')?.split([' ', ')']).next()?;
    let first = name.chars().next()?;
    let identifier = first.is_alphabetic() || ("!$%&*/:<=>?^_~+-".contains(first) && name.parse::<f64>().is_err());
    identifier.then(|| name.to_string())
}

/// `markdown` without its code quotes, bold and link targets.
fn plain(markdown: &str) -> String {
    let mut out = String::new();
    let mut rest = markdown;
    while let Some(i) = rest.find(['`', '[', '*']) {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        if rest.starts_with('`') {
            // A code span is its text.
            let fence = if rest.starts_with("``") { "``" } else { "`" };
            match rest[fence.len()..].split_once(fence) {
                Some((code, after)) => {
                    out.push_str(code.trim());
                    rest = after;
                }
                None => {
                    out.push_str(rest);
                    rest = "";
                }
            }
        } else if let Some(after) = rest.strip_prefix("**") {
            rest = after;
        } else if let Some((label, after)) = link(rest) {
            out.push_str(label);
            rest = after;
        } else {
            out.push_str(&rest[..1]);
            rest = &rest[1..];
        }
    }
    out.push_str(rest);
    out
}

/// A link at the start of `text`, `[label](target)`: its label, and the
/// text after it.
fn link(text: &str) -> Option<(&str, &str)> {
    let (label, target) = text.strip_prefix('[')?.split_once("](")?;
    Some((label, target.split_once(')')?.1))
}

/// A heading's anchor, as MkDocs makes it (Python-Markdown's `slugify`):
/// lower case, without punctuation, with runs of spaces and hyphens as one
/// hyphen.
fn slug(heading: &str) -> String {
    let kept: String = heading.chars().filter(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | ' ')).collect::<String>().to_lowercase();
    kept.split([' ', '-']).filter(|w| !w.is_empty()).collect::<Vec<_>>().join("-")
}

/// Auxiliary syntax, which has no signature of its own: each keyword, and
/// the form whose documentation explains it.
const AUXILIARY: &[(&str, &str)] = &[("else", "cond"), ("=>", "cond"), ("_", "syntax-rules"), ("...", "syntax-rules")];

/// The help for `name` from the documentation, if it has any.
pub(crate) fn documented(name: &str) -> Option<String> {
    if let Some(&(_, form)) = AUXILIARY.iter().find(|&&(k, _)| k == name) {
        return documented(form).map(|text| format!("{} is auxiliary syntax of {}.\n{}", name, form, text));
    }
    entries().get(name).map(|e| format!("{}\n  {}: {}", e.text, e.place, e.url))
}

/// The help for a procedure compiled from `t`, called `name`: its docstring
/// if it has one, else the documentation's, else its signature alone.
fn procedure_help(t: &Template, name: &str, heap: &Heap) -> String {
    if t.doc.is_none()
        && let Some(text) = documented(name)
    {
        return text;
    }
    let required = &t.local_names[..t.required as usize];
    let mut signature = format!("({}", name);
    for param in required {
        signature.push(' ');
        signature.push_str(param);
    }
    if t.rest {
        let rest = t.local_names.get(t.required as usize).map_or("rest", |r| r.as_str());
        signature.push_str(" . ");
        signature.push_str(rest);
    }
    signature.push(')');
    let mut text = signature;
    if let Some(doc) = &t.doc {
        text.push_str("\n  ");
        text.push_str(doc);
    }
    if let Some(place) = t.position.and_then(|p| heap.sources.describe(p)) {
        text.push_str(&format!("\n  Defined at {}.", place));
    }
    text
}

/// The template of the closure `v`, if it is one.
fn template(v: Value) -> Option<&'static Template> {
    match &v.as_closure()?.link().get().kind {
        Obj::Template(t) => Some(t),
        _ => None,
    }
}

/// `(help x)`'s text: for a name, what it is bound to in the interaction
/// environment; for a procedure, the procedure.
fn help_text(heap: &Heap, x: Value) -> String {
    let value_help = |v: Value, name: &str| match (template(v), kind(v)) {
        (Some(t), _) => procedure_help(t, name, heap),
        (None, Some(Obj::Primitive(p))) => documented(p.name).unwrap_or_else(|| format!("{}: a built-in procedure, not documented.", p.name)),
        _ => documented(name).unwrap_or_else(|| format!("{}: not documented.", name)),
    };
    match kind(x) {
        Some(Obj::Symbol(name)) => {
            let binding = heap.interaction.and_then(|env| env_of(env).borrow().lookup(x.as_ref().unwrap()));
            match binding {
                Some(TopBinding::Variable(cell)) if !cell_value(cell).get().is_undefined() => value_help(cell_value(cell).get(), name),
                _ => documented(name).unwrap_or_else(|| format!("{}: not documented, and not defined.", name)),
            }
        }
        _ if template(x).is_some() => {
            let t = template(x).unwrap();
            procedure_help(t, if t.name.is_empty() { "lambda" } else { &t.name }, heap)
        }
        Some(Obj::Primitive(_)) => value_help(x, ""),
        _ => format!("help: give a name, such as 'car, or a procedure, not {}", crate::printer::print_value(x)),
    }
}

/// The names in the interaction environment that contain `text`, in
/// order, but for the hidden ones that start with `%`.
fn apropos(heap: &mut Heap, text: &str) -> Value {
    let Some(env) = heap.interaction else { return Value::NIL };
    let mut names: Vec<(String, GcRef)> = env_of(env)
        .borrow()
        .bindings()
        .filter_map(|(id, _)| match kind(Value::obj(id)) {
            Some(Obj::Symbol(name)) if name.contains(text) && !name.starts_with('%') => Some((name.to_string(), id)),
            _ => None,
        })
        .collect();
    names.sort_by(|a, b| a.0.cmp(&b.0));
    heap.list(names.into_iter().map(|(_, id)| Value::obj(id)).collect())
}

pub static PRIMITIVES: &[Primitive] = &[
    prim("%help-text", 1, Some(1), |h, a| {
        let text = help_text(h, a[0]);
        Ok(h.string(&text))
    }),
    prim("%apropos", 1, Some(1), |h, a| match kind(a[0]) {
        Some(Obj::Str(s)) => {
            let text = s.borrow().to_string();
            Ok(apropos(h, &text))
        }
        _ => Err(Error::from(crate::builtins::expected("apropos", "a string", a[0]))),
    }),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signatures_and_markdown() {
        assert_eq!(signatures("`(memq obj list)`, `(memv obj list)`: compare with `(eq? a b)`."), (vec!["memq".into(), "memv".into()], vec!["eq?".into()]));
        assert_eq!(signatures("`(quote datum)`, or `'datum`"), (vec!["quote".into()], vec![]));
        assert_eq!(signature_name("(1 2 3)"), None);
        assert_eq!(code_spans("or `` `template `` with `,` (`(unquote expr)`)"), ["`template", ",", "(unquote expr)"]);
        assert_eq!(plain("`(let* ((v i)) b)`: see [Records](./records.md), **not** `x`"), "(let* ((v i)) b): see Records, not x");
        assert_eq!(plain("`(quasiquote template)`, or `` `template ``, a * b"), "(quasiquote template), or `template, a * b");
        assert_eq!(slug("`let`, `let*`"), "let-let");
        assert_eq!(slug("Named `let`"), "named-let");
    }

    #[test]
    fn documented_names() {
        let member = documented("member").unwrap();
        assert!(member.starts_with("(memq obj list), (memv obj list), (member obj list [compare]): the first sublist"), "{}", member);
        assert!(member.ends_with("Pairs and Lists, Searching: https://timbarnes.github.io/s3/pairs-and-lists/#searching"), "{}", member);
        // A name a paragraph shows after another's signature.
        assert!(documented("or").unwrap().starts_with("(and test ...) evaluates"));
        assert!(documented("cond").unwrap().starts_with("(cond clause ...)"));
    }
}
