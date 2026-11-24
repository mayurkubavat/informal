// C++ proof-tracing Craig interpolator over CaDiCaL::Tracer (McMillan CAV'03).
//
// Runs CaDiCaL in `plain` mode (pure RUP/CDCL proofs with LRAT antecedents),
// prunes the derivation DAG backward from the derived empty clause, and
// constructs a hash-consed McMillan interpolant AIG over shared (A ∩ B)
// DIMACS variables.

#include "cadical.hpp"
#include "tracer.hpp"

#include <algorithm>
#include <cstdint>
#include <cstdlib>
#include <unordered_map>
#include <unordered_set>
#include <vector>

namespace {

constexpr uint32_t AIG_FALSE = 0;
constexpr uint32_t AIG_TRUE = 1;

inline uint32_t aig_neg(uint32_t l) { return l ^ 1u; }

struct StoredClause {
  bool present = false;
  bool is_original = false;
  uint8_t part = 0; // 0 = A, 1 = B
  int witness = 0;
  std::vector<int> lits;
  std::vector<int64_t> antecedents;
};

struct InterpTerminator final : public CaDiCaL::Terminator {
  void *state = nullptr;
  int (*fn)(void *) = nullptr;
  bool terminate() override { return fn ? (fn(state) != 0) : false; }
};

struct InterpTracer final : public CaDiCaL::Tracer {
  uint8_t current_part = 0;
  int64_t empty_id = 0;
  bool overflow = false;
  static constexpr size_t MAX_CLAUSES = 2000000;

  std::vector<StoredClause> clauses;
  std::vector<uint8_t> var_mask; // bit 0 = in A, bit 1 = in B

  void ensure_id(int64_t id) {
    if (id <= 0 || static_cast<size_t>(id) > MAX_CLAUSES) {
      overflow = true;
      return;
    }
    if (static_cast<size_t>(id) >= clauses.size()) {
      clauses.resize(static_cast<size_t>(id) + 1);
    }
  }

  void mark_vars(const std::vector<int> &c, uint8_t bit) {
    for (int l : c) {
      size_t v = static_cast<size_t>(std::abs(l));
      if (v >= var_mask.size()) {
        var_mask.resize(v + 1, 0);
      }
      var_mask[v] |= bit;
    }
  }

  void add_original_clause(int64_t id, bool, const std::vector<int> &c,
                           bool) override {
    ensure_id(id);
    if (overflow) return;
    auto &slot = clauses[static_cast<size_t>(id)];
    slot.present = true;
    slot.is_original = true;
    slot.part = current_part;
    slot.witness = 0;
    slot.lits = c;
    slot.antecedents.clear();
    mark_vars(c, current_part == 0 ? 1u : 2u);
    if (c.empty() && empty_id == 0) {
      empty_id = id;
    }
  }

  void add_derived_clause(int64_t id, bool, int w, const std::vector<int> &c,
                          const std::vector<int64_t> &ant) override {
    ensure_id(id);
    if (overflow) return;
    auto &slot = clauses[static_cast<size_t>(id)];
    slot.present = true;
    slot.is_original = false;
    slot.part = 0;
    slot.witness = w;
    slot.lits = c;
    slot.antecedents = ant;
    if (c.empty()) {
      empty_id = id;
    }
  }

  void report_status(int status, int64_t id) override {
    if (status == 20 && id > 0) {
      empty_id = id;
    }
  }
};

struct AigBuilder {
  uint32_t num_shared = 0;
  std::vector<uint32_t> and_lhs;
  std::vector<uint32_t> and_rhs;
  std::unordered_map<uint64_t, uint32_t> hash;

  uint32_t mk_and(uint32_t a, uint32_t b) {
    if (a == AIG_FALSE || b == AIG_FALSE) return AIG_FALSE;
    if (a == AIG_TRUE) return b;
    if (b == AIG_TRUE || a == b) return a;
    if (a == aig_neg(b)) return AIG_FALSE;
    if (a > b) std::swap(a, b);
    uint64_t key = (static_cast<uint64_t>(a) << 32) | static_cast<uint64_t>(b);
    auto it = hash.find(key);
    if (it != hash.end()) return it->second;
    uint32_t idx = static_cast<uint32_t>(and_lhs.size());
    and_lhs.push_back(a);
    and_rhs.push_back(b);
    uint32_t lit = 2u * (1u + num_shared + idx);
    hash.emplace(key, lit);
    return lit;
  }

  uint32_t mk_or(uint32_t a, uint32_t b) {
    return aig_neg(mk_and(aig_neg(a), aig_neg(b)));
  }
};

struct InterpHandle {
  CaDiCaL::Solver solver;
  InterpTracer tracer;
  InterpTerminator term;
  bool connected = false;
  int last_status = 0;

  // Extracted interpolant storage owned until free.
  std::vector<int32_t> shared_vars;
  std::vector<uint32_t> and_lhs;
  std::vector<uint32_t> and_rhs;
  uint32_t root = AIG_FALSE;

  InterpHandle() {
    solver.configure("plain");
    solver.set("quiet", 1);
    solver.connect_proof_tracer(&tracer, /*antecedents=*/true,
                                /*finalize_clauses=*/false);
    connected = true;
  }

  ~InterpHandle() {
    if (connected) {
      solver.disconnect_proof_tracer(&tracer);
    }
  }
};

} // namespace

extern "C" {

void *informal_interp_new() { return new InterpHandle(); }

void informal_interp_free(void *ptr) {
  delete static_cast<InterpHandle *>(ptr);
}

void informal_interp_set_terminate(void *ptr, void *state,
                                   int (*fn)(void *)) {
  auto *h = static_cast<InterpHandle *>(ptr);
  h->term.state = state;
  h->term.fn = fn;
  h->solver.connect_terminator(fn ? &h->term : nullptr);
}

void informal_interp_reserve(void *ptr, int n) {
  auto *h = static_cast<InterpHandle *>(ptr);
  if (n > 0) {
    h->solver.declare_more_variables(n);
  }
}

void informal_interp_add_clause(void *ptr, int part, const int *lits,
                                size_t len) {
  auto *h = static_cast<InterpHandle *>(ptr);
  h->tracer.current_part = (part == 0) ? 0u : 1u;
  // Also record variables even if a clause is tautological and dropped by CaDiCaL.
  uint8_t bit = (part == 0) ? 1u : 2u;
  for (size_t i = 0; i < len; ++i) {
    size_t v = static_cast<size_t>(std::abs(lits[i]));
    if (v >= h->tracer.var_mask.size()) {
      h->tracer.var_mask.resize(v + 1, 0);
    }
    h->tracer.var_mask[v] |= bit;
    h->solver.add(lits[i]);
  }
  h->solver.add(0);
}

int informal_interp_solve(void *ptr) {
  auto *h = static_cast<InterpHandle *>(ptr);
  h->last_status = h->solver.solve();
  return h->last_status;
}

int informal_interp_val(void *ptr, int lit) {
  auto *h = static_cast<InterpHandle *>(ptr);
  return h->solver.val(lit);
}

// Extract McMillan interpolant AIG after UNSAT (`status == 20`).
// Returns 0 on success, non-zero on error/unsupported proof step.
int informal_interp_extract(void *ptr, const int32_t **out_shared,
                            size_t *out_num_shared, const uint32_t **out_lhs,
                            const uint32_t **out_rhs, size_t *out_num_ands,
                            uint32_t *out_root) {
  auto *h = static_cast<InterpHandle *>(ptr);
  if (h->last_status != 20 || h->tracer.overflow || h->tracer.empty_id <= 0) {
    return 1;
  }
  const auto &clauses = h->tracer.clauses;
  const size_t empty_idx = static_cast<size_t>(h->tracer.empty_id);
  if (empty_idx >= clauses.size() || !clauses[empty_idx].present) {
    return 2;
  }

  // 1. Prune proof DAG backward from empty_id.
  std::vector<uint8_t> needed(clauses.size(), 0);
  std::vector<int64_t> stack;
  stack.push_back(h->tracer.empty_id);
  needed[empty_idx] = 1;
  std::vector<int64_t> active;

  while (!stack.empty()) {
    int64_t id = stack.back();
    stack.pop_back();
    active.push_back(id);
    const auto &cl = clauses[static_cast<size_t>(id)];
    if (!cl.is_original) {
      if (cl.witness != 0 || cl.antecedents.empty()) {
        return 3;
      }
      for (int64_t ant : cl.antecedents) {
        if (ant <= 0 || static_cast<size_t>(ant) >= clauses.size() ||
            !clauses[static_cast<size_t>(ant)].present) {
          return 4;
        }
        if (!needed[static_cast<size_t>(ant)]) {
          needed[static_cast<size_t>(ant)] = 1;
          stack.push_back(ant);
        }
      }
    }
  }
  std::sort(active.begin(), active.end());

  // 2. Build shared variable table (`var_mask[v] == 3` -> in both A and B).
  h->shared_vars.clear();
  std::vector<uint32_t> var_to_aig(h->tracer.var_mask.size(), 0);
  for (size_t v = 1; v < h->tracer.var_mask.size(); ++v) {
    if (h->tracer.var_mask[v] == 3u) {
      uint32_t idx = static_cast<uint32_t>(h->shared_vars.size());
      h->shared_vars.push_back(static_cast<int32_t>(v));
      var_to_aig[v] = 2u * (1u + idx);
    }
  }

  AigBuilder aig;
  aig.num_shared = static_cast<uint32_t>(h->shared_vars.size());

  auto lit_to_shared_aig = [&](int l) -> uint32_t {
    size_t v = static_cast<size_t>(std::abs(l));
    uint32_t base = (v < var_to_aig.size()) ? var_to_aig[v] : 0u;
    if (base == 0u) return 0u;
    return (l > 0) ? base : aig_neg(base);
  };

  auto is_a_local = [&](int v) -> bool {
    size_t uv = static_cast<size_t>(v);
    return uv < h->tracer.var_mask.size() && h->tracer.var_mask[uv] == 1u;
  };

  // 3. Forward McMillan interpolation along sorted active clause IDs.
  std::vector<uint32_t> itp(clauses.size(), AIG_FALSE);
  std::unordered_set<int> cur_lits;
  cur_lits.reserve(64);

  for (int64_t id : active) {
    const auto &cl = clauses[static_cast<size_t>(id)];
    if (cl.is_original) {
      if (cl.part == 0) {
        uint32_t acc = AIG_FALSE;
        for (int l : cl.lits) {
          uint32_t sl = lit_to_shared_aig(l);
          if (sl != 0u) {
            acc = aig.mk_or(acc, sl);
          }
        }
        itp[static_cast<size_t>(id)] = acc;
      } else {
        itp[static_cast<size_t>(id)] = AIG_TRUE;
      }
      continue;
    }

    // Derived RUP clause: resolve antecedents from back to front (matching CaDiCaL LRAT order).
    const auto &ant = cl.antecedents;
    int64_t last_id = ant.back();
    const auto &last_cl = clauses[static_cast<size_t>(last_id)];
    cur_lits.clear();
    for (int l : last_cl.lits) {
      cur_lits.insert(l);
    }
    uint32_t cur_itp = itp[static_cast<size_t>(last_id)];

    for (size_t k = ant.size() - 1; k > 0; --k) {
      int64_t aid = ant[k - 1];
      const auto &acl = clauses[static_cast<size_t>(aid)];
      int pivot = 0;
      for (int l : acl.lits) {
        if (cur_lits.count(-l) != 0) {
          pivot = l;
          break;
        }
      }
      if (pivot == 0) {
        // Antecedent not needed for binary resolution step (subsumed / redundant).
        continue;
      }
      cur_lits.erase(-pivot);
      for (int l : acl.lits) {
        if (l != pivot) {
          cur_lits.insert(l);
        }
      }
      uint32_t ant_itp = itp[static_cast<size_t>(aid)];
      if (is_a_local(std::abs(pivot))) {
        cur_itp = aig.mk_or(cur_itp, ant_itp);
      } else {
        cur_itp = aig.mk_and(cur_itp, ant_itp);
      }
    }
    itp[static_cast<size_t>(id)] = cur_itp;
  }

  h->and_lhs = std::move(aig.and_lhs);
  h->and_rhs = std::move(aig.and_rhs);
  h->root = itp[empty_idx];

  *out_shared = h->shared_vars.data();
  *out_num_shared = h->shared_vars.size();
  *out_lhs = h->and_lhs.data();
  *out_rhs = h->and_rhs.data();
  *out_num_ands = h->and_lhs.size();
  *out_root = h->root;
  return 0;
}

} // extern "C"
