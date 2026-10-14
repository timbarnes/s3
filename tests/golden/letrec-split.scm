;; Internal definitions mixing a constant and procedures (split-letrecs):
;; the constant is bound by a let, and the procedures are a lambda group,
;; made with MakeClosure and PatchFree, with no boxes. The counter, which
;; a procedure assigns, keeps its box.
(define (scale xs)
  (define factor 3)
  (define count 0)
  (define (one x) (set! count (+ count 1)) (* x factor))
  (define (all xs) (if (null? xs) '() (cons (one (car xs)) (all (cdr xs)))))
  (all xs))
