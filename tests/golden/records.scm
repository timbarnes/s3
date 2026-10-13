;; records: an accessor's body is one RecordRef and a modifier's one
;; RecordSet, the type and the field's index fetched from globals.
(define-record-type point (make-point x y) point? (x point-x set-point-x!) (y point-y))
