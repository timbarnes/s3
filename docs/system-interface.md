# System Interface

R7RS: [section 6.14, System interface](https://standards.scheme.org/corrected-r7rs/r7rs-Z-H-8.html#TAG:__tex2page_sec_6.14). It covers `(scheme process-context)`, `(scheme time)`, and `load` from `(scheme load)`. `file-exists?` and `delete-file` are in [Input and Output](./input-and-output.md), and `features` in [Libraries](./libraries.md#cond-expand-and-features).

## Running s3

```
s3 [FLAGS]                  REPL on standard input
s3 [FLAGS] -e TEXT          evaluate each form in TEXT, writing each result
s3 [FLAGS] -f FILE          evaluate each form in FILE; an error is reported
                            and the next form runs
s3 [FLAGS] PROGRAM ARG ...  run a program; PROGRAM may be source or compiled
s3 --compile FILE [-o OUT]  compile the program FILE to OUT (by default FILE
                            with the extension .s3o), without running it
```

* **A program** runs, and s3 exits when it finishes. Everything after the program's name, including words that start with `-`, is passed to the program, and `command-line` returns it. The first error ends the program with status 70 (`EX_SOFTWARE`), after the `after` thunks of any `dynamic-wind` it is inside have run. The program's first line may be `#!/usr/bin/env s3` (the reader treats `#!/...` and `#! ...` as comments to the end of the line).
* **`-e` and `-f`** are for experiments and tests: each form's result is written (`-e`) or not (`-f`), and an error is reported and the next form runs. s3 then exits with status 1 if any form failed, otherwise 0.
* **The REPL** is described in [Program Structure](./program-structure.md#the-repl).
* **The flags** show what the compiler does with each form before it runs: `--expand` prints the core form after macro expansion, and `--dis` the compiled bytecode.

If standard output is a pipe whose reader closes it, s3 ends at once, as `cat` does: `s3 prog.scm | head` stops when `head` has its lines.

s3's core library, `scheme/core.scm`, and its compiler are built into the binary, already compiled, so s3 runs from any directory and needs no other files. The environment variables `S3_CORE_IMAGE` and `S3_COMPILER_IMAGE` name other compiled images to start from instead, made with `s3 --core-image FILE` and `s3 --compiler-image FILE`.

### Compiled programs

`s3 --compile prog.scm` writes `prog.s3o`, and `s3 prog.s3o arg ...` runs it as `s3 prog.scm arg ...` would, without compiling again. Compiling runs nothing of the program, except that the libraries it imports are loaded and its own `define-library` forms are run, so later forms can import them.

Library files are compiled the same way, automatically: the first import of `foo/bar.sld` writes `foo/bar.s3o` beside it, and later runs use that file until `foo/bar.sld` (or a file it includes) changes, or s3 is rebuilt. See [Libraries](./libraries.md#library-files).

A compiled file records the version of its format, and s3 refuses a file of another version.

## `command-line`

`(command-line)`

Returns the command line as a list of strings: the program's name as given, followed by its arguments. Without a program it is `("s3")`.

```scheme
;; s3 count.scm a b
(command-line)          ; => ("count.scm" "a" "b")
```

## `exit`

`(exit [obj])`

Runs the `after` thunks of every `dynamic-wind` the call is inside, innermost first, flushes standard output and every open file port, and ends the process. The exit status is 0 if `obj` is absent or `#t`, 1 if it is `#f`, and `obj` itself if it is an exact integer. Any other `obj` is an error. Works at the REPL too.

## `emergency-exit`

`(emergency-exit [obj])`

Ends the process at once, with the same exit status as `exit`, without running any `after` thunks. Standard output and open file ports are flushed.

## `get-environment-variable`

`(get-environment-variable name)`

Returns the value of the environment variable `name` (a string) as a string, or `#f` if it isn't set. A value that isn't valid UTF-8 is converted with replacement characters.

```scheme
(get-environment-variable "HOME")       ; => "/home/user"
(get-environment-variable "NO_SUCH")    ; => #f
```

## `get-environment-variables`

`(get-environment-variables)`

Returns all environment variables as a list of `(name . value)` pairs of strings.

## `current-second`

`(current-second)`

Returns the current time as an inexact number of seconds since the Unix epoch (1970-01-01). R7RS specifies TAI; s3 uses the system clock, which is UTC, so it differs from TAI by the leap seconds (37 in 2026).

## `current-jiffy`

`(current-jiffy)`

Returns the number of jiffies since the program first called `current-jiffy`, as an exact integer, from a clock that never goes backwards. Use it to time things:

```scheme
(define start (current-jiffy))
(do-something)
(/ (- (current-jiffy) start) (jiffies-per-second))   ; elapsed seconds, exact
```

## `jiffies-per-second`

`(jiffies-per-second)`

Returns 1000000000: a jiffy is a nanosecond.

## `load`

`(load filename [environment])`

`filename` must be a string. `load` reads every form in the file, then evaluates them in turn in `environment`, by default the interaction environment (see [Environments and Evaluation](./environments.md)), and returns when the last has run. Each form is compiled as it is reached, so a macro defined in the file can be used by the forms after it. Implemented in `core.scm`.
