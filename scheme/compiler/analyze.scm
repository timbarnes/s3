;;; Analysis over the core AST, as s2's src/compile/analyze.rs did it:
;;;
;;; - the free variables of each lambda, the values its flat closure copies;
;;; - which variables need boxes;
;;; - which letrecs are lambda groups, after splitting off the simple
;;;   bindings of those that are lambdas and values (split-letrecs).
;;;
;;; Variables are numbers here, unique within a top-level form, so no
;;; scopes are needed. Sets are lists.

;; The local variables that lambda `l` references but doesn't bind, in
;; order of first reference. A variable is free if it is referenced
;; anywhere inside (nested lambdas included) and bound nowhere inside.
;; Assigning a variable counts as a reference: a captured, assigned
;; variable's box must be copied.
;;
;; `memo` is a one-element vector holding ((lambda . free) ...) for the
;; lambdas of one form, so each is walked once. A nested lambda counts as
;; its free variables, in their order: the same result as walking into it,
;; since variable numbers are unique within the form, so its own
;; variables are referenced nowhere else.
(define (free-variables l memo)
  (cond ((assq l (vector-ref memo 0)) => cdr)
        (else (let ((free (walk-free-variables l memo)))
                (vector-set! memo 0 (cons (cons l free) (vector-ref memo 0)))
                free))))

;; free-variables without the memo: walk `l`'s body, collecting the
;; variables referenced and those bound inside, and keep the first.
(define (walk-free-variables l memo)
  (let ((bound (map var-id (lambda-vars l)))
        (refs '()))
    (define (bind! v) (set! bound (cons (var-id v) bound)))
    (define (ref! id) (set! refs (cons id refs)))
    (define (walk e)
      (case (node-tag e)
        ((const global-ref) #f)
        ((local-ref) (ref! (local-ref-id e)))
        ((local-set) (ref! (local-set-id e)) (walk (local-set-value e)))
        ((global-define global-set) (walk (global-value e)))
        ((if) (walk (if-test e)) (walk (if-then e)) (walk (if-else e)))
        ((seq) (for-each walk (seq-body e)))
        ((lambda) (for-each ref! (free-variables e memo)))
        ((call) (walk (call-fn e)) (for-each walk (call-args e)))
        ((let)
         (for-each (lambda (b) (bind! (binding-var b)) (walk (binding-init b))) (let-bindings e))
         (walk (let-body e)))
        ((letrec)
         (for-each (lambda (b) (bind! (binding-var b))) (let-bindings e))
         (for-each (lambda (b) (walk (binding-init b))) (let-bindings e))
         (walk (let-body e)))))
    (walk (lambda-body l))
    (let loop ((refs (reverse refs)) (free '()))
      (cond ((null? refs) (reverse free))
            ((or (memv (car refs) bound) (memv (car refs) free)) (loop (cdr refs) free))
            (else (loop (cdr refs) (cons (car refs) free)))))))

;; Whether a letrec's bindings are a lambda group: every init a lambda,
;; and no variable boxed.
(define (lambda-group? bindings boxed)
  (every? (lambda (b)
            (and (eq? (node-tag (binding-init b)) 'lambda)
                 (not (memv (var-id (binding-var b)) boxed))))
          bindings))

;; The variables that live in a box (plan/03 §7): those
;; assigned with set!, and letrec variables referenced before their init
;; has run, outside lambda groups.
(define (boxed-variables e)
  (let ((assigned '()) (pending '()) (early '()))
    (define (add id set) (if (memv id set) set (cons id set)))
    (define (collect e)
      (case (node-tag e)
        ((const global-ref local-ref) #f)
        ((local-set)
         (set! assigned (add (local-set-id e) assigned))
         (collect (local-set-value e)))
        ((global-define global-set) (collect (global-value e)))
        ((if) (collect (if-test e)) (collect (if-then e)) (collect (if-else e)))
        ((seq) (for-each collect (seq-body e)))
        ((lambda) (collect (lambda-body e)))
        ((call) (collect (call-fn e)) (for-each collect (call-args e)))
        ((let letrec)
         (for-each (lambda (b) (collect (binding-init b))) (let-bindings e))
         (collect (let-body e)))))
    (define (used! id)
      (if (memv id pending) (set! early (add id early))))
    (define (walk e)
      (case (node-tag e)
        ((const global-ref) #f)
        ((local-ref) (used! (local-ref-id e)))
        ((local-set) (used! (local-set-id e)) (walk (local-set-value e)))
        ((global-define global-set) (walk (global-value e)))
        ((if) (walk (if-test e)) (walk (if-then e)) (walk (if-else e)))
        ((seq) (for-each walk (seq-body e)))
        ((lambda) (walk (lambda-body e)))
        ((call) (walk (call-fn e)) (for-each walk (call-args e)))
        ((let)
         (for-each (lambda (b) (walk (binding-init b))) (let-bindings e))
         (walk (let-body e)))
        ((letrec)
         ;; In a lambda group nothing runs during the inits, so no
         ;; reference is early.
         (if (not (lambda-group? (let-bindings e) assigned))
             (for-each (lambda (b) (set! pending (cons (var-id (binding-var b)) pending)))
                       (let-bindings e)))
         (for-each (lambda (b)
                     (walk (binding-init b))
                     (set! pending (remove-id (var-id (binding-var b)) pending)))
                   (let-bindings e))
         (walk (let-body e)))))
    (collect e)
    (walk e)
    (let loop ((early early) (boxed assigned))
      (if (null? early) boxed (loop (cdr early) (add (car early) boxed))))))

;; `ids` without `id`.
(define (remove-id id ids)
  (cond ((null? ids) '())
        ((eqv? (car ids) id) (remove-id id (cdr ids)))
        (else (cons (car ids) (remove-id id (cdr ids))))))

;; `e` with each letrec whose inits are all lambdas or simple values, and
;; some of them values, split in two: (let (simple ...) (letrec (lambda
;; ...) body)), so that the lambdas are a lambda group, made without boxes
;; (*Fixing Letrec*). A body of internal definitions with a constant among
;; its procedures is such a letrec. A simple init is a constant or a
;; variable from outside the letrec: neither it nor making a closure can
;; run code, so evaluating the simple ones first changes nothing.
(define (split-letrecs e)
  (define (split e) (split-letrecs e))
  (case (node-tag e)
    ((const local-ref global-ref) e)
    ((local-set) (vector 'local-set (local-set-id e) (split (local-set-value e))))
    ((global-define global-set) (vector (node-tag e) (global-cell e) (split (global-value e))))
    ((if) (vector 'if (split (if-test e)) (split (if-then e)) (split (if-else e))))
    ((seq) (vector 'seq (map split (seq-body e))))
    ((call) (make-call (split (call-fn e)) (map split (call-args e)) (node-position e)))
    ((lambda) (lambda-with-body e (split (lambda-body e))))
    ((let) (vector 'let (map (lambda (b) (cons (binding-var b) (split (binding-init b)))) (let-bindings e)) (split (let-body e))))
    ((letrec)
     (let* ((bindings (map (lambda (b) (cons (binding-var b) (split (binding-init b)))) (let-bindings e)))
            (body (split (let-body e)))
            (ids (map (lambda (b) (var-id (binding-var b))) bindings)))
       (define (simple? init)
         (case (node-tag init)
           ((const global-ref) #t)
           ((local-ref) (not (memv (local-ref-id init) ids)))
           (else #f)))
       (define (lambda? init) (eq? (node-tag init) 'lambda))
       (if (and (any? (lambda (b) (simple? (binding-init b))) bindings)
                (every? (lambda (b) (or (lambda? (binding-init b)) (simple? (binding-init b)))) bindings))
           (let loop ((bs bindings) (simple '()) (lambdas '()))
             (cond ((pair? bs)
                    (if (lambda? (binding-init (car bs)))
                        (loop (cdr bs) simple (cons (car bs) lambdas))
                        (loop (cdr bs) (cons (car bs) simple) lambdas)))
                   ((null? lambdas) (vector 'let (reverse simple) body))
                   (else (vector 'let (reverse simple) (vector 'letrec (reverse lambdas) body)))))
           (vector 'letrec bindings body))))
    (else e)))
