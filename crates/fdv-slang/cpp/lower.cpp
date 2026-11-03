// Lowers a slang-elaborated design to a Btor2 transition system.
//
// The design is flattened: every variable and net of every instance is
// one value, keyed by its slang symbol (instance caching is off, so each
// instance has its own symbols). Values are built on demand:
//   - top-level inputs are Btor2 inputs;
//   - variables assigned in clocked blocks (always_ff, always @(edge)) are
//     states whose next value is the block's result; initializers give init;
//   - continuous assigns, port connections and combinational blocks
//     (always_comb, always @*) are lowered when first read, with a cycle
//     check; undriven bits become free inputs.
// Immediate assert / assume statements become `bad` / `constraint` lines,
// guarded by the path condition that reaches them; a labelled assertion
// names its `bad` line. `$past`, `$rose`, `$fell`, `$stable`, `$changed`
// add history states. All registers step on every model step (one clock),
// asynchronous resets act synchronously, and X / Z read as 0.
//
// Not supported yet (reported as errors): unpacked arrays and memories,
// loops, functions and tasks, interfaces, real numbers, latches with
// feedback, concurrent assertions in the RTL.

#include "slang/ast/ASTVisitor.h"
#include "slang/ast/Compilation.h"
#include "slang/ast/EvalContext.h"
#include "slang/ast/symbols/CompilationUnitSymbols.h"
#include "slang/ast/symbols/InstanceSymbols.h"
#include "slang/ast/symbols/MemberSymbols.h"
#include "slang/ast/symbols/ParameterSymbols.h"
#include "slang/ast/symbols/PortSymbols.h"
#include "slang/ast/symbols/VariableSymbols.h"
#include "slang/ast/types/AllTypes.h"
#include "slang/diagnostics/DiagnosticEngine.h"
#include "slang/driver/Driver.h"
#include "slang/syntax/AllSyntax.h"
#include "slang/text/SourceManager.h"

#include <cstdlib>
#include <cstring>
#include <map>
#include <set>
#include <stdexcept>
#include <string>
#include <unordered_map>
#include <unordered_set>
#include <vector>

using namespace slang;
using namespace slang::ast;
using namespace slang::driver;

namespace {

struct Unsupported : std::runtime_error {
    using std::runtime_error::runtime_error;
};

[[noreturn]] void unsupported(const std::string& what) {
    throw Unsupported(what);
}

// ---------------------------------------------------------------- Btor2

struct Btor {
    std::string text;
    int next = 1;
    std::map<uint32_t, int> sorts;
    std::map<std::pair<uint32_t, std::string>, int> consts;
    std::unordered_map<int, uint32_t> widthOf;

    int sort(uint32_t w) {
        auto it = sorts.find(w);
        if (it != sorts.end())
            return it->second;
        int id = next++;
        text += std::to_string(id) + " sort bitvec " + std::to_string(w) + "\n";
        sorts[w] = id;
        return id;
    }

    int op(const char* name, uint32_t w, std::initializer_list<int> args,
           const std::string& extra = "") {
        int s = sort(w);
        int id = next++;
        std::string line = std::to_string(id) + " " + name + " " + std::to_string(s);
        for (int a : args)
            line += " " + std::to_string(a);
        if (!extra.empty())
            line += " " + extra;
        text += line + "\n";
        widthOf[id] = w;
        return id;
    }

    // `bits` is MSB first.
    int konst(const std::string& bits) {
        uint32_t w = (uint32_t)bits.size();
        auto key = std::make_pair(w, bits);
        auto it = consts.find(key);
        if (it != consts.end())
            return it->second;
        int id = op("const", w, {}, bits);
        consts[key] = id;
        return id;
    }

    int zero(uint32_t w) { return konst(std::string(w, '0')); }
    int ones(uint32_t w) { return konst(std::string(w, '1')); }
    int num(uint32_t w, uint64_t v) {
        std::string b(w, '0');
        for (uint32_t i = 0; i < w && i < 64; i++)
            if ((v >> i) & 1)
                b[w - 1 - i] = '1';
        return konst(b);
    }

    uint32_t width(int n) const { return widthOf.at(n < 0 ? -n : n); }

    int input(uint32_t w, const std::string& name) {
        return op("input", w, {}, name);
    }
    int state(uint32_t w, const std::string& name) {
        return op("state", w, {}, name);
    }
    void init(int state, int value) {
        int id = next++;
        text += std::to_string(id) + " init " + std::to_string(sort(width(state))) + " " +
                std::to_string(state) + " " + std::to_string(value) + "\n";
    }
    void nextOf(int state, int value) {
        int id = next++;
        text += std::to_string(id) + " next " + std::to_string(sort(width(state))) + " " +
                std::to_string(state) + " " + std::to_string(value) + "\n";
    }
    void bad(int cond, const std::string& name) {
        int id = next++;
        text += std::to_string(id) + " bad " + std::to_string(cond) +
                (name.empty() ? "" : " " + name) + "\n";
    }
    void constraint(int cond) {
        int id = next++;
        text += std::to_string(id) + " constraint " + std::to_string(cond) + "\n";
    }

    // ---- word helpers

    int un(const char* name, int a) { return op(name, width(a), {a}); }
    int bin(const char* name, int a, int b) { return op(name, width(a), {a, b}); }
    int cmp(const char* name, int a, int b) { return op(name, 1, {a, b}); }
    int slice(int a, uint32_t hi, uint32_t lo) {
        if (lo == 0 && hi + 1 == width(a))
            return a;
        return op("slice", hi - lo + 1, {a}, std::to_string(hi) + " " + std::to_string(lo));
    }
    int concat(int hi, int lo) { return op("concat", width(hi) + width(lo), {hi, lo}); }
    int ite(int c, int a, int b) {
        if (a == b || c == T())
            return a;
        if (c == F())
            return b;
        return op("ite", width(a), {c, a, b});
    }
    int resize(int a, uint32_t w, bool sign) {
        uint32_t aw = width(a);
        if (aw == w)
            return a;
        if (aw > w)
            return slice(a, w - 1, 0);
        return op(sign ? "sext" : "uext", w, {a}, std::to_string(w - aw));
    }
    int boolOf(int a) { return width(a) == 1 ? a : op("redor", 1, {a}); }
    int T() { return konst("1"); }
    int F() { return konst("0"); }
    int lnot(int a) {
        a = boolOf(a);
        if (a == T())
            return F();
        if (a == F())
            return T();
        return op("not", 1, {a});
    }
    int land(int a, int b) {
        a = boolOf(a), b = boolOf(b);
        if (a == F() || b == F())
            return F();
        if (a == T() || a == b)
            return b;
        if (b == T())
            return a;
        return op("and", 1, {a, b});
    }
    int lor(int a, int b) {
        a = boolOf(a), b = boolOf(b);
        if (a == T() || b == T())
            return T();
        if (a == F() || a == b)
            return b;
        if (b == F())
            return a;
        return op("or", 1, {a, b});
    }
};

// ---------------------------------------------------------------- lowering

std::string pathOf(const Symbol& s) {
    std::string p = s.getHierarchicalPath();
    for (auto& c : p)
        if (c == ' ')
            c = '_';
    return p;
}

uint32_t widthOf(const Type& t) {
    if (t.isUnpackedArray() || !t.isIntegral())
        unsupported("type `" + t.toString() + "` (only packed integral types are supported)");
    auto w = t.getBitWidth();
    if (w == 0)
        unsupported("zero-width type");
    return (uint32_t)w;
}

using Env = std::map<const ValueSymbol*, int>;

struct ContAssign {
    const Expression* lhs;
    const Expression* rhs;       // or
    const ValueSymbol* rhsSym;   // value of another symbol (output ports)
};

struct Assertion {
    bool assume;
    int cond;   // path => cond, in the cycle it is sampled
    int path;
    std::string label;
};

struct Lowerer {
    Btor b;
    const ASTContext* astCtx = nullptr;
    std::unordered_map<const ValueSymbol*, std::vector<ContAssign>> contAssigns;
    std::unordered_map<const ValueSymbol*, const ProceduralBlockSymbol*> combDriver;
    std::unordered_map<const ValueSymbol*, const ProceduralBlockSymbol*> ffDriver;
    std::vector<const ProceduralBlockSymbol*> ffBlocks, combBlocks;
    std::unordered_set<const ValueSymbol*> topInputs;
    std::unordered_map<const ValueSymbol*, int> values;
    std::unordered_set<const ValueSymbol*> inProgress;
    std::unordered_map<const ProceduralBlockSymbol*, Env> combDone;
    std::unordered_set<const ProceduralBlockSymbol*> combBusy;
    std::vector<Assertion> assertions;
    std::vector<std::string> notes;
    // History registers for $past: (node, depth) -> state
    std::map<std::pair<int, int>, int> pastRegs;
    std::vector<std::pair<int, int>> pendingNext; // state, next value
    int pastCount = 0;

    // ---- classification

    static bool isClocked(const ProceduralBlockSymbol& p) {
        if (p.procedureKind == ProceduralBlockKind::AlwaysFF)
            return true;
        if (p.procedureKind != ProceduralBlockKind::Always)
            return false;
        auto& body = p.getBody();
        if (body.kind != StatementKind::Timed)
            return false;
        auto& t = body.as<TimedStatement>().timing;
        auto edgeOf = [](const TimingControl& tc) {
            return tc.kind == TimingControlKind::SignalEvent &&
                   tc.as<SignalEventControl>().edge != EdgeKind::None;
        };
        if (edgeOf(t))
            return true;
        if (t.kind == TimingControlKind::EventList) {
            for (auto ev : t.as<EventListControl>().events)
                if (edgeOf(*ev))
                    return true;
        }
        return false;
    }

    static const Statement& bodyOf(const ProceduralBlockSymbol& p) {
        auto& body = p.getBody();
        if (body.kind == StatementKind::Timed)
            return body.as<TimedStatement>().stmt;
        return body;
    }

    static const ValueSymbol* rootSymbol(const Expression& e) {
        switch (e.kind) {
            case ExpressionKind::NamedValue:
            case ExpressionKind::HierarchicalValue:
                return &e.as<ValueExpressionBase>().symbol;
            case ExpressionKind::ElementSelect:
                return rootSymbol(e.as<ElementSelectExpression>().value());
            case ExpressionKind::RangeSelect:
                return rootSymbol(e.as<RangeSelectExpression>().value());
            case ExpressionKind::MemberAccess:
                return rootSymbol(e.as<MemberAccessExpression>().value());
            default:
                return nullptr;
        }
    }

    static void lhsRoots(const Expression& e, std::vector<const ValueSymbol*>& out) {
        if (e.kind == ExpressionKind::Concatenation) {
            for (auto op : e.as<ConcatenationExpression>().operands())
                lhsRoots(*op, out);
            return;
        }
        if (auto s = rootSymbol(e))
            out.push_back(s);
    }

    static void assignedIn(const Statement& s, std::vector<const ValueSymbol*>& out) {
        struct V : public ASTVisitor<V, VisitFlags::Statements | VisitFlags::Expressions> {
            std::vector<const ValueSymbol*>& out;
            explicit V(std::vector<const ValueSymbol*>& o) : out(o) {}
            void handle(const AssignmentExpression& a) {
                lhsRoots(a.left(), out);
                visitDefault(a);
            }
        } v(out);
        s.visit(v);
    }

    void collect(const RootSymbol& root) {
        for (auto top : root.topInstances) {
            for (auto port : top->body.getPortList()) {
                if (port->kind != SymbolKind::Port)
                    unsupported("non-simple top-level port " + std::string(port->name));
                auto& p = port->as<PortSymbol>();
                if (p.direction == ArgumentDirection::In && p.internalSymbol)
                    topInputs.insert(&p.internalSymbol->as<ValueSymbol>());
            }
        }
        root.visit(makeVisitor(
            [&](auto& v, const InstanceSymbol& inst) {
                if (!inst.isModule())
                    unsupported("instance " + pathOf(inst) + " of an interface or program");
                for (auto conn : inst.getPortConnections()) {
                    if (conn->port.kind != SymbolKind::Port)
                        unsupported("interface / multi port on " + pathOf(inst));
                    auto& p = conn->port.as<PortSymbol>();
                    auto expr = conn->getExpression();
                    if (!expr || !p.internalSymbol)
                        continue;
                    auto& internal = p.internalSymbol->as<ValueSymbol>();
                    if (p.direction == ArgumentDirection::In) {
                        // A null lhs means the whole variable.
                        contAssigns[&internal].push_back({nullptr, expr, nullptr});
                    }
                    else if (p.direction == ArgumentDirection::Out) {
                        const Expression* lhs = expr;
                        if (lhs->kind == ExpressionKind::Assignment)
                            lhs = &lhs->as<AssignmentExpression>().left();
                        std::vector<const ValueSymbol*> roots;
                        lhsRoots(*lhs, roots);
                        for (auto r : roots)
                            contAssigns[r].push_back({lhs, nullptr, &internal});
                    }
                    else {
                        unsupported("inout port " + std::string(p.name));
                    }
                }
                v.visitDefault(inst);
            },
            [&](auto& v, const GenerateBlockSymbol& g) {
                if (!g.isUninstantiated)
                    v.visitDefault(g);
            },
            [&](auto&, const ContinuousAssignSymbol& ca) {
                auto& a = ca.getAssignment().as<AssignmentExpression>();
                std::vector<const ValueSymbol*> roots;
                lhsRoots(a.left(), roots);
                for (auto r : roots)
                    contAssigns[r].push_back({&a.left(), &a.right(), nullptr});
            },
            [&](auto&, const NetSymbol& n) {
                if (auto init = n.getInitializer())
                    contAssigns[&n].push_back({nullptr, init, nullptr});
            },
            [&](auto&, const ProceduralBlockSymbol& p) {
                if (p.procedureKind == ProceduralBlockKind::Initial ||
                    p.procedureKind == ProceduralBlockKind::Final) {
                    notes.push_back("ignored an initial/final block");
                    return;
                }
                std::vector<const ValueSymbol*> assigned;
                assignedIn(p.getBody(), assigned);
                if (isClocked(p)) {
                    ffBlocks.push_back(&p);
                    for (auto s : assigned)
                        ffDriver[s] = &p;
                }
                else {
                    if (p.procedureKind == ProceduralBlockKind::AlwaysLatch)
                        notes.push_back("always_latch lowered as combinational logic");
                    combBlocks.push_back(&p);
                    for (auto s : assigned)
                        combDriver[s] = &p;
                }
            }));
    }

    // ---- values of symbols

    int value(const ValueSymbol& sym) {
        auto it = values.find(&sym);
        if (it != values.end())
            return it->second;
        if (sym.kind == SymbolKind::Parameter) {
            auto& cv = sym.as<ParameterSymbol>().getValue();
            return constant(cv, widthOf(sym.getType()));
        }
        if (sym.kind == SymbolKind::EnumValue) {
            auto& cv = sym.as<EnumValueSymbol>().getValue();
            return constant(cv, widthOf(sym.getType()));
        }
        uint32_t w = widthOf(sym.getType());
        if (topInputs.count(&sym)) {
            int n = b.input(w, pathOf(sym));
            values[&sym] = n;
            return n;
        }
        if (ffDriver.count(&sym)) {
            int n = b.state(w, pathOf(sym));
            values[&sym] = n;
            if (sym.kind == SymbolKind::Variable) {
                if (auto init = sym.as<VariableSymbol>().getInitializer()) {
                    int v = expr(*init, nullptr);
                    b.init(n, b.resize(v, w, init->type->isSigned()));
                }
            }
            return n;
        }
        if (inProgress.count(&sym))
            unsupported("combinational loop through " + pathOf(sym));
        inProgress.insert(&sym);
        int n;
        auto cb = combDriver.find(&sym);
        if (cb != combDriver.end()) {
            if (combBusy.count(cb->second))
                unsupported("read of " + pathOf(sym) +
                            " before it is assigned in its combinational block (latch or loop)");
            auto& env = lowerComb(*cb->second);
            auto e = env.find(&sym);
            n = e != env.end() ? e->second : b.input(w, pathOf(sym) + "_undriven");
        }
        else {
            auto ca = contAssigns.find(&sym);
            if (ca != contAssigns.end()) {
                Env env;
                bool whole = false;
                for (auto& a : ca->second) {
                    bool named = a.lhs && (a.lhs->kind == ExpressionKind::NamedValue ||
                                           a.lhs->kind == ExpressionKind::HierarchicalValue) &&
                                 &a.lhs->as<ValueExpressionBase>().symbol == &sym;
                    whole |= a.lhs == nullptr || named;
                }
                env[&sym] = whole ? b.zero(w) : b.input(w, pathOf(sym) + "_undriven");
                for (auto& a : ca->second) {
                    int rhs = a.rhsSym ? value(*a.rhsSym) : expr(*a.rhs, nullptr);
                    if (a.lhs)
                        assign(*a.lhs, rhs, env, sym);
                    else
                        env[&sym] = b.resize(rhs, w, a.rhs && a.rhs->type->isSigned());
                }
                n = env[&sym];
            }
            else if (sym.kind == SymbolKind::Variable && sym.as<VariableSymbol>().getInitializer()) {
                auto init = sym.as<VariableSymbol>().getInitializer();
                n = b.resize(expr(*init, nullptr), w, init->type->isSigned());
            }
            else {
                n = b.input(w, pathOf(sym) + "_undriven");
            }
        }
        inProgress.erase(&sym);
        values[&sym] = n;
        return n;
    }

    int constant(const ConstantValue& cv, uint32_t w) {
        if (!cv.isInteger())
            unsupported("non-integer constant");
        auto& v = cv.integer();
        std::string bits(w, '0');
        for (uint32_t i = 0; i < w && i < v.getBitWidth(); i++) {
            auto bit = v[(int32_t)i];
            if (!bit.isUnknown() && bit.value)
                bits[w - 1 - i] = '1';
        }
        if (v.isSigned() && v.getBitWidth() < w && v[(int32_t)v.getBitWidth() - 1].value == 1)
            for (uint32_t i = v.getBitWidth(); i < w; i++)
                bits[w - 1 - i] = '1';
        return b.konst(bits);
    }

    std::optional<int64_t> constInt(const Expression& e) {
        EvalContext ctx(*astCtx);
        auto cv = e.eval(ctx);
        if (!cv || !cv.isInteger() || cv.integer().hasUnknown())
            return std::nullopt;
        return cv.integer().as<int64_t>();
    }

    // ---- procedural blocks

    Env& lowerComb(const ProceduralBlockSymbol& p) {
        auto it = combDone.find(&p);
        if (it != combDone.end())
            return it->second;
        Env env;
        int path = b.ones(1);
        combBusy.insert(&p);
        stmt(bodyOf(p), env, env, path, false);
        combBusy.erase(&p);
        return combDone[&p] = env;
    }

    void lowerFF(const ProceduralBlockSymbol& p) {
        Env blocking, nba;
        int path = b.ones(1);
        stmt(bodyOf(p), blocking, nba, path, true);
        for (auto& [sym, v] : nba) {
            if (!ffDriver.count(sym) || ffDriver[sym] != &p)
                continue;
            int s = value(*sym);
            pendingNext.push_back({s, b.resize(v, b.width(s), false)});
        }
        // Blocking assignments to variables that persist (rare in always_ff).
        for (auto& [sym, v] : blocking) {
            if (nba.count(sym) || !ffDriver.count(sym) || ffDriver[sym] != &p)
                continue;
            int s = value(*sym);
            pendingNext.push_back({s, b.resize(v, b.width(s), false)});
        }
    }

    int read(const ValueSymbol& sym, const Env* env) {
        if (env) {
            auto it = env->find(&sym);
            if (it != env->end())
                return it->second;
        }
        return value(sym);
    }

    // Merge two branch environments under `c` (then / else).
    void merge(int c, const Env& base, const Env& t, const Env& e, Env& out, const Env* readEnv) {
        std::set<const ValueSymbol*> keys;
        for (auto& kv : t)
            keys.insert(kv.first);
        for (auto& kv : e)
            keys.insert(kv.first);
        for (auto k : keys) {
            auto pick = [&](const Env& env) {
                auto it = env.find(k);
                if (it != env.end())
                    return it->second;
                auto bt = base.find(k);
                if (bt != base.end())
                    return bt->second;
                return read(*k, readEnv);
            };
            out[k] = b.ite(c, pick(t), pick(e));
        }
    }

    // `blocking` holds values visible to later reads; `nba` collects
    // non-blocking results (the same map for combinational blocks).
    void stmt(const Statement& s, Env& blocking, Env& nba, int path, bool clocked) {
        switch (s.kind) {
            case StatementKind::Empty:
                return;
            case StatementKind::List:
                for (auto st : s.as<StatementList>().list)
                    stmt(*st, blocking, nba, path, clocked);
                return;
            case StatementKind::Block:
                stmt(s.as<BlockStatement>().body, blocking, nba, path, clocked);
                return;
            case StatementKind::VariableDeclaration: {
                auto& var = s.as<VariableDeclStatement>().symbol;
                if (auto init = var.getInitializer())
                    blocking[&var] = b.resize(expr(*init, &blocking), widthOf(var.getType()),
                                              init->type->isSigned());
                return;
            }
            case StatementKind::Timed:
                stmt(s.as<TimedStatement>().stmt, blocking, nba, path, clocked);
                return;
            case StatementKind::ExpressionStatement: {
                auto& e = s.as<ExpressionStatement>().expr;
                if (e.kind == ExpressionKind::Assignment) {
                    auto& a = e.as<AssignmentExpression>();
                    if (a.isCompound())
                        unsupported("compound assignment");
                    int rhs = expr(a.right(), &blocking);
                    if (a.isNonBlocking() && clocked)
                        assign(a.left(), rhs, nba, nullptr, &blocking);
                    else
                        assign(a.left(), rhs, blocking, nullptr, &blocking);
                    return;
                }
                if (e.kind == ExpressionKind::Call && e.as<CallExpression>().isSystemCall())
                    return; // $display and friends
                unsupported("expression statement");
            }
            case StatementKind::Conditional: {
                auto& c = s.as<ConditionalStatement>();
                if (c.conditions.size() != 1 || c.conditions[0].pattern)
                    unsupported("if with patterns");
                int cond = b.boolOf(expr(*c.conditions[0].expr, &blocking));
                branch(cond, c.ifTrue, c.ifFalse, blocking, nba, path, clocked);
                return;
            }
            case StatementKind::Case: {
                auto& c = s.as<CaseStatement>();
                if (c.condition == CaseStatementCondition::Inside)
                    unsupported("case inside");
                int sel = expr(c.expr, &blocking);
                // Build as an if / else-if chain.
                lowerCase(c, 0, sel, blocking, nba, path, clocked);
                return;
            }
            case StatementKind::ImmediateAssertion: {
                auto& a = s.as<ImmediateAssertionStatement>();
                if (a.assertionKind == AssertionKind::CoverProperty)
                    unsupported("immediate cover");
                std::string label;
                if (s.syntax) {
                    if (auto ss = s.syntax->as_if<syntax::StatementSyntax>(); ss && ss->label)
                        label = std::string(ss->label->name.valueText());
                }
                int cond = b.boolOf(expr(a.cond, &blocking));
                assertions.push_back({a.assertionKind == AssertionKind::Assume, cond, path, label});
                return;
            }
            default:
                unsupported(std::string("statement kind ") + std::string(toString(s.kind)));
        }
    }

    void branch(int cond, const Statement& t, const Statement* f, Env& blocking, Env& nba,
                int path, bool clocked) {
        bool sameMap = &blocking == &nba;
        Env tb = blocking, tn = sameMap ? Env{} : nba;
        Env fb = blocking, fn = sameMap ? Env{} : nba;
        stmt(t, tb, sameMap ? tb : tn, b.land(path, cond), clocked);
        if (f)
            stmt(*f, fb, sameMap ? fb : fn, b.land(path, b.lnot(cond)), clocked);
        Env base = blocking;
        Env merged = blocking;
        merge(cond, base, tb, fb, merged, &blocking);
        if (!sameMap) {
            Env mergedN = nba;
            merge(cond, nba, tn, fn, mergedN, &blocking);
            // Values not assigned on one side keep the pending NBA, or the
            // register itself.
            nba = mergedN;
        }
        blocking = merged;
    }

    void lowerCase(const CaseStatement& c, size_t i, int sel, Env& blocking, Env& nba, int path,
                   bool clocked) {
        if (i == c.items.size()) {
            if (c.defaultCase)
                stmt(*c.defaultCase, blocking, nba, path, clocked);
            return;
        }
        auto& item = c.items[i];
        int match = b.zero(1);
        for (auto e : item.expressions)
            match = b.lor(match, caseMatch(c, sel, *e, blocking));
        bool sameMap = &blocking == &nba;
        Env tb = blocking, tn = sameMap ? Env{} : nba;
        Env fb = blocking, fn = sameMap ? Env{} : nba;
        stmt(*item.stmt, tb, sameMap ? tb : tn, b.land(path, match), clocked);
        lowerCase(c, i + 1, sel, fb, sameMap ? fb : fn, b.land(path, b.lnot(match)), clocked);
        Env merged = blocking;
        merge(match, blocking, tb, fb, merged, &blocking);
        if (!sameMap) {
            Env mergedN = nba;
            merge(match, nba, tn, fn, mergedN, &blocking);
            nba = mergedN;
        }
        blocking = merged;
    }

    int caseMatch(const CaseStatement& c, int sel, const Expression& item, Env& env) {
        uint32_t w = std::max(b.width(sel), widthOf(*item.type));
        int s = b.resize(sel, w, c.expr.type->isSigned());
        if (c.condition != CaseStatementCondition::Normal) {
            EvalContext ctx(*astCtx);
            auto cv = item.eval(ctx);
            if (cv && cv.isInteger()) {
                auto& v = cv.integer();
                std::string care(w, '1'), val(w, '0');
                for (uint32_t i = 0; i < w && i < v.getBitWidth(); i++) {
                    auto bit = v[(int32_t)i];
                    bool wild = c.condition == CaseStatementCondition::WildcardJustZ
                                    ? bit.value == logic_t::Z_VALUE
                                    : bit.isUnknown();
                    if (wild)
                        care[w - 1 - i] = '0';
                    else if (bit.value == 1)
                        val[w - 1 - i] = '1';
                }
                int m = b.konst(care);
                return b.cmp("eq", b.bin("and", s, m), b.konst(val));
            }
        }
        int v = b.resize(expr(item, &env), w, item.type->isSigned());
        return b.cmp("eq", s, v);
    }

    // ---- assignment to lvalues

    // Writes `v` into lvalue `lhs` in `env`. `only` restricts writes to one
    // root symbol (continuous assigns lowering one variable).
    void assign(const Expression& lhs, int v, Env& env, const ValueSymbol* only,
                const Env* readEnv = nullptr) {
        assignTo(lhs, v, env, only, readEnv ? readEnv : &env);
    }

    void assign(const Expression& lhs, int v, Env& env, const ValueSymbol& only) {
        assignTo(lhs, v, env, &only, &env);
    }

    void assignTo(const Expression& lhs, int v, Env& env, const ValueSymbol* only,
                  const Env* readEnv) {
        uint32_t w = widthOf(*lhs.type);
        switch (lhs.kind) {
            case ExpressionKind::NamedValue:
            case ExpressionKind::HierarchicalValue: {
                auto& sym = lhs.as<ValueExpressionBase>().symbol;
                if (only && &sym != only)
                    return;
                env[&sym] = b.resize(v, w, false);
                return;
            }
            case ExpressionKind::Concatenation: {
                auto ops = lhs.as<ConcatenationExpression>().operands();
                uint32_t hi = b.width(v);
                v = b.resize(v, w, false);
                hi = w;
                for (auto op : ops) {
                    uint32_t ow = widthOf(*op->type);
                    assignTo(*op, b.slice(v, hi - 1, hi - ow), env, only, readEnv);
                    hi -= ow;
                }
                return;
            }
            case ExpressionKind::ElementSelect:
            case ExpressionKind::RangeSelect:
            case ExpressionKind::MemberAccess: {
                const Expression& base = lhs.kind == ExpressionKind::ElementSelect
                                             ? lhs.as<ElementSelectExpression>().value()
                                         : lhs.kind == ExpressionKind::RangeSelect
                                             ? lhs.as<RangeSelectExpression>().value()
                                             : lhs.as<MemberAccessExpression>().value();
                auto root = rootSymbol(base);
                if (only && root != only)
                    return;
                int old = readLvalue(base, env, readEnv);
                auto [off, dynOff] = offsetOf(lhs, readEnv);
                int nv = insert(old, b.resize(v, w, false), off, dynOff);
                assignTo(base, nv, env, only, readEnv);
                return;
            }
            default:
                unsupported("assignment target");
        }
    }

    int readLvalue(const Expression& e, Env& env, const Env* readEnv) {
        if (auto root = rootSymbol(e); root && (e.kind == ExpressionKind::NamedValue ||
                                                e.kind == ExpressionKind::HierarchicalValue)) {
            auto it = env.find(root);
            if (it != env.end())
                return it->second;
            if (readEnv) {
                auto jt = readEnv->find(root);
                if (jt != readEnv->end())
                    return jt->second;
            }
            // A register keeps its value; bits of a combinational variable
            // that its own block has not written yet start free.
            auto cb = combDriver.find(root);
            if (cb != combDriver.end() && combBusy.count(cb->second))
                return b.input(widthOf(root->getType()), pathOf(*root) + "_undriven");
            return value(*root);
        }
        Env merged = *readEnv;
        for (auto& kv : env)
            merged[kv.first] = kv.second;
        return expr(e, &merged);
    }

    // Insert `v` into `old` at bit offset `off` (or the dynamic offset).
    int insert(int old, int v, uint32_t off, int dynOff) {
        uint32_t ow = b.width(old), vw = b.width(v);
        if (dynOff < 0) {
            if (off >= ow)
                return old;
            if (off + vw > ow)
                v = b.slice(v, ow - off - 1, 0), vw = ow - off;
            int r = v;
            if (off > 0)
                r = b.concat(r, b.slice(old, off - 1, 0));
            if (off + vw < ow)
                r = b.concat(b.slice(old, ow - 1, off + vw), r);
            return r;
        }
        int sh = b.resize(dynOff, ow, false);
        int mask = b.bin("sll", b.resize(b.ones(vw), ow, false), sh);
        int val = b.bin("sll", b.resize(v, ow, false), sh);
        return b.bin("or", b.bin("and", old, b.un("not", mask)), val);
    }

    // Bit offset of a select from its base: constant, or a node (>= 0).
    std::pair<uint32_t, int> offsetOf(const Expression& sel, const Env* env) {
        if (sel.kind == ExpressionKind::MemberAccess) {
            auto& m = sel.as<MemberAccessExpression>().member;
            if (m.kind != SymbolKind::Field)
                unsupported("member access");
            auto& f = m.as<FieldSymbol>();
            if (!sel.as<MemberAccessExpression>().value().type->isPackedArray() &&
                !sel.as<MemberAccessExpression>().value().type->getCanonicalType().isStruct())
                unsupported("member of an unpacked type");
            return {(uint32_t)f.bitOffset, -1};
        }
        const Expression& base = sel.kind == ExpressionKind::ElementSelect
                                     ? sel.as<ElementSelectExpression>().value()
                                     : sel.as<RangeSelectExpression>().value();
        auto& bt = base.type->getCanonicalType();
        if (!bt.isIntegral())
            unsupported("select from a non-packed value");
        auto range = bt.getFixedRange();
        uint32_t ew = 1;
        if (bt.isPackedArray())
            ew = (uint32_t)bt.as<PackedArrayType>().elementType.getBitWidth();
        auto toOffset = [&](int64_t idx) -> uint32_t {
            return (uint32_t)range.translateIndex((int32_t)idx) * ew;
        };
        if (sel.kind == ExpressionKind::ElementSelect) {
            auto& s = sel.as<ElementSelectExpression>().selector();
            if (auto c = constInt(s))
                return {toOffset(*c), -1};
            int idx = expr(s, env);
            return {0, dynamicOffset(idx, range, ew)};
        }
        auto& rs = sel.as<RangeSelectExpression>();
        auto lo = constInt(rs.left()), ro = constInt(rs.right());
        switch (rs.getSelectionKind()) {
            case RangeSelectionKind::Simple:
                if (!lo || !ro)
                    unsupported("non-constant range select");
                return {std::min(toOffset(*lo), toOffset(*ro)), -1};
            case RangeSelectionKind::IndexedUp:
            case RangeSelectionKind::IndexedDown: {
                if (!ro)
                    unsupported("non-constant part-select width");
                int64_t width = *ro;
                bool up = rs.getSelectionKind() == RangeSelectionKind::IndexedUp;
                if (lo) {
                    int64_t a = *lo, z = up ? *lo + width - 1 : *lo - width + 1;
                    return {std::min(toOffset(a), toOffset(z)), -1};
                }
                if (!range.isDescending())
                    unsupported("variable part-select on a big-endian range");
                int idx = expr(rs.left(), env);
                if (!up)
                    idx = b.bin("sub", idx, b.num(b.width(idx), (uint64_t)(width - 1)));
                return {0, dynamicOffset(idx, range, ew)};
            }
        }
        unsupported("select");
    }

    int dynamicOffset(int idx, const ConstantRange& range, uint32_t ew) {
        uint32_t w = std::max<uint32_t>(b.width(idx), 32);
        idx = b.resize(idx, w, false);
        int off;
        if (range.isDescending())
            off = range.right == 0 ? idx : b.bin("sub", idx, b.num(w, (uint64_t)range.right));
        else
            off = b.bin("sub", b.num(w, (uint64_t)range.right), idx);
        if (ew != 1)
            off = b.bin("mul", off, b.num(w, ew));
        return off;
    }

    // ---- expressions

    int past(int n, int depth) {
        int cur = n;
        for (int d = 1; d <= depth; d++) {
            auto key = std::make_pair(n, d);
            auto it = pastRegs.find(key);
            if (it == pastRegs.end()) {
                int s = b.state(b.width(n), "fdv_past_" + std::to_string(pastCount++));
                pendingNext.push_back({s, cur});
                it = pastRegs.emplace(key, s).first;
            }
            cur = it->second;
        }
        return cur;
    }

    int expr(const Expression& e, const Env* env) {
        int n = exprRaw(e, env);
        if (e.type->isIntegral())
            n = b.resize(n, widthOf(*e.type), e.type->isSigned());
        return n;
    }

    int exprRaw(const Expression& e, const Env* env) {
        switch (e.kind) {
            case ExpressionKind::IntegerLiteral: {
                ConstantValue cv(e.as<IntegerLiteral>().getValue());
                return constant(cv, widthOf(*e.type));
            }
            case ExpressionKind::UnbasedUnsizedIntegerLiteral: {
                ConstantValue cv(e.as<UnbasedUnsizedIntegerLiteral>().getValue());
                return constant(cv, widthOf(*e.type));
            }
            case ExpressionKind::NamedValue:
            case ExpressionKind::HierarchicalValue:
                return read(e.as<ValueExpressionBase>().symbol, env);
            case ExpressionKind::Conversion: {
                auto& op = e.as<ConversionExpression>().operand();
                int v = expr(op, env);
                return b.resize(v, widthOf(*e.type), op.type->isSigned());
            }
            case ExpressionKind::UnaryOp:
                return unary(e.as<UnaryExpression>(), env);
            case ExpressionKind::BinaryOp:
                return binary(e.as<BinaryExpression>(), env);
            case ExpressionKind::ConditionalOp: {
                auto& c = e.as<ConditionalExpression>();
                if (c.conditions.size() != 1 || c.conditions[0].pattern)
                    unsupported("conditional with patterns");
                int cond = b.boolOf(expr(*c.conditions[0].expr, env));
                uint32_t w = widthOf(*e.type);
                int t = b.resize(expr(c.left(), env), w, c.left().type->isSigned());
                int f = b.resize(expr(c.right(), env), w, c.right().type->isSigned());
                return b.ite(cond, t, f);
            }
            case ExpressionKind::Concatenation: {
                int acc = -1;
                for (auto op : e.as<ConcatenationExpression>().operands()) {
                    if (op->type->isVoid())
                        continue;
                    int v = expr(*op, env);
                    acc = acc < 0 ? v : b.concat(acc, v);
                }
                if (acc < 0)
                    unsupported("empty concatenation");
                return acc;
            }
            case ExpressionKind::Replication: {
                auto& r = e.as<ReplicationExpression>();
                auto cnt = constInt(r.count());
                if (!cnt || *cnt <= 0)
                    unsupported("replication count");
                int v = expr(r.concat(), env);
                int acc = v;
                for (int64_t i = 1; i < *cnt; i++)
                    acc = b.concat(acc, v);
                return acc;
            }
            case ExpressionKind::ElementSelect:
            case ExpressionKind::RangeSelect:
            case ExpressionKind::MemberAccess: {
                const Expression& base = e.kind == ExpressionKind::ElementSelect
                                             ? e.as<ElementSelectExpression>().value()
                                         : e.kind == ExpressionKind::RangeSelect
                                             ? e.as<RangeSelectExpression>().value()
                                             : e.as<MemberAccessExpression>().value();
                int v = expr(base, env);
                uint32_t w = widthOf(*e.type);
                auto [off, dyn] = offsetOf(e, env);
                if (dyn < 0) {
                    if (off + w > b.width(v))
                        unsupported("constant select out of range");
                    return b.slice(v, off + w - 1, off);
                }
                int sh = b.bin("srl", v, b.resize(dyn, b.width(v), false));
                return b.slice(sh, w - 1, 0);
            }
            case ExpressionKind::Call:
                return call(e.as<CallExpression>(), env);
            default:
                unsupported(std::string("expression kind ") + std::string(toString(e.kind)));
        }
    }

    int call(const CallExpression& c, const Env* env) {
        if (!c.isSystemCall())
            unsupported("function call " + std::string(c.getSubroutineName()));
        auto name = c.getSubroutineName();
        auto args = c.arguments();
        auto arg = [&](size_t i) { return expr(*args[i], env); };
        if (name == "$past") {
            int depth = 1;
            if (args.size() > 1 && args[1]->kind != ExpressionKind::EmptyArgument) {
                auto d = constInt(*args[1]);
                if (!d || *d < 1)
                    unsupported("$past depth");
                depth = (int)*d;
            }
            if (args.size() > 2)
                unsupported("$past with a gating expression");
            return past(arg(0), depth);
        }
        if (name == "$rose" || name == "$fell") {
            int v = b.slice(arg(0), 0, 0);
            int p = past(v, 1);
            return name == "$rose" ? b.land(v, b.lnot(p)) : b.land(b.lnot(v), p);
        }
        if (name == "$stable" || name == "$changed") {
            int v = arg(0);
            int eq = b.cmp("eq", v, past(v, 1));
            return name == "$stable" ? eq : b.lnot(eq);
        }
        if (name == "$signed" || name == "$unsigned")
            return arg(0);
        if (name == "$countones" || name == "$onehot" || name == "$onehot0") {
            int v = arg(0);
            uint32_t w = b.width(v), cw = 32;
            int cnt = b.zero(cw);
            for (uint32_t i = 0; i < w; i++)
                cnt = b.bin("add", cnt, b.resize(b.slice(v, i, i), cw, false));
            if (name == "$countones")
                return cnt;
            if (name == "$onehot")
                return b.cmp("eq", cnt, b.num(cw, 1));
            return b.cmp("ulte", cnt, b.num(cw, 1));
        }
        if (name == "$isunknown")
            return b.zero(1);
        unsupported("system function " + std::string(name));
    }

    int unary(const UnaryExpression& u, const Env* env) {
        int a = expr(u.operand(), env);
        switch (u.op) {
            case UnaryOperator::Plus: return a;
            case UnaryOperator::Minus: return b.un("neg", a);
            case UnaryOperator::BitwiseNot: return b.un("not", a);
            case UnaryOperator::BitwiseAnd: return b.op("redand", 1, {a});
            case UnaryOperator::BitwiseOr: return b.op("redor", 1, {a});
            case UnaryOperator::BitwiseXor: return b.op("redxor", 1, {a});
            case UnaryOperator::BitwiseNand: return b.lnot(b.op("redand", 1, {a}));
            case UnaryOperator::BitwiseNor: return b.lnot(b.op("redor", 1, {a}));
            case UnaryOperator::BitwiseXnor: return b.lnot(b.op("redxor", 1, {a}));
            case UnaryOperator::LogicalNot: return b.lnot(a);
            default: unsupported("increment / decrement operator");
        }
    }

    int shiftAmount(int a, int s) {
        // Btor2 shifts need equal widths; a larger amount saturates.
        uint32_t aw = b.width(a), sw = b.width(s);
        if (sw <= aw)
            return b.resize(s, aw, false);
        int high = b.op("redor", 1, {b.slice(s, sw - 1, aw)});
        return b.ite(high, b.ones(aw), b.slice(s, aw - 1, 0));
    }

    int binary(const BinaryExpression& be, const Env* env) {
        int l = expr(be.left(), env);
        int r = expr(be.right(), env);
        bool sgn = be.left().type->isSigned() && be.right().type->isSigned();
        auto same = [&]() {
            uint32_t w = std::max(b.width(l), b.width(r));
            l = b.resize(l, w, be.left().type->isSigned());
            r = b.resize(r, w, be.right().type->isSigned());
        };
        switch (be.op) {
            case BinaryOperator::Add: same(); return b.bin("add", l, r);
            case BinaryOperator::Subtract: same(); return b.bin("sub", l, r);
            case BinaryOperator::Multiply: same(); return b.bin("mul", l, r);
            case BinaryOperator::Divide: same(); return b.bin(sgn ? "sdiv" : "udiv", l, r);
            case BinaryOperator::Mod: same(); return b.bin(sgn ? "srem" : "urem", l, r);
            case BinaryOperator::BinaryAnd: same(); return b.bin("and", l, r);
            case BinaryOperator::BinaryOr: same(); return b.bin("or", l, r);
            case BinaryOperator::BinaryXor: same(); return b.bin("xor", l, r);
            case BinaryOperator::BinaryXnor: same(); return b.bin("xnor", l, r);
            case BinaryOperator::Equality:
            case BinaryOperator::CaseEquality:
            case BinaryOperator::WildcardEquality: same(); return b.cmp("eq", l, r);
            case BinaryOperator::Inequality:
            case BinaryOperator::CaseInequality:
            case BinaryOperator::WildcardInequality: same(); return b.cmp("neq", l, r);
            case BinaryOperator::GreaterThanEqual: same(); return b.cmp(sgn ? "sgte" : "ugte", l, r);
            case BinaryOperator::GreaterThan: same(); return b.cmp(sgn ? "sgt" : "ugt", l, r);
            case BinaryOperator::LessThanEqual: same(); return b.cmp(sgn ? "slte" : "ulte", l, r);
            case BinaryOperator::LessThan: same(); return b.cmp(sgn ? "slt" : "ult", l, r);
            case BinaryOperator::LogicalAnd: return b.land(l, r);
            case BinaryOperator::LogicalOr: return b.lor(l, r);
            case BinaryOperator::LogicalImplication: return b.lor(b.lnot(l), r);
            case BinaryOperator::LogicalEquivalence: return b.cmp("eq", b.boolOf(l), b.boolOf(r));
            case BinaryOperator::LogicalShiftLeft:
            case BinaryOperator::ArithmeticShiftLeft: return b.bin("sll", l, shiftAmount(l, r));
            case BinaryOperator::LogicalShiftRight: return b.bin("srl", l, shiftAmount(l, r));
            case BinaryOperator::ArithmeticShiftRight:
                return b.bin(be.left().type->isSigned() ? "sra" : "srl", l, shiftAmount(l, r));
            case BinaryOperator::Power: {
                auto base = constInt(be.left()), ex = constInt(be.right());
                if (base && ex && *ex >= 0) {
                    uint64_t v = 1;
                    for (int64_t i = 0; i < *ex; i++)
                        v *= (uint64_t)*base;
                    return b.num(widthOf(*be.type), v);
                }
                unsupported("non-constant power");
            }
        }
        unsupported("binary operator");
    }

    // ---- driver

    std::string run(const RootSymbol& root) {
        collect(root);
        for (auto p : ffBlocks)
            lowerFF(*p);
        for (auto p : combBlocks)
            lowerComb(*p);
        int bads = 0;
        for (auto& a : assertions) {
            int ok = b.lor(b.lnot(a.path), a.cond);
            if (a.assume)
                b.constraint(ok);
            else {
                b.bad(b.lnot(ok), a.label);
                bads++;
            }
        }
        // `next` lines last: every value they use exists by now. Lowering a
        // next value can create more history registers, so loop.
        for (size_t i = 0; i < pendingNext.size(); i++)
            b.nextOf(pendingNext[i].first, pendingNext[i].second);
        if (bads == 0)
            notes.push_back("the design has no assertions");
        return b.text;
    }
};

void esc(std::string& out, std::string_view s) {
    out += '"';
    for (char c : s) {
        switch (c) {
            case '"': out += "\\\""; break;
            case '\\': out += "\\\\"; break;
            case '\n': out += "\\n"; break;
            case '\t': out += "\\t"; break;
            default:
                if ((unsigned char)c < 0x20) {
                    char buf[8];
                    snprintf(buf, sizeof buf, "\\u%04x", c);
                    out += buf;
                }
                else {
                    out += c;
                }
        }
    }
    out += '"';
}

char* dup(const std::string& s) {
    char* p = (char*)std::malloc(s.size() + 1);
    std::memcpy(p, s.c_str(), s.size() + 1);
    return p;
}

} // namespace

extern "C" {

// Same argument convention as fdv_slang_compile. Returns JSON:
// {"ok", "errors": [...], "btor2": "...", "notes": [...]}.
char* fdv_slang_lower(const char* args) {
    std::vector<std::string> words{"slang"};
    for (const char* p = args; *p; p += std::strlen(p) + 1)
        words.emplace_back(p);
    std::vector<const char*> argv;
    for (auto& w : words)
        argv.push_back(w.c_str());

    std::string out = "{";
    std::vector<std::string> errors;
    std::string btor;
    std::vector<std::string> notes;
    try {
        Driver driver;
        driver.addStandardArgs();
        driver.options.compilationFlags[CompilationFlags::DisableInstanceCaching] = true;
        if (!driver.parseCommandLine((int)argv.size(), argv.data()) || !driver.processOptions())
            throw std::runtime_error("bad front-end options");
        driver.parseAllSources();
        auto comp = driver.createCompilation();
        auto& sm = driver.sourceManager;
        DiagnosticEngine engine(sm);
        for (auto& d : comp->getAllDiagnostics()) {
            auto sev = engine.getSeverity(d.code, d.location);
            if (sev == DiagnosticSeverity::Error || sev == DiagnosticSeverity::Fatal) {
                std::string where;
                if (d.location.valid()) {
                    auto loc = sm.getFullyOriginalLoc(d.location);
                    where = sm.getFullPath(loc.buffer()).string() + ":" +
                            std::to_string(sm.getLineNumber(loc)) + ": ";
                }
                errors.push_back(where + engine.formatMessage(d));
            }
        }
        if (errors.empty()) {
            auto& root = comp->getRoot();
            if (root.topInstances.size() != 1)
                throw Unsupported("expected exactly one top module, found " +
                                  std::to_string(root.topInstances.size()));
            ASTContext ctx(root.topInstances[0]->body, LookupLocation::max);
            Lowerer l;
            l.astCtx = &ctx;
            btor = l.run(root);
            notes = l.notes;
        }
    }
    catch (const Unsupported& e) {
        errors.push_back(std::string("not supported yet: ") + e.what());
    }
    catch (const std::exception& e) {
        errors.push_back(std::string("slang: ") + e.what());
    }
    out += "\"ok\":";
    out += errors.empty() ? "true" : "false";
    out += ",\"errors\":[";
    for (size_t i = 0; i < errors.size(); i++) {
        if (i)
            out += ',';
        esc(out, errors[i]);
    }
    out += "],\"notes\":[";
    for (size_t i = 0; i < notes.size(); i++) {
        if (i)
            out += ',';
        esc(out, notes[i]);
    }
    out += "],\"btor2\":";
    esc(out, btor);
    out += "}";
    return dup(out);
}

} // extern "C"
