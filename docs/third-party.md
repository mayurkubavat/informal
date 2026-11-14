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

| Tool | License | Commit | Upstream | Used for | Found through |
| --- | --- | --- | --- | --- | --- |
| svlang | Apache-2.0 | pinned in the root `Cargo.toml` (`rev`) | github.com/mayurkubavat/svlang | SystemVerilog front end (crate `informal-svlang`, D14) | Cargo git dependency |
| slang | MIT | `d02b7cd` (2026-11-02, master) | github.com/MikePopoloski/slang | Optional cross-check lowering (`INFORMAL_FRONTEND=slang`, crate `informal-slang`) | `INFORMAL_SLANG_DIR`, needs `build/lib/libsvlang.a` |
| Bitwuzla | MIT | `9d391d1` (2026-10-30, main) | github.com/bitwuzla/bitwuzla | Word-level BMC `informal-wbmc`; its bundled CaDiCaL is also informal's SAT solver | `INFORMAL_BITWUZLA_DIR`, needs `install/` |
| CaDiCaL (bundled) | MIT | `rel-3.0.1` with Bitwuzla's patches | via Bitwuzla's meson subproject | SAT for all in-house bit-level engines | linked from Bitwuzla's static library |
| CaDiCaL (standalone) | MIT | `c607304` = `rel-3.0.1` | github.com/arminbiere/cadical | Fallback when Bitwuzla is not built | `INFORMAL_CADICAL_DIR`, needs `build/libcadical.a` |
| rIC3 | BSD-3-Clause (LICENSE file; its Cargo.toml says GPL-3.0) | `4d63899` (`v1.5.2-117`) | github.com/gipsyh/rIC3 | External engine and soundness oracle (`ric3`, `ric3-ic3`, `ric3-bmc`) | `~/.local/bin/ric3` symlink |
| ABC | BSD-style (UC Berkeley) | `77875a3b6` (2026-11-01, master) | github.com/berkeley-abc/abc | External engines `abc-pdr`, `abc-pdr-pp`, `abc-bmc3` | `~/.local/bin/abc` symlink |

How each one reaches InFormal: CaDiCaL, Bitwuzla and slang are linked
statically; rIC3 and ABC run as separate programs; svlang is compiled in
as a Cargo dependency. Bitwuzla also brings symfpu (GPL-3.0 or
BSD-3-Clause, our choice) and links GMP and MPFR (LGPL-3.0) dynamically.
None of these licenses restricts InFormal's own (Apache-2.0, decision D16).
Their source is not in this repository.

System packages (Ubuntu 26.04): `tcl8.6-dev` 8.6.17 (Tcl license, BSD-style; crate `informal-tcl`),
meson (Bitwuzla build), and for the desktop app webkit2gtk-4.1, gtk3,
libsoup3 and librsvg2 dev packages. Yosys is no longer used (D14).

Not installed: Pono and BtorMC (their `engines.toml` entries are
unchecked), Surfer (the app's trace hand-off hides itself without it).

## Build steps

Run from the tools directory (`$INFORMAL_TOOLS_DIR`). Rebuild informal afterwards (`cargo build`),
since the build scripts link whatever they find. The original builds
were run by hand and not logged; these commands are reconstructed from
the recorded build configuration (slang `CMakeCache.txt`: Release,
static, PIC; Bitwuzla meson options: release, static, CaDiCaL only, no
Python), so check the first rebuild against it.

```sh
# slang: static release build with the tools
cmake -S slang -B slang/build -DCMAKE_BUILD_TYPE=Release \
      -DCMAKE_POSITION_INDEPENDENT_CODE=ON -DSLANG_INCLUDE_TESTS=OFF
cmake --build slang/build -j16

# Bitwuzla: static, CaDiCaL only, installed into bitwuzla/install
cd bitwuzla && ./configure.py --static --prefix "$PWD/install" && \
  cd build && meson compile && meson install && cd ../..

# CaDiCaL standalone (only needed without Bitwuzla)
cd cadical && ./configure -fPIC && make -j16 && cd ..

# rIC3 (stable Rust)
cd rIC3 && cargo build --release && cd ..
ln -sf "$PWD/rIC3/target/release/ric3" ~/.local/bin/ric3

# ABC
make -C abc -j16 ABC_USE_NO_READLINE=1 abc
ln -sf "$PWD/abc/abc" ~/.local/bin/abc
```

## Updating a pin

1. `git -C <tool> fetch && git -C <tool> checkout <new commit>`, rebuild
   it with the steps above, then rebuild informal.
2. Run `scripts/check.sh`, then the HWMCC'24 sample
   (`scripts/inhouse_sample.sh`) and `scripts/hwmcc_check.py`: an engine
   or SAT update must show 0 disagreements before it is kept.
3. Update the commit in the table above in the same commit as any code
   change it needed.

## Local patches

None yet.
