//! The s3 binary: `-e`, standard input, files, exit status.

use std::io::Write;
use std::process::{Command, Output, Stdio};

fn s3(args: &[&str], stdin: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_s3"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    child.wait_with_output().unwrap()
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[test]
fn eval_flag_writes_each_result() {
    let o = s3(&["-e", "'(a . (b c)) ''x #(1 2/4) (define x 10) (if (< x 20) (+ x 1) 0) (* 6 7)"], "");
    assert!(o.status.success());
    assert_eq!(stdout(&o), "(a b c)\n(quote x)\n#(1 1/2)\nx\n11\n42\n");
}

#[test]
fn stdin_without_a_terminal_has_no_prompt() {
    let o = s3(&[], "'#0=(1 . #0#)\n\"s\"\n");
    assert!(o.status.success());
    assert_eq!(stdout(&o), "#0=(1 . #0#)\n\"s\"\n");
}

#[test]
fn syntax_error_continues_and_fails() {
    let o = s3(&["-e", "1 #\\bogus 2 )"], "");
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(stdout(&o), "1\n2\n");
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("unknown character name"), "{}", err);
    assert!(err.contains("Unexpected ')'"), "{}", err);
}

#[test]
fn runtime_error_continues_and_fails() {
    let o = s3(&["-e", "1 undefined-var (display \"after\")"], "");
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(stdout(&o), "1\nafter");
    assert!(String::from_utf8_lossy(&o.stderr).contains("unbound variable: undefined-var"));
}

#[test]
fn file_results_are_not_written() {
    let path = std::env::temp_dir().join(format!("s3-cli-test-{}.scm", std::process::id()));
    std::fs::write(&path, "(define x 6) (* x 7) (display (* x 7)) (newline)").unwrap();
    let o = s3(&[path.to_str().unwrap()], "");
    std::fs::remove_file(&path).ok();
    assert!(o.status.success());
    assert_eq!(stdout(&o), "42\n");
}

#[test]
fn dis_prints_code_without_running_it() {
    let o = s3(&["-e", "(dis '(display 1)) (dis '(if))"], "");
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(stdout(&o), "template top-level\n   0  GlobalRef k0     ; display\n   1  Fixnum 1\n   2  TailCall 1\n");
    assert!(String::from_utf8_lossy(&o.stderr).contains("if: bad syntax: (if)"));
}

/// A program that starts with an import is compiled as one body (plan/09):
/// a small procedure is inlined at its calls, the library's too.
#[test]
fn programs_are_compiled_as_one_body() {
    let path = std::env::temp_dir().join(format!("s3-cli-program-{}.scm", std::process::id()));
    let program = "(import (scheme base) (scheme write))
        (define-library (t lib) (export twice add1 show) (import (scheme base) (scheme write))
          (begin (define (helper x) (* 2 x)) (define (twice x) (helper x))
                 (define (unused x) (+ x 1)) (define (add1 x) (unused x))
                 (define-syntax show (syntax-rules () ((_ e) (display (helper e)))))))
        (import (t lib))
        (define (sq x) (* x x))
        (define (g y) (+ (sq y) (+ (twice y) (add1 y))))
        (display (g 5)) (show 21)";
    std::fs::write(&path, program).unwrap();
    let run = s3(&[path.to_str().unwrap()], "");
    let dis = s3(&["--dis", path.to_str().unwrap()], "");
    std::fs::write(&path, "(import (scheme base) (scheme write)) (display 1) (display (").unwrap();
    let bad = s3(&[path.to_str().unwrap()], "");
    std::fs::remove_file(&path).ok();
    assert_eq!(stdout(&run), "4142");
    let dis = stdout(&dis);
    let g = dis.split("template ").find(|t| t.starts_with("g ")).unwrap();
    assert!(!g.contains("Call"), "g calls nothing:\n{}", g);
    // The whole program is read before any of it runs.
    assert_eq!(stdout(&bad), "");
    assert_eq!(bad.status.code(), Some(70));
}

#[test]
fn expand_prints_the_core_ast() {
    let o = s3(&["--expand", "-e", "(let ((x 1)) (and x 2))"], "");
    assert!(o.status.success());
    assert_eq!(stdout(&o), "(let ((x.1 1)) (if x.1 2 #f))\n2\n");
}

#[test]
fn bad_usage() {
    assert_eq!(s3(&["-x"], "").status.code(), Some(2));
}

/// The boot images are current and at a fixed point: s3, started from
/// them, compiles `core.scm` and the compiler to the same bytes. If this
/// fails after a change to `scheme/`, run `scripts/rebuild-images.sh`.
#[test]
fn boot_images_reproduce_themselves() {
    let dir = std::env::temp_dir().join(format!("s3-boot-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (flag, image) in [("--core-image", "s3-core.s3o"), ("--compiler-image", "s3-compiler.s3o")] {
        let out = dir.join(image);
        let o = s3(&[flag, out.to_str().unwrap()], "");
        assert!(o.status.success(), "{}: {}", flag, String::from_utf8_lossy(&o.stderr));
        let committed = std::fs::read(format!("{}/boot/{}", env!("CARGO_MANIFEST_DIR"), image)).unwrap();
        assert!(std::fs::read(&out).unwrap() == committed, "boot/{} is stale: run scripts/rebuild-images.sh", image);
    }
    std::fs::remove_dir_all(&dir).unwrap();
}
