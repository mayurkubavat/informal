# Third-party tools and libraries

What informal builds against, pinned to the exact commits in use on the
development machine. Everything lives in one tools directory outside this
repository: set `INFORMAL_TOOLS_DIR` to it (the default is
`~/Silicon/DV/tools`). The build scripts find each library there, or
through the environment variable listed.

Policy: we keep local patches only, never upstream ones. As of 2026-11-08
none of the checkouts below carries a local patch (all working trees are
clean at the listed commit). When a patch is needed, commit it on a local
branch named `informal` in that checkout and list it in the table at the end.

## Pinned versions

The commits of the external tools are in `tools.lock` at the top of the
repository, which `scripts/check.sh` checks (`scripts/tools.sh check`): a
tool at another commit, not built, or not the `ric3` / `abc` on `PATH`
fails the gate. `scripts/tools.sh build <name>` checks out the pinned commit
and builds it with the steps below. The table repeats the commits for
reading; when a pin moves, change both.

| Tool | License | Commit | Upstream | Used for | Found through |
| --- | --- | --- | --- | --- | --- |
| svlang | Apache-2.0 | 0.5.1 = `a7501b4` (untagged): the `rev` in the root `Cargo.toml`, its commit in `Cargo.lock` (`scripts/repin.sh` moves it) | github.com/mayurkubavat/svlang | SystemVerilog front end (crate `informal-svlang`, D14) | Cargo git dependency |
| Bitwuzla | MIT | `9d391d1` (2026-10-30, main) | github.com/bitwuzla/bitwuzla | Word-level BMC `informal-wbmc`; its bundled CaDiCaL is also informal's SAT solver | `INFORMAL_BITWUZLA_DIR`, needs `install/` |
| CaDiCaL (bundled) | MIT | `rel-3.0.1` with Bitwuzla's patches | via Bitwuzla's meson subproject | SAT for all in-house bit-level engines | linked from Bitwuzla's static library |
| CaDiCaL (standalone) | MIT | `c607304` = `rel-3.0.1` | github.com/arminbiere/cadical | Fallback when Bitwuzla is not built | `INFORMAL_CADICAL_DIR`, needs `build/libcadical.a` |
| rIC3 | BSD-3-Clause (LICENSE file; its Cargo.toml says GPL-3.0) | `4d63899` (`v1.5.2-117`) | github.com/gipsyh/rIC3 | External engine and soundness oracle (`ric3`, `ric3-ic3`, `ric3-bmc`) | `~/.local/bin/ric3` symlink |
| ABC | BSD-style (UC Berkeley) | `77875a3b6` (2026-11-01, master) | github.com/berkeley-abc/abc | External engines `abc-pdr`, `abc-pdr-pp`, `abc-bmc3` | `~/.local/bin/abc` symlink |
| Certifaiger | MIT | `b540f0c` (2026-11-05, main; version 10.4.1) | github.com/Froleyks/certifaiger | Tests only: checks the witness circuits that `informal check --certificate` writes for proofs (`scripts/certificate_check.py`, D18); never linked or shipped | `INFORMAL_TOOLS_DIR/certifaiger/bin/certifaiger`, or `CERTIFAIGER` |
| AIGER tools | MIT | `039ec1a` (2026-01-08, master) | github.com/arminbiere/aiger | Tests only: `aigsim` replays counterexamples, `aigsplit` and `aigtocnf` prepare Certifaiger's conditions, `aigfuzz` makes the random models of `scripts/fuzz.py` (nightly run); its library is compiled into Certifaiger | `INFORMAL_TOOLS_DIR/aiger/<tool>`, or `AIGSIM`, `AIGSPLIT`, `AIGTOCNF` |
| Kissat | MIT | `8af8e56` = `rel-4.0.4` | github.com/arminbiere/kissat | Tests only: the SAT solver for Certifaiger's conditions, as in Certifaiger's own scripts | `INFORMAL_TOOLS_DIR/kissat/build/kissat`, or `KISSAT` |
| btor2tools (BtorSim) | MIT | `e6941e3` (2026-10-21, master) | github.com/Boolector/btor2tools | Tests only: `scripts/btorsim_replay.py` replays the counterexamples of `tests/sv` in the gate; never linked or shipped | `INFORMAL_TOOLS_DIR/btor2tools/build/bin/btorsim`, or `BTORSIM` |

How each one reaches InFormal: CaDiCaL and Bitwuzla are linked
statically; rIC3 and ABC run as separate programs; svlang is compiled in
as a Cargo dependency. Bitwuzla also brings symfpu (GPL-3.0 or
BSD-3-Clause, our choice) and links GMP and MPFR (LGPL-3.0) dynamically.
None of these licenses restricts InFormal's own (Apache-2.0, decision D16).
Their source is not in this repository.

System packages (Ubuntu 26.04): `tcl8.6-dev` 8.6.17 (Tcl license, BSD-style; crate `informal-tcl`),
meson (Bitwuzla build), and for the desktop app webkit2gtk-4.1, gtk3,
libsoup3 and librsvg2 dev packages. Yosys is no longer used (D14).
`scripts/bootstrap.sh --apt` installs the Debian/Ubuntu packages (it also
needs `libgmp-dev`, `libmpfr-dev`, `meson`, `ninja-build` and `cmake` for
Bitwuzla) and builds everything of `tools.lock`. Where only git reaches
GitHub, `scripts/wrap_prefill.py` makes Bitwuzla's CaDiCaL and mimalloc
archives from git, checked against the hashes in Bitwuzla's wrap files.

macOS (MacBook, arm64): Homebrew has no `tcl-tk@8` any more; point
`INFORMAL_TCL_LIB_DIR` at a Tcl 8.6 library (8.6.15 works). GMP needs
`RUSTFLAGS="-L /opt/homebrew/lib"`. Bitwuzla installs its library in
`install/lib`, not `install/lib/x86_64-linux-gnu`; `scripts/tools.sh`
accepts either.

Not installed: Pono and BtorMC (their `engines.toml` entries are
unchecked), Surfer (the app's trace hand-off hides itself without it).

## Build steps

Run from the tools directory (`$INFORMAL_TOOLS_DIR`). Rebuild informal afterwards (`cargo build`),
since the build scripts link whatever they find. The original builds
were run by hand and not logged; these commands are reconstructed from
the recorded build configuration (Bitwuzla meson options: release,
static, CaDiCaL only, no Python), so check the first rebuild against it.

```sh
# Bitwuzla: static, CaDiCaL only, installed into bitwuzla/install
cd bitwuzla && ./configure.py --static --prefix "$PWD/install" && \
  cd build && meson compile && meson install && cd ../..

# CaDiCaL standalone (only needed without Bitwuzla)
cd cadical && ./configure -fPIC && make -j16 && cd ..

# btor2tools (BtorSim, for the gate's witness replay)
git clone https://github.com/Boolector/btor2tools && (cd btor2tools && ./configure.sh && cd build && make)

# The certificate checkers (for scripts/certificate_check.py): the AIGER
# tools, Certifaiger compiled against their library, and Kissat.
# Certifaiger's own `make` builds the same binary and also downloads the
# tools around it; this compiles its one source file with what is here.
git clone https://github.com/arminbiere/aiger && (cd aiger && ./configure.sh && make aigsim aigsplit aigtocnf aigfuzz)
git clone https://github.com/Froleyks/certifaiger && (cd certifaiger && mkdir -p bin && \
  g++ -std=c++23 -O2 -DNDEBUG -DVERSION='"10.4.1"' -DGITID="\"$(git rev-parse --short HEAD)\"" \
      -I ../aiger -x c++ src/certifaiger.cpp -x c ../aiger/aiger.c -o bin/certifaiger)
git clone https://github.com/arminbiere/kissat && (cd kissat && ./configure && make)

# rIC3 (stable Rust)
cd rIC3 && cargo build --release && cd ..
ln -sf "$PWD/rIC3/target/release/ric3" ~/.local/bin/ric3

# ABC
make -C abc -j16 ABC_USE_NO_READLINE=1 abc
ln -sf "$PWD/abc/abc" ~/.local/bin/abc
```

## Updating a pin

1. Put the new commit in `tools.lock`, run `scripts/tools.sh build <name>`,
   then rebuild informal.
2. Run `scripts/check.sh`, then the HWMCC'24 sample
   (`scripts/inhouse_sample.sh`) and `scripts/hwmcc_check.py`: an engine
   or SAT update must show 0 disagreements before it is kept.
3. Update the commit in the table above in the same commit as any code
   change it needed.

## Local patches

None yet.
