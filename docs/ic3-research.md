# IC3 research: our own hypotheses

Working notes for closing the gap to rIC3 on the CPU-style models
(cv32e40x, rocket, ILA_Piccolo, veer). The rule from the project: every
experiment writes its prediction down before it runs, and keeps the
measured result next to it, hit or miss. Copying another tool's
heuristics is not the plan; measurements of our own IC3 are.

## Instrument: the IC3 event trace

`INFORMAL_IC3_TRACE=<file>` writes one JSON line per event:

| `e` | When | Fields |
|-----|------|--------|
| `L` | a lemma is added | `src` (`po` from an obligation, `ctg`, `ctp`, `import`), `lvl`, `top`, `q` (queries so far), `sub` (older lemmas it subsumes), `lits` |
| `P` | a push is tried | `from`, `ok`, `lits` |
| `F` | propagation starts | `q`, `sizes` (lemmas per level) |
| `O` | an obligation is taken | `lvl`, `d` (depth), `n` (cube length) |
| `H` | H2 moves lemmas to the invariant | `inv`, `moved`, `ms` |

## Baseline facts (ILA_Piccolo_JALR_sanity, informal-ic3, 90 s)

rIC3's IC3 proves this model in about 11 s (17 frames, about 560 lemmas,
7,354 solver calls). Ours did not finish in 200 s.

- Preprocessing is not the gap: after scorr rIC3 has 5,893 latches, we
  have 5,886.
- Solver calls cost 3.5 ms each before 03c84b7 and 1.8 ms after it
  (rIC3: 0.54 ms), and we make several times as many.
- 867 lemmas: 512 from obligations, 355 from CTGs. Lengths: 102 with
  1 literal, 344 with 2, 189 with 3, 94 with 4.
- They spread over 792 different latches (the top 100 latches cover
  only 50% of lemma literals), and almost every lemma has its own latch
  set (782 sets for 867 lemmas).
- Two-literal lemmas: 193 are "these two latches are never both 1",
  146 are implications, 5 are "never both 0". Only 8 are halves of an
  equivalence, so latch merging (scorr) can't capture them.
- Pushes: 2,824 tried, 1,915 failed (68%). 40 lemmas were learned
  again, up to 5 times.
- Obligations: 2,657, spread over every level, with full-state cubes of
  about 2,950 literals.

## Hypotheses

### H1. The invariant is mostly two-latch relations; mine them up front

*Claim.* The lemmas IC3 needs are mostly two-literal clauses that are
only inductive as a group. IC3 finds them one counterexample at a time,
in every frame. Simulation can propose them all at once, and Houdini can
prove the inductive subset before IC3 starts.

*Algorithm (src/mine.rs).* Sample reachable states. Keep every
two-literal clause over the 1,500 latches nearest the property that no
sample violates. Rank the clauses by support, run Houdini, and assume
the result in every frame (`Ic3::assume_invariant`).

*Prediction.* On JALR, half the lemmas or fewer, and solved within 120 s.

*Result: not testable on JALR yet.* Its input constraint kills nearly
every random run at step 0 (2 of 64 runs survive one step). Redrawing
inputs for failed runs does not help. SAT-sampled traces (64 traces of
16 frames from init) show none of the 1,500 latches changing. Parked
until the sampling reaches deeper states, for example from IC3's own
counterexamples to induction.

### H2. Lemmas that only hold together get stuck at staggered levels

*Claim.* A lemma at level j can only be pushed when the lemmas it
depends on are already at level j or higher. When a group of lemmas is
inductive only as a group and its members sit at different levels, each
one blocks the others, so they fail to push frame after frame and get
re-derived in the higher frames. Pushes fail 68% of the time on JALR.

*Algorithm.* Every N frames, run Houdini on all lemmas (plus the
invariant found so far). The subset that is inductive as a whole holds
in every reachable state, so it moves out of the frames into an
invariant that every frame from 1 up assumes. It needs no more pushing,
it strengthens every frame at once, and it becomes part of the final
certificate (`Ic3::houdini_to_inf`, `INFORMAL_IC3_HOUDINI=N`, Houdini
limited to a tenth of the time left).

*Prediction.* JALR solves within 200 s. On the sample there is no net
loss, and a gain on the CPU models.

*Result so far.* JALR with N=4: solved and certified in 144 s (Houdini at
frames 4, 8, 12 moved 2, 27, 27 lemmas). With N=2 it was still
unsolved at 180 s, so the gain is sensitive to N and may partly be
luck. sample60 at 30 s, with Houdini in every IC3 (ic3-mt runs it in all
4 workers): informal-ic3 20 -> 21, informal-ic3-mt 25 -> 22 (lost
cv32e40x-p128 and p363 and one x-epic bug), 0 wrong. At 30 s, Houdini's
share of the time costs more than it gains. The gap set (23 models rIC3
solves and we don't) at 300 s is the real test.

Gap set, informal-ic3, 300 s: baseline 1 (veer-p28 in 55 s), H2 with N=4
3 (JALR in 170 s, cv32e40x-p514 in 216 s, veer-p28 in 158 s), 0 wrong,
every SAFE certified. The prediction holds, modestly: 2 of 23 gained, and
one solve slowed down by 3x.

*Adopted (default):* N=4 in `informal-ic3`, and in one of the four
`informal-ic3-mt` workers (`INFORMAL_IC3_HOUDINI=N` overrides; 0 turns it
off). With only one worker running it, ic3-mt on sample60 at 30 s gave 22.
The ic3-mt counts in this session ranged from 22 to 25 between runs of the
same code, because a full benchmark was running on the same machine, and
the models that changed were all near the 30 s limit. A clean run is
needed before reading anything into that.

### Candidate hypotheses not yet tested

### H3. Push failures are caused by lemmas stuck one level lower

*Claim.* The state that breaks a failed push is usually excluded by a
lemma sitting at a lower level: H2's mechanism.

*Measurement.* The trace now records, for every failed push (`X` events),
the highest lower level whose lemmas exclude the counterexample state.
informal-ic3 for 90 s with H2 off:

| Model | Failed pushes | CTI excluded by a lower lemma | Of those, exactly one level lower |
|-------|---------------|-------------------------------|-----------------------------------|
| ILA_Piccolo_JALR_sanity | 1,909 | 1,854 (97%) | 1,804 |
| cv32e40x-p514 | 2,183 | 2,163 (99%) | 2,133 |

*Result: the mechanism is confirmed.* Pushes fail almost only because the
lemma that would block the counterexample sits one level lower and
can't move up either. That chain ends only at a set that is inductive as
a whole, which is what Houdini finds.

*Follow-up rejected: trigger Houdini on push failures.* Running Houdini
when more than half of a propagation's pushes failed (at most every 2
frames) fired early, at frames 5, 8 and 10 with few lemmas, and solved
neither model in 300 s. The fixed N=4 schedule, rerun under the same
load, solved both again (JALR 161 s, p514 199 s), with its big moves late
(p514: 318 lemmas at frame 12, 1,075 at frame 16). Houdini pays off once
the lemma pool is large, so a better trigger would look at pool size, not
failure rate. That's untested.

### H2b. Trigger Houdini on lemma-pool growth (prediction, 2026-11-11)

*Claim.* Houdini pays off once the lemma pool is large (H3's follow-up),
so it should run when the lemmas in the frames have doubled since its last
run (and number at least 100), not every 4 frames.

*Prediction.* Solves JALR and cv32e40x-p514 within 300 s, like N=4.
Single-IC3 sample60 at 30 s at least 21 (the N=4 count), with fewer
Houdini runs on models that solve early.

*Result: a tie on the hard models, slightly worse on the sample; not
adopted.* Quiet machine, informal-ic3 (`INFORMAL_IC3_HOUDINI=grow` against
`=4`):

| Set | N=4 | grow |
|-----|-----|------|
| sample60 at 30 s | 21 solved | 20 solved |
| gap set (23) at 300 s, 4 at a time | 4 solved | 4 solved |

Both give 0 wrong. On the gap set they solve different models: N=4 gets
JALR (110 s) and grow does not; grow gets ILA_Piccolo_BGEU (271 s) and N=4
does not; both get ILA_Piccolo_BEQ, cv32e40x-p514 and veer-p28, N=4 faster
on the last two. The prediction missed on JALR (solved alone in 214 s,
unsolved with 4 models running at once) and on the sample (20, not 21).
N=4 stays the default; `grow` stays as an option.

The union is 5 of 23, so the two schedules are complementary. An untested
follow-up: give `grow` to a second `informal-ic3-mt` worker. That needs
repeated runs, since ic3-mt results vary by 3 between runs of the same
code.

Also new since H2's first measurement: with the solver hot-loop fixes
(1bb2d5b) N=4 now solves 4 of the gap set, up from 3
(ILA_Piccolo_BEQ, 293 s).

### H2c. Both schedules in the 4-worker IC3 (prediction, 2026-11-11)

*Claim.* N=4 and `grow` solve different models (union 5 of 23 on the gap
set), and `informal-ic3-mt` takes the first worker to finish. With N=4 in
worker 0 and `grow` in worker 2 (`INFORMAL_IC3MT_GROW`), ic3-mt should get
closer to the union.

*Prediction.* Gap set at 300 s: at least 1 more model than the baseline
ic3-mt in the same session. sample60 at 30 s over 3 runs each: the mean
within 1 model of the baseline (no loss beyond noise). 0 wrong.

*Result: no gain; not adopted.* Quiet machine, `informal-ic3-mt`, 3 models
at a time:

| Set | Baseline | `grow` in worker 2 |
|-----|----------|--------------------|
| sample60 at 30 s, 3 runs | 24, 23, 23 | 22, 23, 24 |
| gap set (23) at 300 s, 1 run | 6 solved | 5 solved |

0 wrong in every run. The sample is the same within noise, as predicted.
The gap-set prediction (at least 1 more) missed: the variant gained
cv32e40x-p363 (48 s) and lost JALR and cv32e40x-p584 (298 s in the
baseline, so on the edge). One run each can't separate that from noise,
and there is no sign of a gain, so the switch was removed again.

What the run did show: the baseline `informal-ic3-mt` now solves 6 of the
23 gap models at 300 s (ILA_Piccolo_BEQ 23 s, BGEU 31 s, JALR 54 s,
veer-p28 39 s, cv32e40x-p033 254 s, p584 298 s). None of these were solved
by our portfolio in the 197-model run on 29d6d69. The changes since then
are H1/H4, the solver hot-loop fixes and UNSAFE certification.

### H6. Solver costs that grow with the number of queries (2026-11-11)

Found while tracing x-epic a16-p128 (rIC3: 37 s, 17 frames, about 17,000
lemmas, 545k solves at 62 us; ours unsolved). All three are in how IC3
uses `Mini`, not in generalisation.

| Cost | Evidence | Fix |
|------|----------|-----|
| Each query left a dead activation variable, a level-0 unit and dead clauses | 443k variables and a 143k-entry level-0 trail after 438k queries; clean-up and model copy were 25% of the time | `Sat::add_temp_clause` / `retire` (d22ac53): the clause and what was learned from it are removed, the variable is reused |
| Learned-clause limit grew 10% per reduction, carried across solves | 38,000 learned clauses of 14 literals, twice the lemmas' watchers | limit restarts at 2000 per solve (fecd0a4) |
| A query on F_i walked the watchers of every frame's lemmas | per-frame solvers were 1.7x faster per query | watch-list groups per level (fecd0a4) |

Results (single IC3, 300 s, six hard models, 3 at a time), solved: none
of the last two 1, limit only 1, groups only 4, both 3. Per query: JALR
4.1 -> 1.2 ms with groups; a16-p128 130 -> 83 us with the limit.
a16-p128 queries in 120 s: 325k before all three, about 970k after.

End to end the three are close to neutral on the broad sets (sample60
21/23 unchanged, genset 24 -> 23, gap set ic3-mt 6 = 6), with different
models won and lost: single IC3 is deterministic but its path is very
sensitive to which models the solver returns. The temp-clause change
alone lost 3 of the 4 gap models single IC3 solved before it; groups
brought them back.

The a16 family (6 models) is still unsolved at 300 s: frames 10-14 of
about 17, with 34,000-67,000 lemmas where rIC3 needs about 17,000. We
make about twice the lemmas per frame, 84% of them from CTGs. That is
the next question (lemma quality), not solver speed.

### H7. Is the CTG step where a16's extra lemmas come from? (2026-11-11)

*Claim.* On a16-p128, 84% of lemmas come from counterexamples to
generalisation (CTGs), and 76% of those are made during literal drops
that end up failing (new counters, `ctg lemmas:` in INFORMAL_IC3_STATS).
So the CTG step looked like the place to save lemmas or queries.

*Prediction.* Fewer or cheaper CTG lemmas reach a deeper frame in 120 s.

*Result: rejected, in every direction.* informal-ic3, a16-p128, 120 s
(default: frame 8, 14,700 lemmas, lemma length 7.3):

| Variant | Frame | Lemmas | Note |
|---------|-------|--------|------|
| no CTGs (`INFORMAL_IC3_CTGS=0`) | 7 | 10,400 | lemmas 9.3 literals long |
| 1 CTG per drop | 7 | 14,300 | |
| 6 CTGs per drop | 8 | 19,000 | |
| CTG depth 2 (`INFORMAL_IC3_CTG_DEPTH=2`) | 7 | 14,800 | |
| CTG lemmas keep their core, no drops | 7 | 28,700 | twice the lemmas |
| CTG lemmas get 3 drops at most | 8 | 21,700 | |

Weaker CTG lemmas cost more than they save, and more CTG effort buys
nothing. The default stays. A cache of failed-drop witnesses (H5's
cousin) was reasoned out, not built: a witness only fixes the cube's own
latches in the successor, so it cannot transfer to cubes over other
latches, and sibling drops of one cube never share a witness.

Where a16 stands against rIC3, factor by factor: lemmas per level 1.3-2x
theirs, queries per lemma 48 against 32, time per query 110 us against
62 us. No single cause; together about 5x. After the H6 fixes, 55% of
the time is unit propagation for SAT answers (88% of queries are SAT, 250
decisions each).

### H11. Feed the standing invariant continuously (prediction, 2026-11-12)

*Evidence.* cv32e40x-p658: rIC3 ends with 3,204 of its roughly 5,000
lemmas in its invariant frame and MIC input cubes of 5.7 literals. Ours,
with Houdini every 4 frames (H2), holds about 300 lemmas in the invariant
at frame 20 and its MIC input cubes are 12.7 literals. A stronger
invariant is in every frame, so every core is smaller and every MIC
cheaper.

*Claim.* The invariant should grow every frame, not every 4, and lemma
by lemma: after each propagation, a lemma that is inductive relative to
the invariant and itself joins it (one query per lemma; failures are
retried after the invariant has grown by an eighth).
`INFORMAL_IC3_PUSH_INF`.

*Prediction.* On cv32e40x-p658 and p299 at 300 s: the invariant holds at
least 1,000 lemmas at the end, MIC input cubes fall below 10 literals,
and at least one of the two is solved (neither is today).

*Result: rejected.* informal-ic3, 300 s: the sweep moved 163 lemmas on
p658 (2,939 queries) and 37 on p299 (2,545 queries); neither model was
solved, and MIC input cubes got longer, not shorter (17.8 and 13.1). Our
lemmas are not inductive one at a time relative to the invariant: they
only hold in groups (H2, H3), which is what Houdini is for. The code was
removed again.

Also rejected the same day (H10): asking again with an unsat core alone
to shrink it. On p658 it removed 262 literals over about 13,000 cores;
the cores are already stable.

What did help the CPU family the same day:

- **scorr could not finish.** Its refinement loop asked SAT for one
  counterexample at a time and peeled about one candidate per query
  (cv32e40x-p658: 3,080 candidates, 2,159 left after 90 s), so on these
  models no latch was ever merged. Now, before each SAT query, it
  refutes candidates without SAT: 64 random states in which every
  candidate equality holds, one step with random inputs, classes split
  on the result. p658: 3,080 candidates down to 176 in 0.02 s, finished
  in 0.8 s with 150 latches merged (rIC3: 148).
- **Group watch lists were dense.** One table per frame meant one cache
  miss per frame for every propagated literal (28% of propagate's time
  on p658). Sparse per-literal lists: 1.69 -> 0.70 ms per SAT answer.

p658 and p299 are still unsolved at 300 s. rIC3 proves p658 in 25 s with
66k solver calls at 240 us; we now do 313k at about 0.6 ms and are at
frame 25 of 26 with 13,000 lemmas against its 5,000. Lemma lengths are
bimodal (most 1-7 literals, a second group at 11-16), so the next
question is what the long ones are.

### H12. Promote an old lemma before deriving it again (prediction, 2026-11-12)

*Evidence.* In the traces, a lemma with exactly the same literals as an
earlier one is added again: 32% of all lemmas on cv32e40x-p658 (up to 8
times each), 21% on p299, 7% on JALR, 2% on a16. Most come from
obligations. The earlier copy sits at a lower level because its push
failed when it was tried (once per frame); by the time the obligation
comes back, the lemma holds one level up, and IC3 pays a full MIC (15 or
more SAT answers for a 15-literal cube) to arrive at the same clause.

*Claim.* Before generalising for an obligation at level i, look for
lemmas below level i whose literals all occur in the obligation's cube,
highest level first, and ask whether one is inductive at level i (one
query, usually UNSAT and cheap). If so, promote it and skip the MIC.
`Config::promote` (`INFORMAL_IC3_PROMOTE=0` turns it off).

*Prediction.* On p658 and p299 at 300 s: exact repeats fall below 5% of
lemmas, queries per lemma fall by at least a quarter, and p658 (which
reaches frame 25 of 26 today) is solved.

*Result so far: mixed, off by default.* informal-ic3, 300 s, neither
model solved (prediction missed):

| Model | Without | With `INFORMAL_IC3_PROMOTE=1` |
|-------|---------|-------------------------------|
| p299 | frame 19, 15,800 lemmas | frame 42, 8,200 lemmas |
| p658 | frame 25, 13,100 lemmas | frame 19, 5,700 lemmas, MIC cubes 25 literals |

Promotions succeeded on 8-9% of tries (852 of 11,058; 761 of 8,666), so
most tries are a wasted query. Exact repeats that did not come from a
promotion fell from 32% to 14% (p658) and from 21% to 16% (p299), not
below 5%: CTG lemmas repeat too, and the cube test only finds lemmas that
are subsets of the obligation's cube. p299 gets more than twice as deep
on half the lemmas; p658 goes the other way, into 755 lemmas of 20 or
more literals over datapath registers. Needs the hard set before any
decision.

### H13. Lemma groups that differ in one literal: derived predicates (prediction, 2026-11-12)

*Evidence.* In the lemma sets of our traces, many lemmas are equal except
for one literal (blocked cubes A+m1, A+m2, ..., A+mk):

| Model | Distinct lemmas | Such groups | Largest groups |
|-------|-----------------|-------------|----------------|
| x-epic a16-p128 | 29,007 | 6,927 (merging saves up to 9,481 lemmas) | 29, 14, 14 |
| cv32e40x-p658 | 2,433 | 792 | 36, 35, 32, 32, 32 |
| cv32e40x-p299 | 5,795 | 3,125 | 60, 53, 51, 48 |

A group of k says "A, and at least one of m1..mk, is unreachable": one
fact about a word (a 32-bit register differing from a value, on the CPU
models) that IC3 states as k clauses, each paid for with its own
obligation and MIC, and each pushed on its own. Pairs with two flipped
polarities (XOR-like) are rare on the CPU models (7 and 55) and common
on a16 (700). [PdrER](https://arxiv.org/abs/2505.18998) reports gains
from AND/XOR extension variables on HWMCC'24; the design below is ours,
from these counts.

*Claim.* A derived predicate x = m1 OR ... OR mk over latch literals,
usable as one literal in cubes, lets one lemma (block A+x) do the work
of the group. Stage A: detect groups of 4 or more at each propagation,
define x, and replace the group by one lemma (fewer clauses, one push
instead of k). Stage B: at the end of MIC, try replacing a literal that
belongs to some x by x itself; if the wider cube is still blocked, later
members of the group never need their own obligation. Certificates,
Houdini and lemma sharing see the expanded clauses, so the trusted base
does not change.

*Prediction.* Stage A alone: 20% fewer lemmas held at the same frame on
p658, p299 and a16-p128, and no loss on the seeded hard set. Stage B: at
least 2 more of the 29 hard-set models solved on average over 3 seeds.

*Result: no gain on average; off by default.* Built in 039b601
(`INFORMAL_IC3_DERIVE=1`). Seeded hard set (29 models, 300 s, 3 seeds, 3
models at a time, quiet machine; `scripts/ic3_seeds.py`):

| Engine | Baseline (0e9473b) | With predicates |
|--------|--------------------|-----------------|
| informal-ic3, solved per seed | 6, 6, 6 | 4, 9, 5 |
| informal-ic3, solved on some seed | 10 | 12 |
| informal-ic3-mt, solved per seed | 6, 7, 6 | 6, 6, 6 |
| informal-ic3-mt, solved on some seed | 8 | 8 |

Both predictions missed: the mean did not move. Clear moves (by 2 seeds
or more): single IC3 gained cv32e40x-p658 (0/3 -> 2/3, 29 s on one seed;
never solved by us before) and lost p299 (3/3 -> 1/3); ic3-mt lost p299
and p363. Stage A merges thousands of lemmas (p299: about 2,000 held at
frame 19 against 15,800 made without), and stage B widens 12-17% of
lemmas on the CPU models, but queries get slower (the predicates pull
their members into every query's domain) and the path changes. 0 wrong;
every certificate passed with predicates expanded. A correction to the
commit message of 039b601: the baseline single IC3 already solves p299 on
all 3 seeds since the scorr fix; it was not a first.

### H14. The runs are heavy-tailed: diversity beats any one setting (2026-11-13)

*Evidence, from the same four seeded runs.* A model is often solved in
30 s on one seed and not in 300 s on another (JALR with predicates: 13 s,
72 s, unsolved; p363 in ic3-mt: 14 s, 23 s, unsolved). Per run, each
configuration solves about 6 of the 29. Solved on at least one seed:
single IC3 10, ic3-mt 8, the two together 11, all four configurations
13. The three full portfolio runs of the week (199, 196, 195, 0 wrong)
have a union of 202, and their differences are mostly ic3-mt wins that
come and go (rocket_2067: 16 s in one run, unsolved in the next).

*Claim.* On these models the choice of path matters more than any
single heuristic we tested this week. ic3-mt's four workers differ in
generalisation settings but start from the same seed and share lemmas.
Giving the workers different seeds, and one of them predicates, should
move ic3-mt from about 6 toward the union.

*Prediction.* ic3-mt with per-worker seeds and predicates in one worker:
mean of at least 8 on the seeded hard set (6.3 today), 0 wrong.

*Result: a small gain, adopted as the default; the prediction (8) was
missed.* Seeded hard set, informal-ic3-mt, 300 s, 3 seeds:

| | Four settings, one seed | Per-worker seeds, predicates in worker 2 |
|---|---|---|
| Solved per seed | 6, 7, 6 (mean 6.3) | 6, 8, 7 (mean 7.0) |
| Solved on some seed | 8 | 10 |

Clear moves: cv32e40x-p607 gained (0/3 -> 3/3, about 150 s), cv32e40x-p033
lost (2/3 -> 0/3). Also solved on one seed each for the first time in
ic3-mt: p514, p568, veer_axi-p06. sample60 at 30 s: 23 and 23, 0 wrong
both ways. With three seeds the difference in the mean is within what
seeds alone produce, so this is weak evidence; it is the default because
the sample does not lose and the hard set does not either
(`INFORMAL_IC3MT_DIVERSE=0` turns it off). The lemma pool may be why the
gain is small: the workers share lemmas, which pulls their paths
together again.

### H15. Reuse a proven invariant across sibling properties (prediction, 2026-11-14)

From the project's idea list. Checked here: all 86
cv32e40x models have the same AIGER header and two of them differ in 6
bytes; veer (4), veer_axi (9) and x-epic a16 are families of the same
kind. Each file is one property of one design.

*Claim.* A certified invariant of one property holds in every reachable
state of the design, so a sibling can assume the part of it that is
inductive on the sibling's own cone. Store: per design (a hash of the
model without its bad outputs), the clauses of every certified
invariant, over latch indices of the model as loaded
(`INFORMAL_INV_STORE=<dir>`). Import: map the clauses through the
sibling's latch map, keep what Houdini proves inductive there, and give
it to IC3 as a standing invariant. The final certificate checks
everything again, so the store is not trusted.

*Reporting.* This crosses model files, so it is a separate mode: the
clean 319-model score never uses a store.

*Prediction.* Seed the store with a 60 s pass of informal-ic3 over the
four families, then run the hard set (29 models, 300 s, 3 seeds) with
the store on: single IC3 solves at least 12 on average (6.0 without).

*Result: the prediction held; the largest gain of any hypothesis so
far.* Built in b96f4d5 and 287ba4f (`INFORMAL_INV_STORE`). Protocol: a
seeding pass (informal-ic3-mt, 60 s, 131 models of the cv32e40x, veer,
veer_axi and x-epic families, store read-write), then the hard set with
the store read-only (29 models, 300 s, 3 seeds, 3 at a time).

| Engine | Without a store | With the store |
|--------|-----------------|----------------|
| informal-ic3, solved per seed | 6, 6, 6 | 14, 10, 13 (mean 12.3) |
| informal-ic3, solved on every seed | 3 | 10 |
| informal-ic3-mt, solved per seed | 6, 8, 7 | 16, 15, 14 (mean 15.0) |
| informal-ic3-mt, solved on every seed | 4 | 14 |

No model was lost in either engine; 0 wrong, no conflicting verdicts
between seeds, every SAFE certified on the loaded model. Clearly gained
(by 2 seeds or more): single IC3 p033, p039, p478, p607, p649, p658;
ic3-mt p033, p039, p478, p568, p584, p649, p658, veer_axi-p06. Three of
the solved models (p607, p658, veer-p28) had been solved in the seeding
pass itself, so in the measured run they imported their own proof (8-14
s); without those three, single IC3 is 9.3 against 5.0 on the other 26.
cv32e40x-p299 went from 82-154 s to 8 s on siblings' clauses alone, and
p039, p478 and p649 had never been solved by us.

What the store held: one file of 6,054 clauses for the cv32e40x design
after the seeding pass (35 of 86 properties proved in 60 s each). The
seeding pass itself solved p610 (10 s) and p658 (55 s), which the clean
300 s portfolio never had. The gain is all in cv32e40x, veer and
veer_axi; none of the six a16 models moved (their group of 16 siblings
gave too little), and the ILA models have no siblings.

A first run was void and is worth remembering: the design hash then
left out only the bad literal, but these files share the bad literal
and differ in the AND gates under it, so nothing was ever imported.

Not part of the clean 319-model score: a result with a store depends
on which siblings were solved before.

### H16. Restarts that keep the inductive core (prediction, 2026-11-14)

From the project's idea list, built on H14's data:
single IC3 solves about 6 of the 29 hard models per seed but 10 on some
seed, and solve times on one model range from 13 s to more than 300 s.

*Claim.* A run that is on a bad path should give up early and try
another, keeping what it proved. `INFORMAL_IC3_RESTART=<seconds>`: IC3
runs in slices of that length, doubling each time (20, 40, 80, 160 fill
300 s). When a slice ends without a verdict, Houdini keeps the lemmas
that are inductive together (with the invariant so far), and a new IC3
starts on another seed with that core as its standing invariant.

*Prediction.* Hard set, single IC3, 300 s, 3 seeds, base 20 s: a mean
of at least 8 (6.0 without restarts), and no model that is solved on
all three seeds today is lost.

*Result:* pending.

### H17. Fold the free-running clock (prediction, 2026-11-14)

*Evidence (static checks on the benchmark files).* Every cv32e40x, veer
and veer_axi model has a latch in the property's cone that starts at 0
and toggles every step (`next = NOT self`): a clock modelled as a latch.
That is 99 models and 56 of the ones we do not solve. In the invariant
our runs stored for cv32e40x, 15% of the clauses carry the clock literal
and 25% a latch derived from it. Every design cycle costs IC3 two
frames.

*Claim.* With an unconditional toggle the reachable states split into
two phases that alternate, so the model can be checked two steps at a
time on the initial phase (`preprocess::fold_clock`): next-state
functions applied twice, bad in either half-step, the clock a constant.
`INFORMAL_FOLD=1`, in the IC3 engines. The certificate stays on the
model as loaded: a folded invariant is checked as a 2-step invariant
there (`certify::check_folded`), and a counterexample is searched at
twice the folded depth or one more.

*Prediction (written before the first run).* On cv32e40x-p658 and p299
the folded model has at least 20% fewer latches after scorr, IC3 closes
at about half the frame number, and makes at least 40% fewer lemmas.
Seeded hard set, single IC3, no store: a mean of at least 9 (6.0
today).

*Result:* pending.

- **H4. The obligation queue revisits the same region.** 2,657
  obligations, rising level by level. Measure how many obligations a new
  lemma would already have blocked. If that's high, generalising against
  the queue (blocking several obligations with one lemma) cuts queries.
- **H5. Short lemmas, wasted drops.** MIC starts from cores of about 4.7
  literals and ends at 3.6, with 84% of drop attempts failing. A cheap
  ternary-simulation pre-check for each drop could skip most failing SAT
  calls.
