// C shim over slang for fdv: compile a design from a slang command line
// and return what fdv needs as JSON (diagnostics, top instances, the
// instance tree and every variable / net with its width).

#include "slang/ast/ASTVisitor.h"
#include "slang/ast/Compilation.h"
#include "slang/ast/symbols/CompilationUnitSymbols.h"
#include "slang/ast/symbols/InstanceSymbols.h"
#include "slang/ast/symbols/VariableSymbols.h"
#include "slang/ast/symbols/MemberSymbols.h"
#include "slang/ast/symbols/ParameterSymbols.h"
#include "slang/ast/symbols/PortSymbols.h"
#include "slang/ast/types/AllTypes.h"
#include "slang/diagnostics/DiagnosticEngine.h"
#include "slang/driver/Driver.h"
#include "slang/text/SourceManager.h"

#include <cstdlib>
#include <cstring>
#include <memory>
#include <string>
#include <vector>

using namespace slang;
using namespace slang::ast;
using namespace slang::driver;

namespace {

void esc(std::string& out, std::string_view s) {
    out += '"';
    for (char c : s) {
        switch (c) {
            case '"': out += "\\\""; break;
            case '\\': out += "\\\\"; break;
            case '\n': out += "\\n"; break;
            case '\r': out += "\\r"; break;
            case '\t': out += "\\t"; break;
            default:
                if ((unsigned char)c < 0x20) {
                    char buf[8];
                    snprintf(buf, sizeof buf, "\\u%04x", c);
                    out += buf;
                } else {
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

// The design browser's view of one instance: its ports and named signals
// (by name relative to the instance, so a generate block's signal reads
// `g[0].x`) and its child instances.
struct Sig {
    std::string name;
    uint64_t width;
    std::string kind; // "port", "variable", "net", "parameter"
    std::string dir;  // ports only: "input", "output", "inout", "ref"
};
struct ScopeNode {
    std::string path, name, module;
    std::vector<Sig> sigs;
    std::vector<std::unique_ptr<ScopeNode>> children;
};

const char* dirName(ArgumentDirection d) {
    switch (d) {
        case ArgumentDirection::In: return "input";
        case ArgumentDirection::Out: return "output";
        case ArgumentDirection::InOut: return "inout";
        default: return "ref";
    }
}

void emitScope(std::string& out, const ScopeNode& n) {
    out += "{\"path\":";
    esc(out, n.path);
    out += ",\"name\":";
    esc(out, n.name);
    out += ",\"module\":";
    esc(out, n.module);
    out += ",\"signals\":[";
    for (size_t i = 0; i < n.sigs.size(); i++) {
        auto& s = n.sigs[i];
        if (i)
            out += ',';
        out += "{\"name\":";
        esc(out, s.name);
        out += ",\"width\":" + std::to_string(s.width) + ",\"kind\":";
        esc(out, s.kind);
        if (!s.dir.empty()) {
            out += ",\"dir\":";
            esc(out, s.dir);
        }
        out += '}';
    }
    out += "],\"children\":[";
    for (size_t i = 0; i < n.children.size(); i++) {
        if (i)
            out += ',';
        emitScope(out, *n.children[i]);
    }
    out += "]}";
}

} // namespace

extern "C" {

// `args` is a NUL-separated list of slang command-line words, ending with
// an empty word. Returns malloc'd JSON; free with fdv_slang_free.
char* fdv_slang_compile(const char* args) {
    std::vector<std::string> words{"slang"};
    for (const char* p = args; *p; p += std::strlen(p) + 1)
        words.emplace_back(p);
    std::vector<const char*> argv;
    for (auto& w : words)
        argv.push_back(w.c_str());

    std::string out = "{";
    try {
        Driver driver;
        driver.addStandardArgs();
        if (!driver.parseCommandLine((int)argv.size(), argv.data()) || !driver.processOptions()) {
            out += "\"ok\":false,\"diagnostics\":[{\"severity\":\"error\",\"message\":";
            esc(out, "bad front-end options");
            out += "}]}";
            return dup(out);
        }
        if (!driver.parseAllSources()) {
            // Parse errors are still reported through the compilation below.
        }
        auto comp = driver.createCompilation();
        auto& sm = driver.sourceManager;
        DiagnosticEngine engine(sm);

        bool ok = true;
        out += "\"diagnostics\":[";
        bool first = true;
        for (auto& d : comp->getAllDiagnostics()) {
            auto sev = engine.getSeverity(d.code, d.location);
            if (sev == DiagnosticSeverity::Ignored || sev == DiagnosticSeverity::Note)
                continue;
            bool err = sev == DiagnosticSeverity::Error || sev == DiagnosticSeverity::Fatal;
            ok &= !err;
            if (!first)
                out += ',';
            first = false;
            out += "{\"severity\":";
            esc(out, err ? "error" : "warning");
            out += ",\"message\":";
            esc(out, engine.formatMessage(d));
            if (d.location.valid()) {
                auto loc = sm.getFullyOriginalLoc(d.location);
                out += ",\"file\":";
                esc(out, sm.getFullPath(loc.buffer()).string());
                out += ",\"line\":" + std::to_string(sm.getLineNumber(loc));
                out += ",\"column\":" + std::to_string(sm.getColumnNumber(loc));
            }
            out += '}';
        }
        out += "],\"tops\":[";
        auto& root = comp->getRoot();
        first = true;
        for (auto inst : root.topInstances) {
            if (!first)
                out += ',';
            first = false;
            esc(out, inst->name);
        }
        out += "],\"instances\":[";
        std::string signals = "\"signals\":[";
        bool firstInst = true, firstSig = true;
        // The scope tree for the design browser, built alongside.
        std::vector<std::unique_ptr<ScopeNode>> tops;
        std::vector<ScopeNode*> stack;
        auto local = [&](const Symbol& s) {
            std::string p = s.getHierarchicalPath();
            const std::string& base = stack.back()->path;
            return p.size() > base.size() + 1 && p.compare(0, base.size(), base) == 0 && p[base.size()] == '.'
                       ? p.substr(base.size() + 1)
                       : std::string(s.name);
        };
        // A signal for the current scope; a port's internal variable or net
        // only fills in its kind.
        auto addSig = [&](const Symbol& s, uint64_t width, const char* kind) {
            if (stack.empty() || s.name.empty())
                return;
            auto name = local(s);
            for (auto& q : stack.back()->sigs)
                if (q.name == name)
                    return;
            stack.back()->sigs.push_back({name, width, kind, ""});
        };
        root.visit(makeVisitor(
            [&](auto& v, const InstanceSymbol& inst) {
                if (!firstInst)
                    out += ',';
                firstInst = false;
                out += "{\"path\":";
                esc(out, inst.getHierarchicalPath());
                out += ",\"module\":";
                esc(out, inst.getDefinition().name);
                out += '}';
                auto node = std::make_unique<ScopeNode>();
                node->path = inst.getHierarchicalPath();
                node->name = std::string(inst.name);
                node->module = std::string(inst.getDefinition().name);
                for (auto ps : inst.body.getPortList()) {
                    if (ps->kind != SymbolKind::Port || ps->name.empty())
                        continue;
                    auto& port = ps->as<PortSymbol>();
                    node->sigs.push_back(
                        {std::string(port.name), port.getType().getBitWidth(), "port", dirName(port.direction)});
                }
                ScopeNode* raw = node.get();
                if (stack.empty())
                    tops.push_back(std::move(node));
                else
                    stack.back()->children.push_back(std::move(node));
                stack.push_back(raw);
                v.visitDefault(inst);
                stack.pop_back();
            },
            [&](auto&, const VariableSymbol& s) {
                if (s.lifetime != VariableLifetime::Automatic)
                    addSig(s, s.getType().getBitWidth(), "variable");
                if (!firstSig)
                    signals += ',';
                firstSig = false;
                signals += "{\"path\":";
                esc(signals, s.getHierarchicalPath());
                signals += ",\"width\":" + std::to_string(s.getType().getBitWidth()) + "}";
            },
            [&](auto&, const ParameterSymbol& s) {
                addSig(s, s.getType().getBitWidth(), "parameter");
                if (!firstSig)
                    signals += ',';
                firstSig = false;
                signals += "{\"path\":";
                esc(signals, s.getHierarchicalPath());
                signals += ",\"width\":" + std::to_string(s.getType().getBitWidth()) + ",\"constant\":true}";
            },
            [&](auto&, const EnumValueSymbol& s) {
                if (!firstSig)
                    signals += ',';
                firstSig = false;
                signals += "{\"path\":";
                esc(signals, s.getHierarchicalPath());
                signals += ",\"width\":" + std::to_string(s.getType().getBitWidth()) + ",\"constant\":true}";
            },
            [&](auto&, const NetSymbol& s) {
                addSig(s, s.getType().getBitWidth(), "net");
                if (!firstSig)
                    signals += ',';
                firstSig = false;
                signals += "{\"path\":";
                esc(signals, s.getHierarchicalPath());
                signals += ",\"width\":" + std::to_string(s.getType().getBitWidth()) + "}";
            }));
        out += "]," + signals + "],\"hierarchy\":[";
        for (size_t i = 0; i < tops.size(); i++) {
            if (i)
                out += ',';
            emitScope(out, *tops[i]);
        }
        out += std::string("],\"ok\":") + (ok ? "true" : "false") + "}";
    }
    catch (const std::exception& e) {
        out = "{\"ok\":false,\"diagnostics\":[{\"severity\":\"error\",\"message\":";
        esc(out, std::string("slang: ") + e.what());
        out += "}]}";
    }
    return dup(out);
}

void fdv_slang_free(char* p) {
    std::free(p);
}

const char* fdv_slang_version(void) {
    return "slang";
}

} // extern "C"
