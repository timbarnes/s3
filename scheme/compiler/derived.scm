;;; The derived forms (R7RS 7.3): and, or, when, unless, let*, cond, case
;;; and do (plan/09, S1 step 2). They were syntax-rules macros in core.scm;
;;; expanding them here skips the matcher and the renaming.
;;;
;;; Each builds the AST its macro's expansion did, expanding the use's
;;; parts in the same order, so variables are numbered alike and the code
;;; is the same. The identifiers a macro introduced (if, let, memv, a
;;; temporary) meant what they mean at top level, whatever the use's scope
;;; binds; here they are AST nodes, the system environment's cell for
;;; memv, and fresh variables that nothing can name. Literals (else, =>)
;;; are compared as a macro's were. A use that fits none of a macro's rules
;;; gets the macro's error.

(define (no-rule-matches form)
  (fail (string-append "no syntax rule matches " (show form))))

;; What the literal `name` means at top level, as a macro defined there
;; saw it.
(define (top-binding x name) (resolve-in x name 0 (%system-environment)))

;; Whether `id` is an identifier that means what a literal resolved to
;; `literal` means.
(define (literal-is? x literal id)
  (and (identifier? id) (same-binding? literal (resolve x id))))

(define (local-node var) (vector 'local-ref (var-id var)))

;; (if #f #f), which the macros used for an unspecified value.
(define (if-false-false) (vector 'if (vector 'const #f) (vector 'const #f) unspecified-node))

;; (let ((temp init)) body), with temp fresh: `make-body` gets its var.
(define (let-temp x name init make-body)
  (let ((var (fresh x name)))
    (vector 'let (list (cons var init)) (make-body var))))

;; (and test ...)
(define (and-form x items top?)
  (let ((tests (cdr items)))
    (cond ((null? tests) (vector 'const #t))
          ((null? (cdr tests)) (expr x (car tests) top?))
          (else
           ;; (if test1 (and test2 ...) #f), its else branch first.
           (let* ((test (expr x (car tests) #f))
                  (then (and-form x (cons (car items) (cdr tests)) #f)))
             (vector 'if test then (vector 'const #f)))))))

;; (or test ...)
(define (or-form x items top?)
  (let ((tests (cdr items)))
    (cond ((null? tests) (vector 'const #f))
          ((null? (cdr tests)) (expr x (car tests) top?))
          (else
           ;; (let ((x test1)) (if x x (or test2 ...)))
           (let-temp x 'x (expr x (car tests) #f)
                     (lambda (var)
                       (let ((els (or-form x (cons (car items) (cdr tests)) #f)))
                         (vector 'if (local-node var) (local-node var) els))))))))

;; (when test result1 result2 ...): (if test (begin result1 result2 ...))
(define (when-form x items)
  (if (< (length items) 3) (no-rule-matches items))
  (let* ((test (expr x (cadr items) #f))
         (then (sequence x (cddr items) #f)))
    (vector 'if test then unspecified-node)))

;; (unless test result1 result2 ...):
;; (if test (if #f #f) (begin result1 result2 ...))
(define (unless-form x items)
  (if (< (length items) 3) (no-rule-matches items))
  (let* ((els (sequence x (cddr items) #f))
         (test (expr x (cadr items) #f)))
    (vector 'if test (if-false-false) els)))

;; (let* ((name val) ...) body1 body2 ...): a let for each binding, the
;; last around (let () body1 body2 ...).
(define (let*-form x items)
  (if (not (and (>= (length items) 3)
                (list? (cadr items))
                (every? (lambda (spec) (and (list? spec) (= (length spec) 2))) (cadr items))))
      (no-rule-matches items))
  (let loop ((specs (cadr items)))
    (if (null? specs)
        (vector 'let '() (scoped x '() (lambda () (body x (cddr items)))))
        (let ((spec (car specs)))
          (if (not (identifier? (car spec)))
              (fail (string-append "let: bad binding: " (show spec))))
          (let* ((init (expr x (cadr spec) #f))
                 (var (fresh x (car spec))))
            (vector 'let (list (cons var init))
                    (scoped x (list var) (lambda () (loop (cdr specs))))))))))

;; (cond clause1 clause2 ...), its rules in their order:
;;   (else result1 result2 ...), the only clause left
;;   (test => result)
;;   (test)
;;   (test result1 result2 ...)
(define (cond-form x items top?)
  (let ((else-b (top-binding x 'else))
        (arrow-b (top-binding x '=>))
        (keyword (car items)))
    (let loop ((clauses (cdr items)) (top? top?))
      (if (not (and (pair? clauses) (pair? (car clauses)) (list? (car clauses))))
          (no-rule-matches (cons keyword clauses)))
      (let* ((clause (car clauses))
             (n (length clause))
             (more? (pair? (cdr clauses)))
             ;; (cond clause2 ...), expanded as an if's else branch is: first.
             (rest (lambda () (if more? (loop (cdr clauses) #f) unspecified-node))))
        (cond ((and (not more?) (>= n 2) (literal-is? x else-b (car clause)))
               ;; (begin result1 result2 ...)
               (begin-form x clause top?))
              ((and (= n 3) (literal-is? x arrow-b (cadr clause)))
               ;; (let ((temp test)) (if temp (result temp) (cond ...)))
               (let-temp x 'temp (expr x (car clause) #f)
                         (lambda (var)
                           (let* ((els (rest))
                                  (f (expr x (caddr clause) #f)))
                             (vector 'if (local-node var) (vector 'call f (list (local-node var))) els)))))
              ((= n 1)
               (if more?
                   ;; (let ((temp test)) (if temp temp (cond ...)))
                   (let-temp x 'temp (expr x (car clause) #f)
                             (lambda (var)
                               (let ((els (rest)))
                                 (vector 'if (local-node var) (local-node var) els))))
                   (expr x (car clause) top?)))
              (else
               ;; (if test (begin result1 result2 ...) (cond ...))
               (let* ((els (rest))
                      (test (expr x (car clause) #f))
                      (then (sequence x (cdr clause) #f)))
                 (vector 'if test then els))))))))

;; (case key clause1 clause2 ...). A key that is a list, an expression to
;; evaluate, is bound to a temporary first, and the clauses expanded in
;; the let's body. Then, with the key an identifier or a constant, its
;; rules in their order:
;;   (else => result), (else result1 result2 ...), the only clause left
;;   ((atom ...) => result)
;;   ((atom ...) result1 result2 ...)
(define (case-form x items top?)
  (if (< (length items) 2) (no-rule-matches items))
  (let ((keyword (car items)) (key (cadr items)))
    (if (list? key)
        ;; (let ((atom-key (key ...))) (case atom-key clause ...))
        (let-temp x 'atom-key (expr x key #f)
                  (lambda (var)
                    (case-clauses x keyword (cons 'temp var) (cddr items) #f #t)))
        (case-clauses x keyword (cons 'datum key) (cddr items) top? #f))))

;; `key` is (datum . key), or (temp . var) for the temporary. `in-body?`: the clauses are a
;; let's body, where an else clause's results are spliced, so they may
;; start with definitions.
(define (case-clauses x keyword key clauses top? in-body?)
  (let ((else-b (top-binding x 'else))
        (arrow-b (top-binding x '=>))
        (memv-cell (%env-cell (%system-environment) 'memv)))
    (define (key-node) (if (eq? (car key) 'temp) (local-node (cdr key)) (expr x (cdr key) #f)))
    (define (shown clauses) (cons keyword (cons (if (eq? (car key) 'temp) 'atom-key (cdr key)) clauses)))
    (let loop ((clauses clauses) (top? top?) (in-body? in-body?))
      (if (not (and (pair? clauses) (pair? (car clauses)) (list? (car clauses))))
          (no-rule-matches (shown clauses)))
      (let* ((clause (car clauses))
             (n (length clause))
             (more? (pair? (cdr clauses)))
             (arrow? (and (= n 3) (literal-is? x arrow-b (cadr clause))))
             (rest (lambda () (if more? (loop (cdr clauses) #f #f) unspecified-node))))
        (cond ((and (not more?) (>= n 2) (literal-is? x else-b (car clause)))
               (cond (arrow? (let ((f (expr x (caddr clause) #f))) (vector 'call f (list (key-node)))))
                     (in-body? (body x (cdr clause)))
                     (else (begin-form x clause top?))))
              ((not (list? (car clause))) (no-rule-matches (shown clauses)))
              (else
               ;; (if (memv key '(atom ...)) (result key) (case key ...)), or
               ;; (if (memv key '(atom ...)) (begin result1 ...) (case key ...))
               (let* ((els (rest))
                      (test (vector 'call (vector 'global-ref memv-cell)
                                    (list (key-node) (vector 'const (%strip-datum (car clause))))))
                      (then (if arrow?
                                (let ((f (expr x (caddr clause) #f))) (vector 'call f (list (key-node))))
                                (sequence x (cdr clause) #f))))
                 (vector 'if test then els))))))))

;; (do ((var init step ...) ...) (test expr ...) command ...):
;; (letrec ((loop (lambda (var ...)
;;                  (if test
;;                      (begin (if #f #f) expr ...)
;;                      (begin command ... (loop step-or-var ...))))))
;;   (loop init ...))
(define (do-form x items)
  (if (not (and (>= (length items) 3)
                (list? (cadr items))
                (every? (lambda (spec) (and (list? spec) (>= (length spec) 2))) (cadr items))
                (list? (caddr items))
                (pair? (caddr items))))
      (no-rule-matches items))
  (let* ((specs (cadr items))
         (exit (caddr items))
         (loop-var (fresh x 'loop))
         (proc (lambda-node
                x "loop" (map car specs)
                (lambda ()
                  (let* ((els (sequence-nodes
                               (append (map-in-order (lambda (c) (expr x c #f)) (cdddr items))
                                       (list (vector 'call (local-node loop-var)
                                                     (map-in-order (lambda (spec)
                                                            (cond ((= (length spec) 2) (expr x (car spec) #f))
                                                                  ((= (length spec) 3) (expr x (caddr spec) #f))
                                                                  (else (no-rule-matches (cons 'do (cons "step" (cons (car spec) (cddr spec))))))))
                                                          specs))))))
                         (test (expr x (car exit) #f))
                         (then (sequence-nodes (cons (if-false-false) (map-in-order (lambda (e) (expr x e #f)) (cdr exit))))))
                    (vector 'if test then els))))))
    (vector 'letrec (list (cons loop-var proc))
            (vector 'call (local-node loop-var) (map-in-order (lambda (spec) (expr x (cadr spec) #f)) specs)))))

;; Expanded expressions as one node, as sequence makes them.
(define (sequence-nodes nodes)
  (if (null? (cdr nodes)) (car nodes) (vector 'seq nodes)))
