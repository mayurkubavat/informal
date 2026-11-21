# Getting Started

This guide takes you from a fresh checkout to your first formal proof, counterexample trace, and desktop workbench session.

---

## 1. Prerequisites

| Dependency | Required? | Purpose |
| --- | --- | --- |
| Rust toolchain (`rustup`, pinned by `rust-toolchain.toml`) | Required | Builds `informal` and `svlang` |
| C++ compiler (`g++` / `clang++`) & Tcl 8.6 headers | Required | Embedded Tcl 8.6 FPV shell (`informal-tcl`) |
| **CaDiCaL** (`rel-3.0.1`) | Recommended | SAT solver powering all in-house bit-level engines (`bmc`, `kind`, `ic3`, `klive`) |
| **Bitwuzla** | Optional | Word-level SMT engines (`informal-wbmc`, `informal-wkind`, unblasted arrays) |
| **rIC3**, **ABC** | Optional | Additional external portfolio engines configured in `config/engines.toml` |
| **Certifaiger**, **aigsim**, **BtorSim** | Optional | Independent external proof and CEX certificate checkers |
| Node.js (pinned in `ui/.nvmrc`) | Optional | Desktop workbench (`InFormal.app`) |

On Ubuntu / Debian, you can install everything automatically via:

```sh
scripts/bootstrap.sh --apt
```

### Manual Setup (Linux & macOS)

```sh
# Linux (Ubuntu/Debian)
sudo apt install build-essential tcl8.6-dev

# macOS (Homebrew)
brew install tcl-tk@8
```

Build CaDiCaL (exact versions and optional Bitwuzla/rIC3/ABC recipes live in [docs/third-party.md](../third-party.md)):

```sh
export INFORMAL_TOOLS_DIR="$HOME/informal-tools"
mkdir -p "$INFORMAL_TOOLS_DIR" && cd "$INFORMAL_TOOLS_DIR"
git clone https://github.com/arminbiere/cadical && cd cadical
git checkout rel-3.0.1 && ./configure -fPIC && make -j4
```

> **Note:** Without CaDiCaL, `informal` still compiles and runs external engines (`rIC3`, `ABC`) if on `PATH`, but in-house bit-level engines are disabled.

---

## 2. Build InFormal

```sh
git clone https://github.com/mayurkubavat/informal && cd informal
# On macOS, limit build parallelism to avoid memory pressure:
CARGO_BUILD_JOBS=2 cargo build --release -p informal-cli
```

Optionally symlink `target/release/informal` onto your `PATH` (for example `~/.local/bin/informal`).

---

## 3. Run Your First Formal Proof

Run the quickstart counter lab (see [Lab 01 — Quickstart Counter](../labs/01-quickstart.md)):

```sh
target/release/informal shell examples/01-quickstart/counter/run.jg.tcl
```

To run the exact same design using VC Formal syntax:

```sh
target/release/informal shell examples/01-quickstart/counter/run.vcf.tcl
```

### Reading the Verdict Table

| Status | Applies to | Meaning |
| --- | --- | --- |
| `proven` | `assert` | Unbounded proof — holds for all legal input traces of arbitrary length |
| `bounded_proven` | `assert` | Holds for all cycles `0..K` (when `-bound K` was requested) |
| `cex` | `assert` | Counterexample found and replayed — VCD written under `--workdir` |
| `covered` | `cover` | Reachable — witness trace found and written as VCD |
| `unreachable` | `cover` | Proven unreachable for all legal traces (potential over-constraint or dead code) |
| `undetermined` | both | Timed out or hit memory cap before settling — see [Convergence & Engines](convergence-and-engines.md) |
| `error` | both | Lowering or SVA error (e.g., unsupported construct or typo) |

---

## 4. Explore Interactively

Start an interactive Tcl prompt in either JasperGold (`jg`) or VC Formal (`vcf`) flavor:

```sh
target/release/informal shell --flavor jg
```

Inside the shell, use the built-in `help` command:

```tcl
informal[jg]> help                ;# categorized command list for active flavor
informal[jg]> help -apps          ;# list all 7 Formal Apps
informal[jg]> help prove          ;# options, equivalents, and examples for `prove`
informal[jg]> help vcf::read_file ;# inspect another flavor's command
```

---

## 5. Launch the Desktop Workbench (Optional)

```sh
cd ui && npm ci && npm run tauri dev
```

For packaging `InFormal.app` and configuring Ask mode or live AI control (`informal mcp --connect`), see [Desktop Workbench UI](workbench-ui.md) and [AI & MCP Integration](../integration/ai-and-mcp.md).

---

## Next Steps

- Learn sound FPV methodology: [FPV Methodology](fpv-methodology.md)
- Work through the hands-on labs: [Hands-On Labs](../labs/README.md)
- Look up any command: [CLI Reference](../reference/cli.md) · [Tcl Command Reference](../reference/tcl-commands.md)
