;;; Code generation: core AST to templates, as s2's src/compile/codegen.rs did
;;; it, instruction for instruction (plan/08, C7).
;;;
;;; A post-order walk. Each expression's code leaves exactly one value on
;;; the stack. Two things are tracked:
;;;
;;; - the depth: the number of values in the current frame. A let
;;;   variable is the slot its init's value landed in;
;;; - whether an expression is in tail position: a call there becomes
;;;   tail-call, and anything else is followed by return.
;;;
;;; Instructions are lists, `(name operand ...)` (src/compile/data.rs),
;;; and %make-template turns them into a template. A jump is emitted with
;;; target 0 and patched when the target is known.

;; The state of compiling one procedure (one template): the code and
;; constants emitted so far, the stack depth, and where each variable
;; lives. An inner lambda gets a new gen (lambda-template) that shares
;; `boxed` and `frees` with its parent.
(define-record-type gen
  (make-gen boxed frees code consts const-index depth locs names local-names free-names self-var next-self params
            lines position recorded)
  gen?
  ;; Shared by a form's procedures: its boxed variables, and the memo of
  ;; its lambdas' free variables (free-variables).
  (boxed gen-boxed)
  (frees gen-frees)
  ;; a buffer of instructions, and one of constants
  (code gen-code)
  (consts gen-consts)
  ;; ((constant . index) ...), to find a constant already in the pool.
  (const-index gen-const-index set-gen-const-index!)
  (depth gen-depth set-gen-depth!)
  ;; Where each variable lives: (id local . slot) or (id free . index).
  (locs gen-locs set-gen-locs!)
  ;; Each variable's name: (id . string).
  (names gen-names set-gen-names!)
  ;; Each slot's names, a list, joined with / at the end.
  (local-names gen-local-names)
  (free-names gen-free-names)
  ;; The lambda-group variable this procedure is bound to, if any: a
  ;; tail call to it is a self-tail-call.
  (self-var gen-self-var set-gen-self-var!)
  ;; While compiling a lambda group's init: the variable it is bound to.
  (next-self gen-next-self set-gen-next-self!)
  (params gen-params set-gen-params!)
  ;; The line table (plan/12, T2): ip, position, ip, position ..., each
  ;; ip the first of the instructions compiled at that position.
  ;; `position` is that of the innermost node being compiled that has
  ;; one, or #f; `recorded` is the last one added to the table. Code
  ;; outside every node with a position (a top-level begin's pops, say)
  ;; can't raise an error, so it needn't be recorded as unknown.
  (lines gen-lines)
  (position gen-position set-gen-position!)
  (recorded gen-recorded set-gen-recorded!))

;; A gen for a new procedure, sharing the form's boxed variables and
;; free-variable memo; everything else starts empty.
(define (new-gen boxed frees)
  (make-gen boxed frees (make-buffer) (make-buffer) '() 0 '() '() (make-buffer) (make-buffer) #f #f 0
            (make-buffer) #f #f))

;; Compile a top-level form into a template of no arguments.
(define (codegen e)
  (let* ((e (split-letrecs e))
         (g (new-gen (boxed-variables e) (vector '()))))
    (gen-expr g e #t)
    (finish g "top-level" 0 #f)))

;; The template for what `g` has compiled: its code, constants, the names
;; of its slots (several names, joined by /, where sibling lets reuse a
;; slot) and of its free variables, for the disassembler and backtraces.
(define (finish g name required rest?)
  (let ((slot-names (map (lambda (names)
                           (if (null? names)
                               ""
                               (let join ((names (cdr names)) (text (car names)))
                                 (if (null? names) text (join (cdr names) (string-append text "/" (car names)))))))
                         (vector->list (buffer->vector (gen-local-names g))))))
    (%make-template name required rest? (buffer-count (gen-free-names g))
                    (buffer->vector (gen-code g))
                    (buffer->vector (gen-consts g))
                    (list->vector slot-names)
                    (buffer->vector (gen-free-names g))
                    (buffer->vector (gen-lines g)))))

;; Record that the code just emitted pushed `n` values (popped, if
;; negative). Every emit is followed by one, so the depth is always the
;; number of values in the frame, which is how a let finds its slots.
(define (depth+! g n) (set-gen-depth! g (+ (gen-depth g) n)))

;; The pool index of `v`, adding it if it isn't there. Constants are
;; shared by identity.
(define (constant g v)
  (cond ((assq v (gen-const-index g)) => cdr)
        (else (let ((i (buffer-push! (gen-consts g) v)))
                (set-gen-const-index! g (cons (cons v i) (gen-const-index g)))
                i))))

;; Emit an instruction; its index. If the position has changed since the
;; last one recorded, the line table records it.
(define (emit g . instruction)
  (let ((position (gen-position g)))
    (if (and position (not (eqv? position (gen-recorded g))))
        (begin (buffer-push! (gen-lines g) (buffer-count (gen-code g)))
               (buffer-push! (gen-lines g) position)
               (set-gen-recorded! g position))))
  (buffer-push! (gen-code g) instruction))

;; Make the call or global-ref `e`'s position, if it has one, the current
;; one; the outer one, to put back when `e` is compiled.
(define (enter-position g e)
  (let ((outer (gen-position g)) (position (node-position e)))
    (if position (set-gen-position! g position))
    outer))

;; Run `thunk`, which compiles the node `e`, at `e`'s position if it has
;; one; its value.
(define (at-position g e thunk)
  (if (memq (node-tag e) '(call global-ref))
      (let* ((outer (enter-position g e)) (result (thunk)))
        (set-gen-position! g outer)
        result)
      (thunk)))

;; Point the jump at `at` to the next instruction to be emitted.
(define (patch g at)
  (set-car! (last-pair (buffer-ref (gen-code g) at)) (buffer-count (gen-code g))))

;; Return the value just pushed, if in tail position.
(define (ret g tail?) (if tail? (emit g 'return)))

;; Whether variable `id` lives in a box (boxed-variables, analyze.scm);
;; where it lives, (local . slot) or (free . index); and its name.
(define (boxed? g id) (memv id (gen-boxed g)))
(define (loc g id) (cdr (assv id (gen-locs g))))
(define (name-of g id) (cdr (assv id (gen-names g))))

;; Make `var` local `slot` of the procedure being compiled, and add its
;; name to the slot's names.
(define (bind-local g var slot)
  (let ((name (identifier-name (var-name var)))
        (slots (gen-local-names g)))
    (set-gen-locs! g (cons (cons (var-id var) (cons 'local slot)) (gen-locs g)))
    (if (<= (buffer-count slots) slot) (buffer-set! slots slot '() '()))
    (let ((names (buffer-ref slots slot)))
      (if (not (member name names)) (buffer-set! slots slot (append names (list name)) '())))
    (set-gen-names! g (cons (cons (var-id var) name) (gen-names g)))))

;; Push the value of local variable `id`.
(define (load g id)
  (load-raw g id)
  (if (boxed? g id) (emit g 'box-ref)))

;; Push what `id`'s slot holds: its box, if it has one.
(define (load-raw g id)
  (let ((l (loc g id)))
    (if (eq? (car l) 'local) (emit g 'local-ref (cdr l)) (emit g 'free-ref (cdr l))))
  (depth+! g 1))

;; Box `var`, just bound in `slot`, if it needs a box.
(define (box-if-needed g var slot)
  (if (boxed? g (var-id var)) (emit g 'make-box slot)))

;; Assign the value of `value` to `id`, leaving nothing on the stack.
(define (assign g id value)
  (if (boxed? g id)
      (begin (load-raw g id)
             (gen-expr g value #f)
             (emit g 'box-set)
             (depth+! g -2))
      (begin (gen-expr g value #f)
             ;; Captured and assigned means boxed, so this is a slot.
             (emit g 'local-set (cdr (loc g id)))
             (depth+! g -1))))

;; Push the unspecified value: the result of set! and similar forms.
(define (unspecified g tail?)
  (emit g 'unspecified)
  (depth+! g 1)
  (ret g tail?))

;; Compile the expression `e`: code that leaves its value on the stack,
;; or, if `tail?`, returns it (a call in tail position becomes a tail
;; call). The heart of codegen: a post-order walk, one case per node.
(define (gen-expr g e tail?)
  (case (node-tag e)
    ((const) (constant-op g (const-value e)) (depth+! g 1) (ret g tail?))
    ((local-ref) (load g (local-ref-id e)) (ret g tail?))
    ((local-set) (assign g (local-set-id e) (local-set-value e)) (unspecified g tail?))
    ((global-set)
     (gen-expr g (global-value e) #f)
     (emit g 'global-set (constant g (global-cell e)))
     (depth+! g -1)
     (unspecified g tail?))
    ((global-ref)
     (let ((outer (enter-position g e)))
       (emit g 'global-ref (constant g (global-cell e)))
       (set-gen-position! g outer))
     (depth+! g 1)
     (ret g tail?))
    ((global-define)
     (gen-expr g (global-value e) #f)
     (emit g 'global-define (constant g (global-cell e)))
     (ret g tail?))
    ((if) (if-expr g e tail?))
    ((seq)
     (let loop ((body (seq-body e)))
       (if (null? (cdr body))
           (gen-expr g (car body) tail?)
           (begin (gen-expr g (car body) #f)
                  (emit g 'pop)
                  (depth+! g -1)
                  (loop (cdr body))))))
    ((lambda)
     (let ((self-var (gen-next-self g))
           (free (free-variables e (gen-frees g))))
       (set-gen-next-self! g #f)
       (for-each (lambda (id) (load-raw g id)) free)
       (let ((t (lambda-template g e free self-var)))
         (emit g 'make-closure (constant g t))
         (depth+! g (- 1 (length free)))
         (ret g tail?))))
    ((call)
     (let ((outer (enter-position g e)))
       (call-expr g e tail?)
       (set-gen-position! g outer)))
    ((let)
     (let ((base (gen-depth g)) (bindings (let-bindings e)))
       (for-each (lambda (b) (gen-expr g (binding-init b) #f)) bindings)
       (let loop ((bindings bindings) (i 0))
         (if (pair? bindings)
             (let ((var (binding-var (car bindings))))
               (bind-local g var (+ base i))
               (box-if-needed g var (+ base i))
               (loop (cdr bindings) (+ i 1)))))
       (end-scope g (let-body e) tail? (length bindings))))
    ((letrec)
     ;; Every variable gets its slot (and box) first, undefined, so the
     ;; inits can capture them; then each init is assigned in order.
     (let ((base (gen-depth g)) (bindings (let-bindings e)))
       (let loop ((bs bindings) (i 0))
         (if (pair? bs)
             (let ((var (binding-var (car bs))))
               (emit g 'undefined)
               (depth+! g 1)
               (bind-local g var (+ base i))
               (box-if-needed g var (+ base i))
               (loop (cdr bs) (+ i 1)))))
       (let ((group? (lambda-group? bindings (gen-boxed g))))
         (for-each (lambda (b)
                     (set-gen-next-self! g (and group? (var-id (binding-var b))))
                     (assign g (var-id (binding-var b)) (binding-init b))
                     (set-gen-next-self! g #f))
                   bindings)
         (if group? (patch-group g bindings)))
       (end-scope g (let-body e) tail? (length bindings))))))

;; Compile an if. The test, if it is an inlined predicate or comparison,
;; is fused with its jump (jump-if-not, or jump-if-not-imm against a small
;; constant); (if (not x) a b) becomes (if x b a). In tail position each
;; branch returns, so no jump is needed over the else.
(define (if-expr g e tail?)
  ;; (if (not x) a b) is (if x b a).
  (let loop ((test (if-test e)) (then (if-then e)) (els (if-else e)))
    (if (and (eq? (node-tag test) 'call)
             (eq? (node-tag (call-fn test)) 'global-ref)
             (equal? (inlinable (call-fn test) (length (call-args test))) '(not)))
        (loop (car (call-args test)) els then)
        (let* ((fused (fused-test g test))
               ;; A comparison with a small constant.
               (imm (and fused
                         (let ((t (car fused)) (args (cdr fused)))
                           (and (= (test-arity t) 2)
                                (not (eq? t 'eq?))
                                (eq? (node-tag (cadr args)) 'const)
                                (small? (const-value (cadr args)) -32768 32767)
                                (list t (car args) (const-value (cadr args)))))))
               ;; A fused test is compiled at the test's position.
               (to-else
                (at-position
                 g test
                 (lambda ()
                   (cond (imm
                          (gen-expr g (cadr imm) #f)
                          (depth+! g -1)
                          (emit g 'jump-if-not-imm (car imm) (caddr imm) 0))
                         (fused
                          (for-each (lambda (a) (gen-expr g a #f)) (cdr fused))
                          (depth+! g (- (length (cdr fused))))
                          (emit g 'jump-if-not (car fused) 0))
                         (else
                          (gen-expr g test #f)
                          (depth+! g -1)
                          (emit g 'jump-if-false 0))))))
               (depth (gen-depth g)))
          (gen-expr g then tail?)
          (set-gen-depth! g depth)
          (if tail?
              ;; The then-branch has returned, so no jump over the else.
              (begin (patch g to-else) (gen-expr g els #t))
              (let ((to-end (emit g 'jump 0)))
                (patch g to-else)
                (gen-expr g els #f)
                (patch g to-end)))))))

;; Compile a call: a self-tail-call for a loop (a lambda-group procedure
;; calling itself in tail position), one instruction for an inlined
;; primitive (car, +, vector-ref ...: inlinable), else the procedure and
;; arguments followed by call or tail-call.
(define (call-expr g e tail?)
  (let* ((f (call-fn e)) (args (call-args e)) (n (length args)))
    (cond
     ;; A loop: a tail call from a lambda-group procedure to itself.
     ((and tail?
           (eq? (node-tag f) 'local-ref)
           (gen-self-var g)
           (eqv? (local-ref-id f) (gen-self-var g))
           (= n (gen-params g)))
      (for-each (lambda (a) (gen-expr g a #f)) args)
      (emit g 'self-tail-call n))
     ((and (eq? (node-tag f) 'global-ref) (inlinable f n))
      => (lambda (op)
           (if (not (superinstruction g op args tail?))
               (begin (for-each (lambda (a) (gen-expr g a #f)) args)
                      (apply emit g op)
                      (depth+! g (- 1 n))
                      (ret g tail?)))))
     (else
      (gen-expr g f #f)
      (for-each (lambda (a) (gen-expr g a #f)) args)
      (emit g (if tail? 'tail-call 'call) n)
      (depth+! g (- n))))))

;; Whether `v` is an exact integer from `lo` to `hi`.
(define (small? v lo hi) (and (exact-integer? v) (<= lo v hi)))

;; The value of `e` if it is a constant that fits an instruction's
;; 32-bit immediate (add-imm, sub-imm), else #f.
(define (small-constant e)
  (and (eq? (node-tag e) 'const)
       (small? (const-value e) -2147483648 2147483647)
       (const-value e)))

;; Compile the inlined call `op` of `args` as a superinstruction, if it is
;; one: car or cdr of an unboxed local, or adding or subtracting a
;; constant. #f if it isn't.
(define (superinstruction g op args tail?)
  (define (done) (ret g tail?) #t)
  (case (car op)
    ((car cdr)
     (let ((a (car args)))
       (and (eq? (node-tag a) 'local-ref)
            (not (boxed? g (local-ref-id a)))
            (let ((l (loc g (local-ref-id a))))
              (and (eq? (car l) 'local)
                   (begin (emit g (if (eq? (car op) 'car) 'car-local 'cdr-local) (cdr l))
                          (depth+! g 1)
                          (done)))))))
    ((add)
     (let ((a (car args)) (b (cadr args)))
       (cond ((small-constant b) => (lambda (k) (gen-expr g a #f) (emit g 'add-imm k) (done)))
             ((small-constant a) => (lambda (k) (gen-expr g b #f) (emit g 'add-imm k) (done)))
             (else #f))))
    ((sub)
     (let ((k (small-constant (cadr args))))
       (and k (begin (gen-expr g (car args) #f) (emit g 'sub-imm k) (done)))))
    (else #f)))

;; The instruction a call to the global `ref` (a global-ref node) with
;; `argc` arguments is inlined as, or #f.
(define (inlinable ref argc) (%inline-instruction (global-cell ref) argc))

;; The test of an if whose test is an inlinable predicate or comparison,
;; and its arguments: (test arg ...), or #f.
(define (fused-test g test)
  (and (eq? (node-tag test) 'call)
       (eq? (node-tag (call-fn test)) 'global-ref)
       (let* ((op (inlinable (call-fn test) (length (call-args test))))
              (t (and op (test-of (car op)))))
         (and t (cons t (call-args test))))))

;; The test, as jump-if-not names it, of the inlined instruction `name`,
;; or #f if it isn't a predicate or comparison.
(define (test-of name)
  (case name
    ((null?) 'null?) ((pair?) 'pair?) ((eq?) 'eq?)
    ((num-eq) '=) ((lt) '<) ((gt) '>) ((le) '<=) ((ge) '>=)
    (else #f)))

;; How many arguments test `t` takes.
(define (test-arity t) (if (memq t '(null? pair?)) 1 2))

;; After a lambda group's closures are made, in order: each one that
;; captured a member made at or after it captured a placeholder, so fill
;; in the member.
(define (patch-group g bindings)
  (let ((ids (map (lambda (b) (var-id (binding-var b))) bindings)))
    (let loop ((bs bindings) (i 0))
      (if (pair? bs)
          (let ((var (binding-var (car bs))))
            (let free ((fvs (free-variables (binding-init (car bs)) (gen-frees g))) (j 0))
              (if (pair? fvs)
                  (let ((k (index-of (car fvs) ids)))
                    (if (and k (>= k i))
                        (let ((c (loc g (var-id var))) (v (loc g (car fvs))))
                          (if (and (eq? (car c) 'local) (eq? (car v) 'local))
                              (emit g 'patch-free (cdr c) j (cdr v)))))
                    (free (cdr fvs) (+ j 1)))))
            (loop (cdr bs) (+ i 1)))))))

;; Compile the body of a let or letrec whose `n` variables are the top
;; `n` slots, then drop them unless the frame is ending anyway.
(define (end-scope g body tail? n)
  (gen-expr g body tail?)
  (if (and (not tail?) (> n 0))
      (begin (emit g 'slide n) (depth+! g (- n)))))

;; Compile an inner lambda into its own template. Its free variables,
;; `free`, are free-ref 0, free-ref 1, ... inside it.
(define (lambda-template g l free self-var)
  (let ((inner (new-gen (gen-boxed g) (gen-frees g)))
        (vars (lambda-vars l)))
    (set-gen-self-var! inner (and (not (lambda-rest l)) self-var))
    (set-gen-params! inner (length (lambda-params l)))
    (let loop ((vars vars) (i 0))
      (if (pair? vars)
          (begin (bind-local inner (car vars) i)
                 (box-if-needed inner (car vars) i)
                 (loop (cdr vars) (+ i 1)))))
    (let loop ((free free) (i 0))
      (if (pair? free)
          (let ((name (name-of g (car free))))
            (set-gen-locs! inner (cons (cons (car free) (cons 'free i)) (gen-locs inner)))
            (buffer-push! (gen-free-names inner) name)
            (set-gen-names! inner (cons (cons (car free) name) (gen-names inner)))
            (loop (cdr free) (+ i 1)))))
    (set-gen-depth! inner (length vars))
    (gen-expr inner (lambda-body l) #t)
    (finish inner (lambda-name l) (length (lambda-params l)) (and (lambda-rest l) #t))))

;; Push a literal, with a dedicated instruction where there is one.
(define (constant-op g v)
  (cond ((small? v -2147483648 2147483647) (emit g 'fixnum v))
        ((null? v) (emit g 'nil))
        ((eq? v (if #f #f)) (emit g 'unspecified))
        ((eq? v #t) (emit g 'true))
        ((eq? v #f) (emit g 'false))
        (else (emit g 'const (constant g v)))))
