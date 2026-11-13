# Literature check, 2026-11-12

A survey of recent work on IC3/PDR and hardware model checking, done to
choose the next engine steps. Abstracts, result tables and the HWMCC'25
slides were read; the papers' full texts were not, so every number here is
the authors' claim, not something we measured. Anything we adopt gets its
own prediction and before/after run in `docs/ic3-research.md`, and we
build our own version from our own traces.

## Where the field stands: HWMCC'25

Source: [HWMCC'25 slides](https://hwmcc.github.io/2025/hwmcc25slides.pdf)
(FMCAD, October 2025).

- Setup: 3600 s wall clock, 120 GB, 16 cores / 32 threads per job. Our
  runs use 300 s and 14 GB, so our counts are not comparable with these.
- Bit-level safety track, 330 benchmarks (the word-level BV benchmarks
  translated to AIGER): rIC3 274, rIC3-multi 271, super-prove 255,
  Supercar 255, aIC3 250, AVY 245, NCIP 135.
- **Certificates are mandatory.** SAT answers need an AIGER witness,
  UNSAT answers a Certifaiger certificate, and an answer only counts if
  validation succeeds. super-prove fell from 255 to 80 certified, AVY
  from 245 to 226.
- Word-level BV track: rIC3 267 (by bit-blasting), Supercar 248,
  Btor2-Select 240, AVR 233, Pono 212. Arrays track: Pono leads.
- A new bit-level liveness track (264 benchmarks): nuXmv 220.

What it means for us: bit-level IC3 is still what wins, including on
word-level benchmarks, so the focus on our own IC3 is right. Our SAFE and
UNSAFE verdicts are certified internally (D8) but not in the formats the
competition validates.

## Extended resolution in PDR (PdrER)

Source: Luka and Vizel, [Property Directed Reachability with Extended
Resolution](https://arxiv.org/abs/2505.18998) (2025).

- Adds auxiliary variables defined over existing literals (x = a AND b,
  x = a XOR b, half adders), found by matching pairs of lemmas that
  differ in one literal, and rewrites lemmas over them. Lemmas over the
  new variables are only used where they can be checked.
- Reported solved, PdrER against their PDR: HWMCC'19 236 / 233, HWMCC'20
  236 / 231, **HWMCC'24 188 / 175**. Their example: an invariant of over
  65,000 clauses becomes about 5,000.
- Costs: finding variables that do not help; some instances get slower.

Why it matters here: it matches two things in our traces. x-epic a16
needs 17,000 lemmas even in rIC3 and 34,000-67,000 in ours; cv32e40x-p658
produces hundreds of 15-20-literal lemmas over datapath registers. Both
look like relations (equalities, comparisons) that clauses over single
bits can only enumerate. HWMCC'24 is also the set where the paper's gain
is largest, and it is our benchmark.

## Adaptive generalisation (A-IC3) and lemma prediction

Sources: [A-IC3](https://arxiv.org/abs/2604.21688) (2026);
[Predicting Lemmas in Generalization of IC3](https://arxiv.org/abs/2411.12749)
(2024); [LeGend](https://arxiv.org/abs/2602.24010) (2026);
[AutoPDR](https://arxiv.org/abs/2603.25048) (2026);
[Certificate-Aware PDR](https://arxiv.org/abs/2605.16472) (2026).

- A-IC3 picks the CTG setting per generalisation with a contextual
  bandit (7 arms; features such as CTI depth, cube size, queue length).
  Reported on 914 instances at 3600 s in rIC3: 603 solved without CTG,
  629 with CTG, 653 with the bandit. Its two failure modes are ours too:
  over-general lemmas that cannot be pushed, and weak lemmas.
- Lemma prediction from counterexamples to propagation (rIC3's authors)
  guesses a minimal lemma before dropping literals.
- LeGend and AutoPDR train models (graph embeddings of the circuit) to
  seed lemmas or pick solver settings; LeGend reports 166 -> 181 of 200
  with IC3ref. Both need a training set and an offline pipeline.
- Certificate-Aware PDR keeps a learned ranker outside the trusted base,
  which is the rule D11 already sets for us.

Why it matters here: our H7 found no single CTG setting that helps a16,
which fits the idea that the right setting varies within a run. It is a
second-tier lever for us: the 4-worker IC3 already runs four settings at
once.

## What we take from it

1. Auxiliary variables for lemmas, designed from our own traces (which
   relations our long lemmas enumerate), with the certificate checker
   taught about the definitions.
2. Certificates in the competition's formats (AIGER witness,
   Certifaiger), so outside checkers can validate our verdicts.
3. Per-generalisation choice of CTG effort, after 1.
4. Not now: learned lemma seeding or configuration prediction.
