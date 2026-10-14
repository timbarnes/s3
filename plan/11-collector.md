# 11. Memory: compact objects, then perhaps a generational collector

Written 2026-10-13 as a plan for a generational collector. Rewritten the
same day after G0 measured where time and memory actually go. It comes
before native code (10): order 1 of the review.

## Why

plan/07 found two gaps against Gauche that it put down to the collector:

- **Allocation-heavy programs are slow.** `mperm` runs at 0.62 of
  Gauche's speed and `earley` at 0.64.
- **Peak memory is large.** `earley` reaches 8.1GB, `mperm` 3.9GB and
  `nboyer` 2.8GB.

The first plan was a generational collector. G0 measured first, and
found:

- the collector is only 5-27% of the time, so a generational collector
  could gain 1.05-1.2x at most;
- the memory comes from the size of objects: every object takes an
  80-byte slot, and most objects are 16-byte pairs.

So the order is now:

1. compact objects (G1, G2);
2. scheduling and returning memory (G3);
3. a generational collector only if it is still needed (G4).

## G0. Measure first (done)

`--features gcstats` (`src/gcstats.rs`) prints, at exit:

- allocations by kind, and bytes, including what is boxed outside the
  slot;
- mark and sweep times;
- how many objects survive their first collection;
- how much of the marking is spent on objects that survived before;
- peak live data and peak slots.

It is a feature of its own because `stats`' per-instruction counting
would distort the times.

### G0 results

The r7rs benchmarks were run at scale 10, as in 07; peak RSS, cycles and system
time come from `/usr/bin/time -l`.

| | GC % of run (mark + sweep) | survive first GC | old in marking | pairs % | closures % | peak live | peak slots | peak RSS |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| deriv | 19.5 (5.7 + 13.8) | 0.4% | 99% | 82 | 15 | 0.5MB | 2MB | 7MB |
| destruc | 5.3 (1.8 + 3.5) | 1.3% | 96% | 79 | 21 | 0.6MB | 2MB | 7MB |
| paraffins | 26.6 (23.0 + 3.6) | 99.6% | 33% | 50 | 0 (vectors 50) | 742MB | 743MB | 1.2GB |
| nboyer | 9.1 (5.0 + 4.1) | 30.6% | 51% | 100 | 0 | 901MB | 2.5GB | 2.8GB |
| mperm | 9.3 (8.5 + 0.8) | 83.7% | 37% | 78 | 22 | 1.3GB | 1.5GB | 3.9GB |
| gcbench | 11.2 (4.0 + 7.2) | 4.6% | 91% | 0 (records 100) | 0 | 88MB | 264MB | 445MB |
| earley | 27.3 (18.7 + 8.6) | 36.3% | 57% | 70 | 30 | 3.5GB | 6.2GB | 8.1GB |

("Old in marking": the share of all marking spent on objects that had
survived a collection before, which is what a minor collection skips.)

A native profile (`sample`) of the session thread gives the same
picture:

| | dispatch loop (incl. inline allocation) | `collect` | `new_page` | `malloc`/`free`/drop of boxes |
|---|---:|---:|---:|---:|
| mperm | 73% | 11% | 4.5% | under 2% |
| earley | 65% | 15% | 2.5% | about 5% |

Instructions per cycle are 3.5 (earley) to 4.3 (mperm), so the mutator
isn't stalling on memory to any great degree.

**What this says:**

1. **The collector is a smaller part of the time than 07 assumed.**
   Since `GC_GROWTH` became 2 (09), marking and sweeping take 5-27% of
   these programs. A minor collection saves only the old part of the
   marking, plus some of the sweeping. Its upper bound is about:
   - 1.1-1.2x on earley and paraffins;
   - 1.03-1.06x on mperm, gcbench and nboyer.

   The 1.5x first planned can't be reached. mperm's and earley's gap to
   Gauche is mostly the cost of each instruction, which plan/10
   addresses.
2. **Memory is the real problem, and it is the size of objects.** Pairs
   are 70-100% of the objects in every program but two (gcbench is
   records, paraffins half vectors), and each takes an 80-byte slot for
   16 bytes of content. Closures are most of the rest: 80 bytes, plus a
   separately allocated array of free variables (3.1GB in earley).
   - A 16-byte pair with its mark bit in a per-page bitmap would cut
     nboyer's live data about 5 times, and earley's and mperm's about
     3 times.
   - A generational collector would only reduce the slack between live
     data and peak slots: 1.1x (mperm, paraffins), 1.8x (earley),
     2.8x (nboyer).
3. **The young survive poorly in the large programs.** 30-84% of young
   objects survive their first collection in nboyer, earley and mperm,
   so promoting them after one survival would fill the old generation
   with data that dies soon after.
4. **Small heaps collect too often.** deriv collects 3,350 times, each
   time re-marking the ~6,500 objects the system itself holds, because
   `GC_THRESHOLD` is 20,000 objects. At 200,000, deriv takes 1.06x
   fewer cycles, destruc 1.01x and gcbench 0.99x. That's small, but it
   is a one-line change.

## The constraint: objects don't move

s3 depends everywhere on objects staying where they are:

- a `GcRef` is an address, and `Value` holds it;
- the dispatch loop's `Frame` holds `&'static Template` and the closure's
  free-variable slice;
- builtins hold `&Obj` across allocations;
- `eq?` hashing for hash tables (09, "what follows") wants addresses that
  don't change;
- native code (10) relies on values not moving between safepoints.

Every phase below keeps objects in place. Compact objects change where
new objects are put, not whether they move.

## Phases

The phases use the same rules as before: each ends with every test
passing in release, debug, and GC stress with poisoning. Speed is
measured in cycles with `/usr/bin/time -l`, against the commit before.
Memory is measured as peak RSS, and with `--features gcstats`.

### G1. Compact pairs

Pairs leave `Obj` and get pages of their own:

- **16-byte slots**, holding only the car and the cdr.
- **Pages aligned to their size** (64KB, 4,096 pairs). A pair's page
  header is then found by masking its address. The header holds a mark
  bitmap, one bit per slot.
- **A `Value` tag of their own**, `PAIR` (tags 5-7 are free), so
  `pair?`, `car` and `cdr` test the tag and never touch a header.
- **Free pairs are threaded through their cdr.**
- **Marking a pair** sets its bit. The worklist holds `Value`s, so it
  can hold pairs and objects alike. A collection clears the pair
  bitmaps first, which is a few hundred bytes for every megabyte of
  pairs: epochs aren't needed for pairs.
- **Sweeping a pair page** is a pass over its bitmap words. A pair owns
  nothing, so nothing is dropped, and a page whose bits are all clear
  can be given back (G3).
- **Poison mode** fills a freed pair with `<<FREED>>` and never reuses
  it, as it does now.

**The compiler finds the sites.** Removing `Obj::Pair` turns every place
that matched on it into a compile error: 27 matches, in the printer,
`syntax_rules.rs`, `heap.rs`, the reader, `fasl.rs` and the builtins.
The maps keyed by `GcRef` (printer and reader datum labels, `equal?`'s
cycle check, `fasl.rs`' sharing) take a key that covers both, an address
with its tag, which the `Value` itself already is.

- **Exit:**
  - every test passes, and so does chibi's suite;
  - peak RSS on earley, mperm, nboyer and paraffins, against G0's;
  - cycles on `bench/` and the r7rs set. Better cache use could make
    them faster; the second kind of slot could make them slower.
- **Target:** peak RSS halved or better on earley, mperm and nboyer, with
  no benchmark more than 3% slower.

### G1 results (2026-10-13)

Done as planned (`src/pairs.rs`). Some details settled while building
it:

- The header is 1.5KB, the page's first 96 slots, which leaves 4,000
  pairs a page. It holds the mark bits, and for poison mode a bit per
  poisoned slot and a scratch bit per slot that was free before the
  collection: those slots stay free, and the other unmarked ones are
  poisoned.
- Rust code that keys maps by identity now keys them by `Value`, which
  is `Eq` and `Hash` on its bits (`eq?`). This covers the printer's
  labels, the reader's datum labels, `equal?`'s cycle check, `fasl.rs`'
  object table and `strip_datum`. `GcRef` is for objects only.
- `<<FREED>>` is interned when the heap is made in poison mode. Interning
  it during a collection would make a symbol that the same collection
  sweeps.
- **No `#[inline]` on `cons`.** With it, `fib` was 16% slower in cycles
  with 0.9% more instructions: the dispatch loop's layout again (09).
  Without it, `fib` and the pair-heavy programs both did better.

Tests: all 109 pass in release, in debug, and with stress and poison
together. chibi's suite gives 1154, the baseline.

**Peak RSS** (`/usr/bin/time -l`, against G0's binary):

| | before | G1 | |
|---|---:|---:|---:|
| nboyer | 2822MB | 523MB | 5.4x less |
| mperm | 3940MB | 1485MB | 2.7x less |
| earley | 8128MB | 4000MB | 2.0x less |
| paraffins | 1181MB | 839MB | 1.4x less |

The target is met: earley, mperm and nboyer are halved or better.
earley's 4GB is now mostly closures, 80 bytes plus their boxed free
variables, which is G2's subject.

**Cycles**, the median of 3 runs, before divided by after, so higher is
faster:

| | | | | | |
|---|---:|---|---:|---|---:|
| earley | 1.27 | mperm | 1.15 | divrec | 1.12 |
| nboyer | 1.10 | paraffins | 1.10 | nqueens | 1.08 |
| primes | 1.08 | sum | 1.07 | deriv | 1.06 |
| bench/listmap | 1.05 | browse | 1.05 | destruc | 1.04 |
| sboyer | 1.04 | bench/gen | 1.03 | puzzle | 1.03 |
| bench/program | 1.02 | bench/deeprec | 1.02 | array1 | 1.02 |
| bench/tail | 1.01 | pnpoly | 1.01 | bench/guard | 1.00 |
| bench/macro-loop | 1.00 | cpstak | 1.00 | fib | 0.99 |
| bench/fib | 0.98 | bench/tak | 0.98 | gcbench | 0.97 |
| tak | 0.97 | | | | |

The geometric mean over these 28 is 1.04x. The programs that use pairs
gain from smaller objects and a cheaper sweep. Those that don't (`fib`,
`tak`, `cpstak`, and `gcbench`, which uses records) lose 0-3%. Their
code didn't change, so this is the dispatch loop's layout again.
**`tak`, at 0.969, is just past the 3% limit.** Chasing it means trying
layouts until one happens to suit it, which 09 found doesn't last. Left
as it is.

### G2. Compact closures and records (if G1 pays)

Closures are most of the objects that aren't pairs: 22% in mperm, 30% in
earley. Each takes an 80-byte slot, plus an array of free variables
allocated separately (3.1GB of them in earley). gcbench is all records,
with their fields boxed the same way. The plan:

- size classes of slots (for example 16, 32, 48 and 64 bytes);
- a closure's free variables, and a record's fields, inline after a
  small header;
- the same bitmap marking as G1.

The free-variable slice that `Frame` holds stays valid, because objects
don't move.

- **Exit:** as G1. Measured on earley, mperm and gcbench.
- **Decided after G1:** whether to go on, and whether vectors should
  join.

### G3. Scheduling and returning memory

- **`GC_THRESHOLD`** raised from 20,000 to a value measured on `bench/`
  and the r7rs set. G0: 200,000 gave deriv 1.06x and the others about
  1.0x.
- **Empty pages given back** to the system after a collection, removing
  `Slots`' ponytail note. With pair pages this is a bitmap test.
- **`GC_GROWTH`** measured again: with compact objects, a growth factor
  costs fewer bytes.

- **Exit:** peak RSS and cycles against G2.

### G4. A generational collector (deferred)

This is worth building only if, after G1-G3, collection time is still a
large share of programs that matter. G0 bounds the gain at 1.05-1.2x
today, and G0 also found that young objects survive poorly in the large
programs (30-84%).

If it is built, the design stays as first planned:

- **Sticky mark bits** (Demers et al. 1990), the non-moving scheme that
  Boehm's generational mode and Ruby's RGenGC use:
  - an object surviving a collection becomes old;
  - a minor collection marks from the roots and the remembered set,
    stops at old objects, and sweeps only the objects allocated since
    the collection before.
- **Promotion after two survivals**, not one, given G0's survival rates.
- **A write barrier:**
  - every store of a `Value` into a heap object goes through
    `Heap::store`;
  - a `Field` type replaces `Cell<Value>`, so the compiler finds every
    store (about 20 today: the dispatch loop's `PatchFree`, `BoxSet`,
    `GlobalSet`, `GlobalDefine`, `VectorSet` and `RecordSet`, the
    mutating builtins, binding cells, and the reader's datum labels);
  - environments call it from `Env::bind` and `own_cell`.
- **`S3_GC_VERIFY`** checks at every collection that no old object
  refers to a young one without being remembered. Stress mode makes
  minor collections constant.
- **If measured:** scanning only the part of the stack that changed
  since the last collection, and card marking for large vectors.

Native code (10) writes every store as `S3_STORE`, so this stays
possible after native code exists. Until G4, `S3_STORE` is a plain
store.

## Decisions to confirm

| # | Decision | Recommendation |
|---|---|---|
| G-a | Moving or not | Non-moving, in every phase |
| G-b | Order | Compact pairs (G1), then closures and records if G1 pays (G2), then scheduling (G3). Generational (G4) deferred |
| G-c | Pair layout | 16-byte slots in size-aligned pages, a mark bitmap in the page header, a `PAIR` tag in `Value` |
| G-d | Finding the sites | Remove `Obj::Pair`, so the compiler lists them |
| G-e | Branch | `collector`, from `main` plus the plans |

## Risks

- **Two kinds of heap reference.** Code that takes any object as a
  `GcRef` no longer sees pairs. Removing `Obj::Pair` makes the type
  checker find the matches. The maps keyed by `GcRef` have to be found
  by reading, and their tests (datum labels in the printer and reader,
  `equal?` on cyclic lists, `fasl.rs` sharing) cover them.
- **The dispatch loop's code generation** (09). `Car`, `Cdr`, `Cons`,
  `PairP`, `CarLocal` and `CdrLocal` all change. They get cheaper on
  paper (a tag test instead of loading the object's kind), but layout
  effects have moved fib and sum by 3-4% before. Measured in G1.
- **A premature free in the new sweep.** Stress and poison modes cover
  pairs from the first commit of G1, as they cover everything else.

## Reading

- Alan Demers, Mark Weiser, Barry Hayes, Hans Boehm, Daniel Bobrow, Scott
  Shenker, *Combining Generational and Conservative Garbage Collection*
  (POPL 1990). The sticky-mark-bit scheme.
- Koichi Sasada, RGenGC (RubyKaigi 2013). The same scheme added to an
  interpreter whose C extensions assume objects don't move, with a write
  barrier introduced gradually.
- Richard Jones, Antony Hosking, Eliot Moss, *The Garbage Collection
  Handbook* (2nd ed., 2023): the chapters on generational collection and on
  write barriers.
