//! The verifier (plan/09): checks a template before the VM can run it.
//! The dispatch loop reads stack slots unchecked in release builds, so
//! code with a wrong slot would be a memory error, not a panic. Every
//! template made from data (`%make-template`) or read from a compiled
//! file passes through here.
//!
//! Besides each operand (constants of the right kind, jump targets, free
//! variables), it follows the stack depth through the code, relative to
//! the frame's first parameter: every local an instruction names must be
//! below it, no instruction may pop past the frame, each instruction must
//! be reached at one depth only, and the code mustn't run off its end.

use crate::heap::Obj;
use crate::vm::op::{Op, Template};

/// Check `t`, or say what's wrong with it.
pub fn verify(t: &Template) -> Result<(), String> {
    let at = |ip: usize, why: &str| format!("{:?} at {} in {}: {}", t.code[ip], ip, t.name, why);
    let kind = |k: u32| t.consts.get(k as usize).and_then(|c| c.as_obj()).map(|o| &o.kind);
    if t.free_names.len() != t.nfree as usize {
        return Err(format!("{}: {} free variables, but {} names", t.name, t.nfree, t.free_names.len()));
    }
    if t.code.is_empty() {
        return Err(format!("{}: no code", t.name));
    }
    let params = t.required as usize + t.rest as usize;
    let mut depth: Vec<Option<usize>> = vec![None; t.code.len()];
    let mut work = vec![(0, params)];
    while let Some((ip, d)) = work.pop() {
        match depth[ip] {
            Some(seen) if seen == d => continue,
            Some(seen) => return Err(at(ip, &format!("reached with {} values on the stack, and with {}", seen, d))),
            None => depth[ip] = Some(d),
        }
        let op = t.code[ip];
        let local = |i: u16, d: usize| if (i as usize) < d { Ok(()) } else { Err(at(ip, "no such local")) };
        let target = |j: u32| if (j as usize) < t.code.len() { Ok(j as usize) } else { Err(at(ip, "jump past the end")) };
        // (values popped, values pushed, where a jump may go, whether it
        // can go on to the next instruction).
        let (pop, push, jump, next) = match op {
            Op::Const(k) => {
                if t.consts.get(k as usize).is_none() {
                    return Err(at(ip, "no such constant"));
                }
                (0, 1, None, true)
            }
            Op::Fixnum(_) | Op::Nil | Op::True | Op::False | Op::Unspecified | Op::Undefined => (0, 1, None, true),
            Op::LocalRef(i) | Op::CarLocal(i) | Op::CdrLocal(i) => {
                local(i, d)?;
                (0, 1, None, true)
            }
            Op::LocalSet(i) => {
                local(i, d.saturating_sub(1))?;
                (1, 0, None, true)
            }
            Op::FreeRef(i) => {
                if i >= t.nfree {
                    return Err(at(ip, "no such free variable"));
                }
                (0, 1, None, true)
            }
            Op::PatchFree(c, _, v) => {
                local(c, d)?;
                local(v, d)?;
                (0, 0, None, true)
            }
            Op::MakeBox(i) => {
                local(i, d)?;
                (0, 0, None, true)
            }
            Op::BoxRef => (1, 1, None, true),
            Op::BoxSet => (2, 0, None, true),
            Op::GlobalRef(k) | Op::GlobalSet(k) | Op::GlobalDefine(k) => {
                if !matches!(kind(k), Some(Obj::Global { .. })) {
                    return Err(at(ip, "not a global variable's cell"));
                }
                match op {
                    Op::GlobalRef(_) => (0, 1, None, true),
                    Op::GlobalSet(_) => (1, 0, None, true),
                    _ => (1, 1, None, true),
                }
            }
            Op::Jump(j) => (0, 0, Some(target(j)?), false),
            Op::JumpIfFalse(j) => (1, 0, Some(target(j)?), true),
            Op::JumpIfNot(test, j) => (test.arity(), 0, Some(target(j)?), true),
            Op::JumpIfNotImm(_, _, j) => (1, 0, Some(target(j)?), true),
            Op::Pop => (1, 0, None, true),
            Op::Slide(n) => (n as usize + 1, 1, None, true),
            Op::MakeClosure(k) => match kind(k) {
                Some(Obj::Template(inner)) => (inner.nfree as usize, 1, None, true),
                _ => return Err(at(ip, "not a template")),
            },
            Op::Call(n) => (n as usize + 1, 1, None, true),
            Op::TailCall(n) => (n as usize + 1, 0, None, false),
            Op::Return => (1, 0, None, false),
            Op::SelfTailCall(n) => {
                if n as usize != params {
                    return Err(at(ip, "not as many arguments as parameters"));
                }
                (n as usize, 0, None, false)
            }
            Op::Car | Op::Cdr | Op::NullP | Op::PairP | Op::Not | Op::VectorLength | Op::AddImm(_) | Op::SubImm(_) => {
                (1, 1, None, true)
            }
            Op::Cons | Op::EqP | Op::Add | Op::Sub | Op::Mul | Op::Div => (2, 1, None, true),
            Op::NumEq | Op::Lt | Op::Gt | Op::Le | Op::Ge | Op::VectorRef => (2, 1, None, true),
            Op::VectorSet => (3, 1, None, true),
        };
        if pop > d {
            return Err(at(ip, &format!("pops {} of {} values", pop, d)));
        }
        let after = d - pop + push;
        if let Some(j) = jump {
            work.push((j, after));
        }
        if next {
            if ip + 1 == t.code.len() {
                return Err(at(ip, "runs off the end of the code"));
            }
            work.push((ip + 1, after));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn template(required: u16, code: Vec<Op>) -> Template {
        Template { name: "t".into(), required, rest: false, nfree: 0, code, consts: Vec::new(), local_names: Vec::new(), free_names: Vec::new() }
    }

    #[test]
    fn checks_the_stack() {
        assert_eq!(verify(&template(1, vec![Op::LocalRef(0), Op::Return])), Ok(()));
        // (if x 1 2) in tail position, and a loop.
        assert_eq!(verify(&template(1, vec![Op::LocalRef(0), Op::JumpIfFalse(4), Op::Fixnum(1), Op::Return, Op::Fixnum(2), Op::Return])), Ok(()));
        assert_eq!(verify(&template(1, vec![Op::LocalRef(0), Op::SubImm(1), Op::SelfTailCall(1)])), Ok(()));
        let bad = |required, code| verify(&template(required, code)).unwrap_err();
        assert!(bad(1, vec![Op::LocalRef(1), Op::Return]).contains("no such local"));
        assert!(bad(0, vec![Op::Pop, Op::Nil, Op::Return]).contains("pops 1 of 0"));
        assert!(bad(0, vec![Op::Nil]).contains("runs off the end"));
        assert!(bad(0, vec![Op::Nil, Op::Jump(0)]).contains("reached with 0 values on the stack, and with 1"));
        assert!(bad(0, vec![Op::Jump(5)]).contains("jump past the end"));
        assert!(bad(1, vec![Op::SelfTailCall(0)]).contains("not as many arguments"));
        // A let's variable is a local while it's on the stack.
        assert_eq!(verify(&template(0, vec![Op::Nil, Op::LocalRef(0), Op::Slide(1), Op::Return])), Ok(()));
    }
}
