# Deep Research prompt: FormalDV decisions review

For Gemini Deep Research (Argon model). Paste everything below the line.
Attach or paste `docs/decisions.md` first so the model sees our decisions.

---

I am building FormalDV, an internal formal verification tool for digital
hardware: a Rust orchestrator over a portfolio of model checkers, with a
SystemVerilog front end (slang), Btor2/AIGER models, in-house SAT-based
engines (BMC, k-induction, IC3/PDR with lemma sharing, Houdini-strengthened
k-induction, word-level BMC on Bitwuzla), a JasperGold/VC Formal Tcl
compatibility shell, and a Tauri desktop UI. Benchmark: HWMCC'24 bit-level
(319 models; we solve 204 in 300 s with no wrong verdicts, level with rIC3's
portfolio). The attached `decisions.md` lists each design decision, its open
issues and ideas.

Do a deep, source-cited review (papers, tool repos, HWMCC reports, vendor
documentation) and answer:

1. **Engines and algorithms.** What has been published since 2020 that beats
   or complements rIC3, ABC pdr, Pono, AVR, CAR and Avy on HWMCC-style
   bit-level and word-level safety? Cover IC3 generalisation (CTG, EXCTG,
   DynAMic, parent lemmas), lemma-sharing parallel IC3, interpolation,
   proof-based abstraction, predicate abstraction (IC3IA), k-induction
   variants, and SAT preprocessing. For each, give the core idea, reported
   gains, implementation difficulty and whether source exists.
2. **Liveness and arrays.** Best practice for liveness (liveness-to-safety,
   k-liveness, ranking functions) and for arrays/memories in Btor2, including
   witness and certificate formats.
3. **Soundness and certificates.** Current state of Certifaiger, BtorSim,
   DRAT/LRAT proof logging and verified checkers; how to certify a safe verdict
   from IC3 cheaply.
4. **Portfolio and AI.** Published work on algorithm selection and scheduling
   for model checking (features, learned selectors, time-sliced schedules),
   and on LLMs for property/invariant generation where results are verified by
   the solver. What actually worked, with numbers.
5. **Front end and SVA.** The most practical route to complete SVA support
   (sequences, repetition, until, liveness operators): existing open source
   (slang, Yosys, sv2v, Surelog/UHDM, SymbiYosys, others), semantics pitfalls
   and how commercial tools handle them. Evaluate Verific as an optional
   add-on.
6. **Architecture review.** Critique each decision in `decisions.md`
   (Rust plus C++ FFI, Btor2 plus AIG, slang-first with Yosys fallback, Tcl
   compatibility, Tauri UI, pinned third-party tools with local patches only).
   Name the alternatives a senior formal-tool architect would consider and
   the trade-offs. Say which decisions you would change and why.
7. **Math ideas.** Suggest mathematical or cross-field techniques not yet in
   mainstream hardware model checking that could pay off (for example
   abstract interpretation, CEGAR variants, symmetry and compositional
   reasoning, SyGuS invariant synthesis, SMT-based learning), with an
   assessment of feasibility.
8. **Priorities.** A ranked list of the ten changes most likely to raise the
   HWMCC solved count or trustworthiness of verdicts, each with effort
   estimate, expected gain and the experiment to validate it (state a
   prediction first, then how to test it).

Output: a structured report with a summary table up front, then sections 1-8,
citations as links, and clear separation of established results from your
speculation.
