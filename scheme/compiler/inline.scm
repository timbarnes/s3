;;; Inlining within a library (plan/09, S1 step 3).
;;;
;;; A library's body is expanded as one form, so its whole AST is known
;;; at once. A variable it defines exactly once and never assigns has the
;;; same value from its definition on: nothing outside can assign it, as
;;; an import can't be assigned. If that value is a small procedure, its
;;; calls in the body with the right number of arguments are replaced by
;;; its body, in a let binding fresh copies of its parameters.
;;;
;;; The procedure is still defined, for its other uses and its exports.
;;; Two things change, both only for programs that are wrong already or
;;; for errors: an inlined procedure called before its definition has run
;;; runs instead of failing as unbound, and an error inside it has no
;;; frame of its own in the backtrace.
;;;
;;; A macro the library defines could expand, in an importer's code
;;; compiled later, into an assignment of the library's variables. So if
;;; any of those macros' templates names set!, nothing is inlined.

;; The most nodes an inlined procedure's body has.
(define max-inline-size 8)

;; The library body's nodes, with calls inlined. `macros` are those its
;; body defined; `lib` its environment.
(define (inline-library x body macros lib)
  (if (any? (lambda (m) (assigns? x m lib)) macros)
      body
      (let* ((found (inlinable-procedures body))
             (procs (car found)))
        (if (null? procs)
            body
            (map-in-order (lambda (e) (inline-calls x e procs (cdr found) '())) body)))))

;; Whether a macro's templates name set!, as the library sees it.
(define (assigns? x m lib)
  (let ((set-b (resolve-in x 'set! 0 lib)))
    (define (scan t)
      (let ((tag (vector-ref t 0)))
        (cond ((eq? tag 'ident)
               (let ((id (vector-ref t 1)))
                 (or (eq? (strip id) 'set!)
                     (same-binding? set-b (resolve-in x id 0 lib)))))
              ((eq? tag 'list) (or (any? scan (vector-ref t 1)) (scan (vector-ref t 2))))
              ((eq? tag 'vec) (any? scan (vector-ref t 1)))
              ((eq? tag 'rep) (scan (vector-ref t 1)))
              (else #f))))
    (any? (lambda (rule) (scan (cdr rule))) (vector-ref (%macro-parts m) 2))))

;; ((cell . lambda) ...), the procedures defined once, never assigned,
;; small, and simple enough to copy; and the local variables assigned
;; anywhere in the body: ((cell . lambda) ...) . (id ...).
(define (inlinable-procedures body)
  (let ((defined '()) (twice '()) (assigned '()) (locals '()))
    (define (note! e)
      (case (node-tag e)
        ((call) (note! (call-fn e)) (for-each note! (call-args e)))
        ((const local-ref global-ref) #f)
        ((if) (note! (if-test e)) (note! (if-then e)) (note! (if-else e)))
        ((let letrec) (for-each (lambda (b) (note! (binding-init b))) (let-bindings e)) (note! (let-body e)))
        ((seq) (for-each note! (seq-body e)))
        ((lambda) (note! (lambda-body e)))
        ((local-set) (set! locals (cons (local-set-id e) locals)) (note! (local-set-value e)))
        ((global-define)
         (if (assq (global-cell e) defined) (set! twice (cons (global-cell e) twice)))
         (set! defined (cons (cons (global-cell e) (global-value e)) defined))
         (note! (global-value e)))
        ((global-set) (set! assigned (cons (global-cell e) assigned)) (note! (global-value e)))
        (else #f)))
    (for-each note! body)
    (let loop ((ds defined) (out '()))
      (if (null? ds)
          (cons out locals)
          (let ((cell (car (car ds))) (value (cdr (car ds))))
            (loop (cdr ds)
                  (if (and (eq? (node-tag value) 'lambda)
                           (not (lambda-rest value))
                           (not (memq cell assigned))
                           (not (memq cell twice))
                           (copyable? (lambda-body value) cell))
                      (cons (cons cell value) out)
                      out)))))))

;; Whether a body is small, has no lambdas, letrecs or definitions in it,
;; and doesn't refer to `self`.
(define (copyable? body self)
  (let ((size 0))
    (let ok? ((e body))
      (set! size (+ size 1))
      (and (<= size max-inline-size)
           (case (node-tag e)
             ((const local-ref) #t)
             ((global-ref) (not (eq? (global-cell e) self)))
             ((local-set) (ok? (local-set-value e)))
             ((global-set) (and (not (eq? (global-cell e) self)) (ok? (global-value e))))
             ((if) (and (ok? (if-test e)) (ok? (if-then e)) (ok? (if-else e))))
             ((seq) (every? ok? (seq-body e)))
             ((call) (and (ok? (call-fn e)) (every? ok? (call-args e))))
             ((let) (and (every? (lambda (b) (ok? (binding-init b))) (let-bindings e)) (ok? (let-body e))))
             (else #f))))))

;; `e` with calls to `procs` inlined. `active`: the cells being inlined
;; around `e`, which aren't inlined again inside, so procedures that call
;; each other don't inline forever.
(define (inline-calls x e procs assigned active)
  (define (walk e) (inline-calls x e procs assigned active))
  (case (node-tag e)
    ((call)
     (let* ((f (call-fn e))
            (args (map-in-order walk (call-args e)))
            (proc (and (eq? (node-tag f) 'global-ref)
                       (not (memq (global-cell f) active))
                       (assq (global-cell f) procs))))
       (if (and proc (= (length args) (length (lambda-params (cdr proc)))))
           ;; Each argument that is a constant, or a local variable nothing
           ;; assigns, replaces its parameter, unless the body assigns it;
           ;; the rest are bound by a let.
           (let* ((l (cdr proc))
                  (body-sets (assigned-in (lambda-body l)))
                  (renames (let loop ((params (lambda-params l)) (args args) (out '()))
                             (if (null? params)
                                 (reverse out)
                                 (let ((param (car params)) (arg (car args)))
                                   (loop (cdr params) (cdr args)
                                         (cons (cons (var-id param)
                                                     (if (and (not (memv (var-id param) body-sets))
                                                              (or (eq? (node-tag arg) 'const)
                                                                  (and (eq? (node-tag arg) 'local-ref)
                                                                       (not (memv (local-ref-id arg) assigned)))))
                                                         arg
                                                         (fresh x (var-name param))))
                                               out))))))
                  (bound (let loop ((rs renames) (args args) (out '()))
                           (cond ((null? rs) (reverse out))
                                 ((eq? (node-tag (cdr (car rs))) 'var)
                                  (loop (cdr rs) (cdr args) (cons (cons (cdr (car rs)) (car args)) out)))
                                 (else (loop (cdr rs) (cdr args) out)))))
                  (copy (copy-body x (lambda-body l) renames))
                  ;; The copy's own assigned variables are new: so far
                  ;; `assigned` doesn't know them.
                  (body (inline-calls x copy procs (append (assigned-in copy) assigned) (cons (car proc) active))))
             (if (null? bound) body (vector 'let bound body)))
           (vector 'call (walk f) args))))
    ((const local-ref global-ref) e)
    ((local-set) (vector 'local-set (local-set-id e) (walk (local-set-value e))))
    ((global-define global-set) (vector (node-tag e) (global-cell e) (walk (global-value e))))
    ((if)
     (let* ((test (walk (if-test e))) (then (walk (if-then e))) (els (walk (if-else e))))
       (vector 'if test then els)))
    ((seq) (vector 'seq (map-in-order walk (seq-body e))))
    ((lambda)
     (vector 'lambda (lambda-name e) (lambda-params e) (lambda-rest e) (walk (lambda-body e))))
    ((let letrec)
     (let ((bindings (map-in-order (lambda (b) (cons (binding-var b) (walk (binding-init b)))) (let-bindings e))))
       (vector (node-tag e) bindings (walk (let-body e)))))
    (else e)))

;; The local variables a copyable body assigns.
(define (assigned-in e)
  (case (node-tag e)
    ((local-set) (cons (local-set-id e) (assigned-in (local-set-value e))))
    ((global-set) (assigned-in (global-value e)))
    ((if) (append (assigned-in (if-test e)) (assigned-in (if-then e)) (assigned-in (if-else e))))
    ((seq) (apply append (map assigned-in (seq-body e))))
    ((call) (apply append (assigned-in (call-fn e)) (map assigned-in (call-args e))))
    ((let) (apply append (assigned-in (let-body e)) (map (lambda (b) (assigned-in (binding-init b))) (let-bindings e))))
    (else '())))

;; A copy of a copyable body, its variables renamed: `renames` maps old
;; variable numbers to new vars, or to the node that replaces the
;; variable, and each let in it binds fresh ones.
(define (copy-body x e renames)
  (define (copy e) (copy-body x e renames))
  (define (renamed id) (cond ((assv id renames) => (lambda (r) (var-id (cdr r)))) (else id)))
  (case (node-tag e)
    ((const global-ref) e)
    ((local-ref)
     (let ((r (assv (local-ref-id e) renames)))
       (cond ((not r) e)
             ((eq? (node-tag (cdr r)) 'var) (vector 'local-ref (var-id (cdr r))))
             (else (cdr r)))))
    ((local-set) (vector 'local-set (renamed (local-set-id e)) (copy (local-set-value e))))
    ((global-set) (vector 'global-set (global-cell e) (copy (global-value e))))
    ((if) (vector 'if (copy (if-test e)) (copy (if-then e)) (copy (if-else e))))
    ((seq) (vector 'seq (map-in-order copy (seq-body e))))
    ((call) (vector 'call (copy (call-fn e)) (map-in-order copy (call-args e))))
    ((let)
     (let* ((inits (map-in-order (lambda (b) (copy (binding-init b))) (let-bindings e)))
            (vars (map-in-order (lambda (b) (fresh x (var-name (binding-var b)))) (let-bindings e)))
            (inner (append (map (lambda (b v) (cons (var-id (binding-var b)) v)) (let-bindings e) vars) renames)))
       (vector 'let (map cons vars inits) (copy-body x (let-body e) inner))))))
