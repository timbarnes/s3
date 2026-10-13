;; s3's prelude: an import first, so that s3 runs each benchmark as an
;; R7RS program, compiled as one body, rather than as a script.
(import (scheme base))
(define (this-scheme-implementation-name) "other")
