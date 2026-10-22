;;; The driver (plan/08, C6): compiling a form, eval, dis, and loading a
;;; library file. At startup, s3 binds eval, dis and
;;; %load-library to eval-form, dis-form and load-library, so every
;;; compile goes through here. Loading never has to wait: an import of a
;;; library whose file isn't loaded loads it there and then
;;; (find-library).

;; Compile one top-level form for the environment `env`: expand it to the
;; core AST, then generate its template.
(define (compile form env) (codegen (expand form env)))

;; A program's forms (R7RS 5.1), expanded in order as one body, so that
;; its small procedures can be inlined (inline-body): their ASTs. A
;; define-library among them is run when it is reached, so that the forms
;; after it can import it, as one loaded from a file would be.
(define (expand-program forms env)
  (positioned (lambda () (expand-program-forms forms env))))

(define (expand-program-forms forms env)
  (let* ((x (make-expander env '() 0 0 0 '()))
         (nodes (map-in-order (lambda (form)
                                (let ((node (expr x form #t)))
                                  (if (library-form? form) ((%template->procedure (codegen node))))
                                  node))
                              forms)))
    (inline-body x nodes (expander-macros x) env)))

;; Whether `form` is a define-library (compile_file in Rust decides the
;; same way).
(define (library-form? form) (and (pair? form) (eq? (car form) 'define-library)))

;; eval (R7RS 6.12). The compiled form is called in tail position, so a
;; tail call to eval is still one.
(define (eval-form . args)
  (if (not (= (length args) 2))
      (fail (string-append "eval: wrong number of arguments: " (number->string (length args)))))
  (if (not (environment? (cadr args)))
      (fail "eval: the second argument must be an environment"))
  ((%template->procedure (compile-for-eval (car args) (cadr args)))))

;; Compile a form for eval. A compile error is raised again from here,
;; outside the compiler, so that its backtrace shows the program's
;; procedures rather than the compiler's.
(define (compile-for-eval form env)
  (let ((result (guard (e (#t (cons #f e)))
                  (cons #t (compile form env)))))
    (if (car result) (cdr result) (raise (cdr result)))))

;; dis: compile a form for the interaction environment and print its
;; code, without running it.
(define (dis-form datum)
  (%disassemble (compile datum (interaction-environment))))

;; Load a library file, for an import that needs one: marked loaded
;; first, so a library that imports itself can't loop. From its compiled
;; file if that is fresh, else compiled.
(define (load-library path)
  (%library-file-loading path)
  (let retry ()
    (let ((cached (%cached-library path)))
      (cond ((string? cached) (load-library cached) (retry))
            (cached (for-each (lambda (thunk) (thunk)) cached))
            (else (compile-library path))))))

;; Compile a library file's forms in the interaction environment, running
;; each before compiling the next: a later define-library may import an
;; earlier one. If every form compiles, write the compiled file.
(define (compile-library path)
  (let ((env (interaction-environment))
        (templates '())
        (done #f))
    (dynamic-wind
     %library-compile-begin
     (lambda ()
       (for-each (lambda (form)
                   (let ((t (compile form env)))
                     (set! templates (cons t templates))
                     ((%template->procedure t))))
                 (%read-library-file path))
       (set! done #t))
     (lambda () (%library-compile-end path (and done (reverse templates)))))))
