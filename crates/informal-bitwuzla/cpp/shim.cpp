// A handle-based C interface over Bitwuzla's C++ API, shaped for Btor2:
// terms and sorts are small integers, operators are named as in Btor2, and
// bit-vectors of width 1 stand in for Booleans (comparisons come back as
// bv1, conditions go in as bv1), as Btor2 does.

#include <bitwuzla/cpp/bitwuzla.h>

#include <atomic>
#include <chrono>
#include <cstring>
#include <memory>
#include <stdexcept>
#include <string>
#include <vector>

using namespace bitwuzla;

namespace {

struct Stop : Terminator {
    std::chrono::steady_clock::time_point deadline;
    const std::atomic<bool>* cancel = nullptr;
    bool terminate() override {
        return (cancel && cancel->load()) || std::chrono::steady_clock::now() >= deadline;
    }
};

struct Ctx {
    TermManager tm;
    std::unique_ptr<Bitwuzla> bz;
    std::vector<Sort> sorts;
    std::vector<Term> terms;
    Stop stop;
    std::string error;
    std::string last_value;

    int add(const Term& t) {
        terms.push_back(t);
        return (int)terms.size() - 1;
    }
    Term one() { return tm.mk_bv_one(tm.mk_bv_sort(1)); }
    Term zero() { return tm.mk_bv_zero(tm.mk_bv_sort(1)); }
    Term to_bv(const Term& b) { return tm.mk_term(Kind::ITE, {b, one(), zero()}); }
    Term to_bool(const Term& t) {
        if (t.sort().is_bool())
            return t;
        return tm.mk_term(Kind::EQUAL, {t, one()});
    }
    // Checked handle lookups: a bad handle from the Rust side throws
    // (caught at the C boundary) instead of reading out of bounds.
    const Term& term(int i) const {
        if (i < 0 || (size_t)i >= terms.size())
            throw std::out_of_range("bad term handle " + std::to_string(i));
        return terms[i];
    }
    const Sort& sort(int i) const {
        if (i < 0 || (size_t)i >= sorts.size())
            throw std::out_of_range("bad sort handle " + std::to_string(i));
        return sorts[i];
    }
};

// Run `f` at the C boundary: a C++ exception becomes -1 with the message
// in the context's error (no exception may cross into Rust).
template <class F>
int guarded(Ctx* c, F f) {
    try {
        return f();
    } catch (const std::exception& e) {
        c->error = e.what();
        return -1;
    } catch (...) {
        c->error = "unknown C++ exception";
        return -1;
    }
}

} // namespace

extern "C" {

void* informal_bw_new_opts(
    int64_t timeout_ms,
    const std::atomic<bool>* cancel,
    int produce_models,
    int produce_unsat_assumptions) {
    auto* c = new Ctx();
    Options opts;
    opts.set(Option::PRODUCE_MODELS, produce_models != 0);
    opts.set(Option::PRODUCE_UNSAT_ASSUMPTIONS, produce_unsat_assumptions != 0);
    c->bz = std::make_unique<Bitwuzla>(c->tm, opts);
    c->stop.deadline = std::chrono::steady_clock::now() + std::chrono::milliseconds(timeout_ms);
    c->stop.cancel = cancel;
    c->bz->configure_terminator(&c->stop);
    return c;
}

void* informal_bw_new(int64_t timeout_ms, const std::atomic<bool>* cancel) {
    return informal_bw_new_opts(timeout_ms, cancel, 0, 0);
}

void informal_bw_free(void* p) {
    delete static_cast<Ctx*>(p);
}

const char* informal_bw_error(void* p) {
    return static_cast<Ctx*>(p)->error.c_str();
}

int informal_bw_bv_sort(void* p, uint64_t w) {
    auto* c = static_cast<Ctx*>(p);
    return guarded(c, [&] {
        c->sorts.push_back(c->tm.mk_bv_sort(w));
        return (int)c->sorts.size() - 1;
    });
}

int informal_bw_array_sort(void* p, int idx, int elem) {
    auto* c = static_cast<Ctx*>(p);
    return guarded(c, [&] {
        c->sorts.push_back(c->tm.mk_array_sort(c->sort(idx), c->sort(elem)));
        return (int)c->sorts.size() - 1;
    });
}

int informal_bw_const(void* p, int sort, const char* name) {
    auto* c = static_cast<Ctx*>(p);
    return guarded(c, [&] { return c->add(c->tm.mk_const(c->sort(sort), name ? name : "")); });
}

// Bit string, most significant bit first.
int informal_bw_value(void* p, int sort, const char* bits) {
    auto* c = static_cast<Ctx*>(p);
    return guarded(c, [&] { return c->add(c->tm.mk_bv_value(c->sort(sort), bits, 2)); });
}

int informal_bw_const_array(void* p, int sort, int value) {
    auto* c = static_cast<Ctx*>(p);
    return guarded(c, [&] { return c->add(c->tm.mk_const_array(c->sort(sort), c->term(value))); });
}

// A Btor2 operator applied to terms; `i0`, `i1` are its numeric parameters
// (slice bounds, extension width). Returns -1 and sets the error message
// for an unknown operator or a sort error.
int informal_bw_op(void* p, const char* op, int n, const int* args, uint64_t i0, uint64_t i1) {
    auto* c = static_cast<Ctx*>(p);
    std::vector<Term> a;
    try {
        for (int i = 0; i < n; i++)
            a.push_back(c->term(args[i]));
    } catch (const std::exception& e) {
        c->error = std::string(op) + ": " + e.what();
        return -1;
    }
    std::string o(op);
    auto& tm = c->tm;
    auto bin = [&](Kind k) { return tm.mk_term(k, {a[0], a[1]}); };
    auto un = [&](Kind k) { return tm.mk_term(k, {a[0]}); };
    auto pred = [&](Kind k) { return c->to_bv(bin(k)); };
    try {
        Term r;
        if (o == "not") r = un(Kind::BV_NOT);
        else if (o == "inc") r = un(Kind::BV_INC);
        else if (o == "dec") r = un(Kind::BV_DEC);
        else if (o == "neg") r = un(Kind::BV_NEG);
        else if (o == "redand") r = un(Kind::BV_REDAND);
        else if (o == "redor") r = un(Kind::BV_REDOR);
        else if (o == "redxor") r = un(Kind::BV_REDXOR);
        else if (o == "iff" || o == "xnor") r = bin(Kind::BV_XNOR);
        else if (o == "implies") r = tm.mk_term(Kind::BV_OR, {tm.mk_term(Kind::BV_NOT, {a[0]}), a[1]});
        else if (o == "eq") r = pred(Kind::EQUAL);
        else if (o == "neq") r = pred(Kind::DISTINCT);
        else if (o == "sgt") r = pred(Kind::BV_SGT);
        else if (o == "ugt") r = pred(Kind::BV_UGT);
        else if (o == "sgte") r = pred(Kind::BV_SGE);
        else if (o == "ugte") r = pred(Kind::BV_UGE);
        else if (o == "slt") r = pred(Kind::BV_SLT);
        else if (o == "ult") r = pred(Kind::BV_ULT);
        else if (o == "slte") r = pred(Kind::BV_SLE);
        else if (o == "ulte") r = pred(Kind::BV_ULE);
        else if (o == "and") r = bin(Kind::BV_AND);
        else if (o == "nand") r = bin(Kind::BV_NAND);
        else if (o == "nor") r = bin(Kind::BV_NOR);
        else if (o == "or") r = bin(Kind::BV_OR);
        else if (o == "xor") r = bin(Kind::BV_XOR);
        else if (o == "rol") r = bin(Kind::BV_ROL);
        else if (o == "ror") r = bin(Kind::BV_ROR);
        else if (o == "sll") r = bin(Kind::BV_SHL);
        else if (o == "sra") r = bin(Kind::BV_ASHR);
        else if (o == "srl") r = bin(Kind::BV_SHR);
        else if (o == "add") r = bin(Kind::BV_ADD);
        else if (o == "mul") r = bin(Kind::BV_MUL);
        else if (o == "sdiv") r = bin(Kind::BV_SDIV);
        else if (o == "udiv") r = bin(Kind::BV_UDIV);
        else if (o == "smod") r = bin(Kind::BV_SMOD);
        else if (o == "srem") r = bin(Kind::BV_SREM);
        else if (o == "urem") r = bin(Kind::BV_UREM);
        else if (o == "sub") r = bin(Kind::BV_SUB);
        else if (o == "saddo") r = pred(Kind::BV_SADD_OVERFLOW);
        else if (o == "uaddo") r = pred(Kind::BV_UADD_OVERFLOW);
        else if (o == "sdivo") r = pred(Kind::BV_SDIV_OVERFLOW);
        else if (o == "udivo") r = c->zero();
        else if (o == "smulo") r = pred(Kind::BV_SMUL_OVERFLOW);
        else if (o == "umulo") r = pred(Kind::BV_UMUL_OVERFLOW);
        else if (o == "ssubo") r = pred(Kind::BV_SSUB_OVERFLOW);
        else if (o == "usubo") r = pred(Kind::BV_USUB_OVERFLOW);
        else if (o == "concat") r = bin(Kind::BV_CONCAT);
        else if (o == "read") r = bin(Kind::ARRAY_SELECT);
        else if (o == "write") r = tm.mk_term(Kind::ARRAY_STORE, {a[0], a[1], a[2]});
        else if (o == "ite") r = tm.mk_term(Kind::ITE, {c->to_bool(a[0]), a[1], a[2]});
        else if (o == "slice") r = tm.mk_term(Kind::BV_EXTRACT, {a[0]}, {i0, i1});
        else if (o == "sext") r = i0 ? tm.mk_term(Kind::BV_SIGN_EXTEND, {a[0]}, {i0}) : a[0];
        else if (o == "uext") r = i0 ? tm.mk_term(Kind::BV_ZERO_EXTEND, {a[0]}, {i0}) : a[0];
        else {
            c->error = "unsupported operator " + o;
            return -1;
        }
        return c->add(r);
    } catch (const std::exception& e) {
        c->error = std::string(op) + ": " + e.what();
        return -1;
    }
}

// Assert that a bv1 term is 1; 0, or -1 on error.
int informal_bw_assert(void* p, int t) {
    auto* c = static_cast<Ctx*>(p);
    return guarded(c, [&] {
        c->bz->assert_formula(c->to_bool(c->term(t)));
        return 0;
    });
}

// 10 SAT, 20 UNSAT, 0 unknown; assumptions are bv1 terms assumed 1.
int informal_bw_check(void* p, int n, const int* assume) {
    auto* c = static_cast<Ctx*>(p);
    return guarded(c, [&] {
        std::vector<Term> a;
        a.reserve(n > 0 ? (size_t)n : 0);
        for (int i = 0; i < n; i++)
            a.push_back(c->to_bool(c->term(assume[i])));
        auto r = c->bz->check_sat(a);
        if (r == Result::SAT) return 10;
        if (r == Result::UNSAT) return 20;
        return 0;
    });
}

int informal_bw_push(void* p, uint32_t levels) {
    auto* c = static_cast<Ctx*>(p);
    return guarded(c, [&] {
        c->bz->push(levels);
        return 0;
    });
}

int informal_bw_pop(void* p, uint32_t levels) {
    auto* c = static_cast<Ctx*>(p);
    return guarded(c, [&] {
        c->bz->pop(levels);
        return 0;
    });
}

// 1 if `t` (as a bv1==1 assumption) is in the last UNSAT assumptions, 0 if not, -1 on error.
int informal_bw_is_unsat_assumption(void* p, int t) {
    auto* c = static_cast<Ctx*>(p);
    return guarded(c, [&] {
        return c->bz->is_unsat_assumption(c->to_bool(c->term(t))) ? 1 : 0;
    });
}

// MSB-first binary string of `get_value(t)` for a BV (or "1"/"0" for Bool) term;
// pointer stays valid until the next call on `p`. Returns nullptr on error.
const char* informal_bw_get_value_bits(void* p, int t) {
    auto* c = static_cast<Ctx*>(p);
    try {
        Term val = c->bz->get_value(c->term(t));
        if (val.sort().is_bool()) {
            c->last_value = val.value<bool>() ? "1" : "0";
        } else {
            c->last_value = val.value<std::string>(2);
        }
        return c->last_value.c_str();
    } catch (const std::exception& e) {
        c->error = e.what();
        return nullptr;
    } catch (...) {
        c->error = "unknown C++ exception";
        return nullptr;
    }
}

} // extern "C"
