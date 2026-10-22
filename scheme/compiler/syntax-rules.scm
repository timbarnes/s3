;;; Identifiers, aliases and syntax-rules (R7RS 4.3.2), as
;;; src/compile/syntax_rules.rs does them, with the same messages.
;;;
;;; An identifier is a symbol or an alias: the renamed identifier a macro
;;; expansion introduces, which remembers the identifier it renames and the
;;; macro's definition scope. The expander resolves aliases; this file only
;;; parses, matches and instantiates.
;;;
;;; A macro is a Rust object (a parsed SyntaxRules), read and made through
;;; %macro-parts and %make-macro, so the two front ends share macros.

;; Whether `v` is an identifier: a symbol, or an alias, the renamed
;; identifier a macro expansion introduces (src/heap.rs, Obj::Alias).
(define (identifier? v) (or (symbol? v) (%alias? v)))

;; An alias's parts: the identifier it renames, and the scope it was
;; renamed in, `depth` frames over the environment `env`.
(define (alias-original a) (vector-ref (%alias-parts a) 0))
(define (alias-depth a) (vector-ref (%alias-parts a) 1))
(define (alias-env a) (vector-ref (%alias-parts a) 2))

;; The symbol an identifier ultimately renames.
(define (strip id) (if (%alias? id) (strip (alias-original id)) id))

;; The name of that symbol.
(define (id-name id) (symbol->string (strip id)))

;; The elements of a possibly improper list, and its final cdr:
;; (items . tail).
(define (list-parts list)
  (let loop ((rest list) (items '()))
    (if (pair? rest)
        (loop (cdr rest) (cons (car rest) items))
        (cons (reverse items) rest))))

;; A vector's elements, or a list's elements and final cdr.
(define (sequence-parts v)
  (if (vector? v) (cons (vector->list v) '()) (list-parts v)))

;; Fail the compile with the message `text`, after the current position
;; (expand.scm) if there is one.
(define (fail text)
  (let ((where (and current-position (%position-string current-position))))
    (error (if where (string-append where ": " text) text))))

;;; A transformer, as the matcher sees it.

(define-record-type syntax-transformer
  (make-transformer ellipsis literals rules depth env)
  transformer?
  (ellipsis transformer-ellipsis)
  (literals transformer-literals)
  ;; ((matcher . template) ...), both compiled
  (rules transformer-rules)
  (depth transformer-depth)
  (env transformer-env))

;; The macro `m`'s parts, as a record the matcher can use.
(define (macro->transformer m)
  (let ((parts (%macro-parts m)))
    (make-transformer (vector-ref parts 0) (vector-ref parts 1) (vector-ref parts 2)
                      (vector-ref parts 3) (vector-ref parts 4))))

;; Whether `id` is one of the macro's literals.
(define (literal? sr id) (memq id (transformer-literals sr)))

;; Whether `v` is the ellipsis. Literals take priority. A custom ellipsis
;; is matched by identity; the default matches any identifier named ...
(define (ellipsis? sr v)
  (and (identifier? v)
       (not (literal? sr v))
       (if (transformer-ellipsis sr)
           (eq? v (transformer-ellipsis sr))
           (eq? (strip v) '...))))

;; Whether `id` is the pattern wildcard _ (unless it is a literal).
(define (underscore? sr id) (and (not (literal? sr id)) (eq? (strip id) '_)))

;; Whether `ok?` holds for some element of `items`.
(define (any? ok? items)
  (and (pair? items) (or (ok? (car items)) (any? ok? (cdr items)))))

;;; Patterns are analysed once, when the macro is defined: each rule's
;;; pattern, without its keyword, is stored as a matcher, a tagged vector:
;;;
;;;   #(lit id)  #(any)  #(var id)  #(nil)  #(datum x)
;;;   #(list (p ...) tail)          a list with no ellipsis; tail a matcher
;;;   #(seq (p ...) rep (p ...) tail vars n-before n-after)
;;;                                 a list with an ellipsis after rep (or #f
;;;                                 for none); vars, rep's pattern variables
;;;   #(vec seq)                    a vector, seq a seq matcher
;;;
;;; Matching. Bindings are an association list from pattern variable to
;;; (one . form) or (many . (match ...)).

;; The matcher for pattern `p` (see above).
(define (compile-pattern sr p)
  (cond ((identifier? p)
         (cond ((literal? sr p) (vector 'lit p))
               ((underscore? sr p) (vector 'any))
               (else (vector 'var p))))
        ((pair? p)
         (let ((parts (list-parts p)))
           (if (any? (lambda (i) (ellipsis? sr i)) (car parts))
               (compile-sequence sr (car parts) (cdr parts))
               (vector 'list (map (lambda (i) (compile-pattern sr i)) (car parts))
                       (compile-pattern sr (cdr parts))))))
        ((vector? p) (vector 'vec (compile-sequence sr (vector->list p) '())))
        ((null? p) (vector 'nil))
        (else (vector 'datum p))))

;; The element before the ellipsis takes as many inputs as leave enough
;; for the patterns after it. (An ellipsis first is a parse error.)
(define (compile-sequence sr items tail)
  (let* ((at (index-where (lambda (i) (ellipsis? sr i)) items))
         (before (if at (take items (- at 1)) items))
         (repeated (and at (list-ref items (- at 1))))
         (after (if at (list-tail items (+ at 1)) '()))
         (compile-all (lambda (ps) (map (lambda (i) (compile-pattern sr i)) ps))))
    (vector 'seq (compile-all before) (and repeated (compile-pattern sr repeated)) (compile-all after)
            (compile-pattern sr tail) (if repeated (pattern-vars sr repeated) '())
            (length before) (length after))))

;; The first `n` elements of `items`.
(define (take items n) (if (= n 0) '() (cons (car items) (take (cdr items) (- n 1)))))

;; The position of the first element of `items` for which `ok?` holds, or
;; #f.
(define (index-where ok? items)
  (let loop ((items items) (i 0))
    (cond ((null? items) #f)
          ((ok? (car items)) i)
          (else (loop (cdr items) (+ i 1))))))

;; The first rule whose pattern matches the use `form`: (template .
;; bindings). `same?` says whether a literal (as the macro's definition
;; scope sees it) and an input identifier (as the use's scope sees it)
;; mean the same binding.
(define (match-use sr form same?)
  (let loop ((rules (transformer-rules sr)))
    (if (null? rules)
        (fail (string-append "no syntax rule matches " (show form)))
        (let ((binds (match (car (car rules)) (cdr form) same? '())))
          (if binds
              (cons (cdr (car rules)) binds)
              (loop (cdr rules)))))))

;; The bindings extended by matching the matcher `m` against `input`, or #f.
(define (match m input same? binds)
  (let ((tag (vector-ref m 0)))
    (cond ((eq? tag 'var) (cons (cons (vector-ref m 1) (cons 'one input)) binds))
          ((eq? tag 'list) (match-list (vector-ref m 1) (vector-ref m 2) input same? binds))
          ((eq? tag 'nil) (and (null? input) binds))
          ((eq? tag 'lit) (and (identifier? input) (same? (vector-ref m 1) input) binds))
          ((eq? tag 'any) binds)
          ((eq? tag 'seq)
           (let ((inputs (list-parts input)))
             (match-sequence m (car inputs) (cdr inputs) same? binds)))
          ((eq? tag 'vec)
           (and (vector? input) (match-sequence (vector-ref m 1) (vector->list input) '() same? binds)))
          (else (and (equal? (vector-ref m 1) input) binds)))))

;; A list with no ellipsis, pair by pair; the tail matches whatever input
;; remains.
(define (match-list ms tail input same? binds)
  (cond ((null? ms) (match tail input same? binds))
        ((pair? input)
         (let ((binds (match (car ms) (car input) same? binds)))
           (and binds (match-list (cdr ms) tail (cdr input) same? binds))))
        (else #f)))

;; Match each matcher against the inputs in turn, from the start of both.
(define (match-each ms inputs same? binds)
  (cond ((not binds) #f)
        ((null? ms) binds)
        (else (match-each (cdr ms) (cdr inputs) same? (match (car ms) (car inputs) same? binds)))))

;; A seq matcher against a list's items and final cdr.
(define (match-sequence m inputs input-tail same? binds)
  (let* ((repeated (vector-ref m 2))
         (n-before (vector-ref m 6))
         (n (- (length inputs) n-before (vector-ref m 7)))
         (reps (if repeated (and (>= n 0) n) (and (= n 0) 0))))
    (and reps
         (let ((binds (match-each (vector-ref m 1) inputs same? binds)))
           (and binds
                (let ((binds
                       (if repeated
                           (let collect ((ins (list-tail inputs n-before)) (i 0) (matches '()))
                             (if (< i reps)
                                 (let ((b (match repeated (car ins) same? '())))
                                   (and b (collect (cdr ins) (+ i 1) (cons b matches))))
                                 (let ((matches (reverse matches)))
                                   (let add ((vars (vector-ref m 5)) (binds binds))
                                     (if (null? vars)
                                         (match-each (vector-ref m 3) ins same? binds)
                                         (add (cdr vars)
                                              (cons (cons (car vars)
                                                          (cons 'many (map (lambda (b) (cdr (assq (car vars) b))) matches)))
                                                    binds)))))))
                           binds)))
                  (and binds (match (vector-ref m 4) input-tail same? binds))))))))

;; The pattern variables in `pattern`, in order.
(define (pattern-vars sr pattern)
  (let ((vars '()))
    (collect-pattern-vars sr pattern 0 (lambda (v depth) (set! vars (cons v vars))))
    (reverse vars)))

;; Call `out` with each pattern variable and its ellipsis depth.
(define (collect-pattern-vars sr pattern depth out)
  (cond ((identifier? pattern)
         (if (and (not (literal? sr pattern)) (not (underscore? sr pattern)) (not (ellipsis? sr pattern)))
             (out pattern depth)))
        ((or (pair? pattern) (vector? pattern))
         (let ((parts (sequence-parts pattern)))
           (let loop ((items (car parts)))
             (if (pair? items)
                 (begin
                   (collect-pattern-vars sr (car items)
                                         (if (and (pair? (cdr items)) (ellipsis? sr (cadr items))) (+ depth 1) depth)
                                         out)
                   (loop (cdr items)))))
           (collect-pattern-vars sr (cdr parts) depth out)))))

;;; Templates, like patterns, are analysed once, when the macro is
;;; defined, into a tagged vector:
;;;
;;;   #(pvar id)  #(ident id)  #(const x)
;;;   #(list (item ...) tail)   item a template, or #(rep t depth vars raw):
;;;                             t followed by depth ellipses, vars the
;;;                             pattern variables in it, raw for messages
;;;   #(vec (item ...))
;;;
;;; `vars` are the rule's pattern variables. Inside (... template) the
;;; ellipsis is an ordinary identifier.

;; The compiled form of template `t` (see above). `vars` are the pattern
;; variables; `escaped?`, inside (... ...), where ellipses are ordinary.
(define (compile-template sr t vars escaped?)
  (cond ((identifier? t) (if (memq t vars) (vector 'pvar t) (vector 'ident t)))
        ((pair? t)
         (if (and (not escaped?) (ellipsis? sr (car t)) (pair? (cdr t)))
             (compile-template sr (cadr t) vars #t)
             (let ((parts (list-parts t)))
               (vector 'list (compile-items sr (car parts) vars escaped?)
                       (compile-template sr (cdr parts) vars escaped?)))))
        ((vector? t) (vector 'vec (compile-items sr (vector->list t) vars escaped?)))
        (else (vector 'const t))))

;; The elements of a list or vector template, with `sub ...` as a rep.
(define (compile-items sr items vars escaped?)
  (let loop ((items items) (out '()))
    (if (null? items)
        (reverse out)
        (let ((depth (if escaped?
                         0
                         (let count ((rest (cdr items)) (n 0))
                           (if (and (pair? rest) (ellipsis? sr (car rest))) (count (cdr rest) (+ n 1)) n)))))
          (if (= depth 0)
              (loop (cdr items) (cons (compile-template sr (car items) vars escaped?) out))
              (loop (list-tail (cdr items) depth)
                    (cons (vector 'rep (compile-template sr (car items) vars escaped?) depth
                                  (template-vars (car items) vars) (car items))
                          out)))))))

;; The pattern variables occurring in `template`, in order, once each.
(define (template-vars template vars)
  (let ((out '()))
    (let walk ((t template))
      (cond ((identifier? t)
             (if (and (memq t vars) (not (memq t out))) (set! out (cons t out))))
            ((pair? t) (walk (car t)) (walk (cdr t)))
            ((vector? t) (for-each walk (vector->list t)))))
    (reverse out)))

;;; Instantiation: one fresh alias per distinct identifier the template
;;; introduces, per expansion.

;; The expansion: the compiled `template` with the pattern variables'
;; values from `binds`, and every other identifier renamed to a fresh
;; alias, the same alias for the same identifier within one expansion.
(define (instantiate sr template binds)
  (let ((renames '()))
    (define (rename id)
      (cond ((assq id renames) => cdr)
            (else (let ((a (%make-alias id (transformer-depth sr) (transformer-env sr))))
                    (set! renames (cons (cons id a) renames))
                    a))))
    (define (inst t binds)
      (let ((tag (vector-ref t 0)))
        (cond ((eq? tag 'pvar)
               (let ((b (cdr (assq (vector-ref t 1) binds))))
                 (if (eq? (car b) 'one)
                     (cdr b)
                     (fail (string-append "syntax-rules: pattern variable " (show (vector-ref t 1))
                                          " used without an ellipsis")))))
              ((eq? tag 'ident) (rename (vector-ref t 1)))
              ((eq? tag 'list) (append (inst-items (vector-ref t 1) binds) (inst (vector-ref t 2) binds)))
              ((eq? tag 'vec) (list->vector (inst-items (vector-ref t 1) binds)))
              (else (vector-ref t 1)))))
    (define (inst-items items binds)
      (let loop ((items items) (out '()))
        (cond ((null? items) (reverse out))
              ((eq? (vector-ref (car items) 0) 'rep)
               (loop (cdr items) (append (reverse (inst-repeated (car items) binds (vector-ref (car items) 2))) out)))
              (else (loop (cdr items) (cons (inst (car items) binds) out))))))
    ;; A rep with `depth` ellipses left: the list of results.
    (define (inst-repeated rep binds depth)
      (let ((seqs (let loop ((vars (vector-ref rep 3)) (seqs '()))
                    (if (null? vars)
                        (reverse seqs)
                        (let ((b (cdr (assq (car vars) binds))))
                          (loop (cdr vars) (if (eq? (car b) 'many) (cons (cons (car vars) (cdr b)) seqs) seqs)))))))
        (if (null? seqs)
            (fail (string-append "syntax-rules: no pattern variable to repeat in " (show (vector-ref rep 4)))))
        (let ((n (length (cdr (car seqs)))))
          (if (any? (lambda (s) (not (= (length (cdr s)) n))) seqs)
              (fail "syntax-rules: pattern variables repeat different numbers of times"))
          (let loop ((i 0) (out '()))
            (if (= i n)
                (reverse out)
                (let ((inner (let add ((seqs seqs) (inner binds))
                               (if (null? seqs)
                                   inner
                                   (add (cdr seqs) (cons (cons (car (car seqs)) (list-ref (cdr (car seqs)) i)) inner))))))
                  (loop (+ i 1)
                        (if (= depth 1)
                            (cons (inst (vector-ref rep 1) inner) out)
                            (append (reverse (inst-repeated rep inner (- depth 1))) out)))))))))
    (inst template binds)))

;;; Parsing: (syntax-rules [ellipsis] (literal ...) (pattern template) ...),
;;; defined in a scope of `depth` frames over `env`, checked, as a macro.

;; The macro for the (syntax-rules ...) form `form`: its literals and
;; rules checked, and their patterns and templates compiled.
(define (parse-syntax-rules form depth env)
  (let ((parts (list-parts form)))
    (if (or (not (null? (cdr parts))) (< (length (car parts)) 2))
        (fail "syntax-rules: expected (syntax-rules (literal ...) rule ...)"))
    (let* ((rest (cdr (car parts)))
           (ellipsis (and (identifier? (car rest)) (car rest)))
           (rest (if ellipsis (cdr rest) rest)))
      (if (null? rest) (fail "syntax-rules: missing literal list"))
      (let ((literals (list-parts (car rest))))
        (if (not (and (null? (cdr literals)) (every? identifier? (car literals))))
            (fail "syntax-rules: literals must be a list of identifiers"))
        (let ((sr (make-transformer ellipsis (car literals) '() depth env)))
          (let ((rules
                 (map-in-order
                  (lambda (rule)
                    (let ((pt (list-parts rule)))
                      (if (not (and (= (length (car pt)) 2) (null? (cdr pt))))
                          (fail (string-append "syntax-rules: a rule must be (pattern template): " (show rule))))
                      (let ((pattern (car (car pt))) (template (cadr (car pt))))
                        (if (not (pair? pattern))
                            (fail (string-append "syntax-rules: a pattern must be a list: " (show pattern))))
                        ;; The keyword position is ignored entirely.
                        (check-pattern sr (cdr pattern))
                        (let ((depths '()))
                          (collect-pattern-vars sr (cdr pattern) 0
                                                (lambda (v d)
                                                  (if (assq v depths)
                                                      (fail (string-append "syntax-rules: pattern variable " (show v) " appears twice")))
                                                  (set! depths (cons (cons v d) depths))))
                          (check-template sr template depths 0 #f)
                          (cons (compile-pattern sr (cdr pattern))
                                (compile-template sr template (map car depths) #f))))))
                  (cdr rest))))
            (%make-macro ellipsis (car literals) rules depth env)))))))

;; map, applying `f` from the first element on: map's order is
;; unspecified, and the expander numbers variables as it goes.
(define (map-in-order f items)
  (let loop ((items items) (out '()))
    (if (null? items) (reverse out) (loop (cdr items) (cons (f (car items)) out)))))

;; At most one ellipsis per list or vector level, and it must follow a
;; pattern.
(define (check-pattern sr p)
  (if (or (pair? p) (vector? p))
      (let* ((parts (sequence-parts p))
             (n (let count ((items (car parts)) (n 0))
                  (if (null? items) n (count (cdr items) (if (ellipsis? sr (car items)) (+ n 1) n))))))
        (if (> n 1) (fail (string-append "syntax-rules: more than one ellipsis in " (show p))))
        (if (and (pair? (car parts)) (ellipsis? sr (car (car parts))))
            (fail (string-append "syntax-rules: ellipsis must follow a pattern in " (show p))))
        (for-each (lambda (i) (if (not (ellipsis? sr i)) (check-pattern sr i))) (car parts))
        (check-pattern sr (cdr parts)))))

;; Every pattern variable must be used under at least as many ellipses as
;; it was matched under.
(define (check-template sr template depths depth escaped?)
  (cond ((identifier? template)
         (let ((d (assq template depths)))
           (if (and d (> (cdr d) depth))
               (fail (string-append "syntax-rules: pattern variable " (show template) " needs "
                                    (number->string (cdr d)) " ellipsis(es) in the template")))))
        ((and (pair? template) (not escaped?) (ellipsis? sr (car template)))
         (let ((parts (list-parts (cdr template))))
           (if (and (= (length (car parts)) 1) (null? (cdr parts)))
               (check-template sr (car (car parts)) depths depth #t)
               (fail "syntax-rules: (... template) takes exactly one template"))))
        ((or (pair? template) (vector? template))
         (let ((parts (sequence-parts template)))
           (let loop ((items (car parts)))
             (if (pair? items)
                 (let ((n (if escaped?
                              0
                              (let count ((rest (cdr items)) (n 0))
                                (if (and (pair? rest) (ellipsis? sr (car rest))) (count (cdr rest) (+ n 1)) n)))))
                   (check-template sr (car items) depths (+ depth n) escaped?)
                   (loop (list-tail (cdr items) n)))))
           (check-template sr (cdr parts) depths depth escaped?)))))
