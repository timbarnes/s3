;;; The core AST as tagged vectors (plan/08, C2), as src/compile/data.rs
;;; makes them:
;;;
;;;   #(const v)              #(local-ref id)          #(local-set id e)
;;;   #(global-ref cell)      #(global-set cell e)     #(global-define cell e)
;;;   #(if test then else)    #(seq (e ...))           #(call f (arg ...))
;;;   #(lambda name (var ...) rest body)   name a string, "" if none; rest a var or #f
;;;   #(let ((var . init) ...) body)       #(letrec ((var . init) ...) body)
;;;   #(var id name)          a binding occurrence: its number, and its name
;;;
;;; A call and a global-ref may have one more field, where they were read
;;; (plan/12, T2): a position, as %source-position gives it. Codegen
;;; records it for the code each one compiles to. Nodes made without one
;;; are compiled at the position of the call they are inside.
;;;
;;; Only this file indexes the vectors.

;; A node's tag (const, local-ref, call ...), and then the accessors of
;; each kind of node's fields, in the order of the table above.
(define (node-tag n) (vector-ref n 0))

(define (var-id v) (vector-ref v 1))
(define (var-name v) (vector-ref v 2))

(define (const-value n) (vector-ref n 1))
(define (local-ref-id n) (vector-ref n 1))
(define (local-set-id n) (vector-ref n 1))
(define (local-set-value n) (vector-ref n 2))
;; global-ref, global-set and global-define
(define (global-cell n) (vector-ref n 1))
(define (global-value n) (vector-ref n 2))
(define (if-test n) (vector-ref n 1))
(define (if-then n) (vector-ref n 2))
(define (if-else n) (vector-ref n 3))
(define (seq-body n) (vector-ref n 1))
(define (call-fn n) (vector-ref n 1))
(define (call-args n) (vector-ref n 2))
;; call and global-ref: where it was read, or #f.
(define (node-position n)
  (let ((i (if (eq? (node-tag n) 'call) 3 2)))
    (and (> (vector-length n) i) (vector-ref n i))))
;; A call node, at `position` if it isn't #f.
(define (make-call f args position)
  (if position (vector 'call f args position) (vector 'call f args)))
(define (lambda-name n) (vector-ref n 1))
(define (lambda-params n) (vector-ref n 2))
(define (lambda-rest n) (vector-ref n 3))
(define (lambda-body n) (vector-ref n 4))
;; let and letrec
(define (let-bindings n) (vector-ref n 1))
(define (let-body n) (vector-ref n 2))
(define (binding-var b) (car b))
(define (binding-init b) (cdr b))

;; The parameters, then the rest parameter if there is one.
(define (lambda-vars l)
  (if (lambda-rest l)
      (append (lambda-params l) (list (lambda-rest l)))
      (lambda-params l)))

;; A value as `write` shows it, as the printer's print_value gives it,
;; for names and messages.
(define (show v)
  (let ((port (open-output-string)))
    (write v port)
    (get-output-string port)))

;; An identifier's name as a string, as the printer shows it.
(define (identifier-name id) (show id))

;; The core AST as text, for --expand. A local variable is shown as
;; name.id, so two variables with the same name (from shadowing, or a
;; macro's renamed temporaries) can be told apart; a named procedure as
;; (named-lambda (name param ...) body), as in MIT Scheme; a letrec as
;; letrec*, which it is.
(define (show-ast e)
  (let ((port (open-output-string)) (names '()))
    (define (out . strings) (for-each (lambda (s) (write-string s port)) strings))
    (define (bind! v)
      (let ((s (string-append (identifier-name (var-name v)) "." (number->string (var-id v)))))
        (set! names (cons (cons (var-id v) s) names))
        s))
    (define (name id) (cdr (assv id names)))
    (define (global e) (show (%cell-name (global-cell e))))
    (define (each es) (for-each (lambda (x) (out " ") (walk x)) es))
    (define (join strings)
      (if (null? strings) "" (apply string-append (car strings) (map (lambda (s) (string-append " " s)) (cdr strings)))))
    (define (walk e)
      (case (node-tag e)
        ((const)
         (let ((v (const-value e)))
           (cond ((eq? v (if #f #f)) (out "#<unspecified>"))
                 ((or (symbol? v) (pair? v) (null? v)) (out "'" (show v)))
                 (else (out (show v))))))
        ((local-ref) (out (name (local-ref-id e))))
        ((global-ref) (out (global e)))
        ((local-set) (out "(set! " (name (local-set-id e)) " ") (walk (local-set-value e)) (out ")"))
        ((global-set global-define)
         (out "(" (if (eq? (node-tag e) 'global-set) "set!" "define") " " (global e) " ")
         (walk (global-value e))
         (out ")"))
        ((if) (out "(if") (each (list (if-test e) (if-then e) (if-else e))) (out ")"))
        ((seq) (out "(begin") (each (seq-body e)) (out ")"))
        ((call) (out "(") (walk (call-fn e)) (each (call-args e)) (out ")"))
        ((lambda)
         (let* ((named? (not (string=? (lambda-name e) "")))
                (head (append (if named? (list (lambda-name e)) '()) (map bind! (lambda-params e))))
                (rest (and (lambda-rest e) (bind! (lambda-rest e)))))
           (out "(" (if named? "named-lambda" "lambda") " "
                (cond ((and rest (null? head)) rest)
                      (rest (string-append "(" (join head) " . " rest ")"))
                      (else (string-append "(" (join head) ")")))
                " ")
           (walk (lambda-body e))
           (out ")")))
        ((let letrec)
         ;; Variable numbers are unique within a form, so naming them
         ;; before their inits are shown changes nothing.
         (out (if (eq? (node-tag e) 'letrec) "(letrec* (" "(let ("))
         (let loop ((vars (map (lambda (b) (bind! (binding-var b))) (let-bindings e)))
                    (bs (let-bindings e))
                    (sep ""))
           (if (pair? bs)
               (begin (out sep "(" (car vars) " ")
                      (walk (binding-init (car bs)))
                      (out ")")
                      (loop (cdr vars) (cdr bs) " "))))
         (out ") ")
         (walk (let-body e))
         (out ")"))))
    (walk e)
    (get-output-string port)))

;;; List helpers that R7RS-small lacks.

;; Whether `ok?` holds for every element of `items`.
(define (every? ok? items)
  (or (null? items) (and (ok? (car items)) (every? ok? (cdr items)))))

;; The last pair of the list `items`.
(define (last-pair items)
  (if (pair? (cdr items)) (last-pair (cdr items)) items))

;; The position of the first element of `items` that is eqv? to `x`, or #f.
(define (index-of x items) (index-where (lambda (y) (eqv? y x)) items))

;;; A buffer: a vector that grows as values are added to its end.

;; `items`, of which the first `count` are in use.
(define-record-type buffer
  (make-buffer-from items count)
  buffer?
  (items buffer-items set-buffer-items!)
  (count buffer-count set-buffer-count!))

;; An empty buffer.
(define (make-buffer) (make-buffer-from (make-vector 8 #f) 0))

;; Add `x` at the end; its index.
(define (buffer-push! b x)
  (let ((n (buffer-count b)) (items (buffer-items b)))
    (if (= n (vector-length items))
        (let ((bigger (make-vector (* 2 n) #f)))
          (vector-copy! bigger 0 items)
          (set-buffer-items! b bigger)))
    (vector-set! (buffer-items b) n x)
    (set-buffer-count! b (+ n 1))
    n))

;; Element `i`.
(define (buffer-ref b i) (vector-ref (buffer-items b) i))

;; Set element `i`, first growing the buffer with `fill` up to it.
(define (buffer-set! b i x fill)
  (let loop ()
    (if (<= (buffer-count b) i)
        (begin (buffer-push! b fill) (loop))))
  (vector-set! (buffer-items b) i x))

;; The elements in use, as a new vector.
(define (buffer->vector b) (vector-copy (buffer-items b) 0 (buffer-count b)))
