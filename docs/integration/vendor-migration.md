# Vendor Tcl Migration (JasperGold & VC Formal)

`informal shell` runs JasperGold (`jg`) and VC Formal (`vcf`) setup scripts inside a real Tcl 8.6 interpreter (`informal-tcl`), so `proc`, `foreach`, `source`, `include`, namespaces, and `[info script]` work as expected.

---

## 1. Pre-Flight Script Audit (`informal shell --check`)

Before running an existing vendor script, audit it without elaborating RTL or starting solvers:

```sh
informal shell --check path/to/setup.tcl
informal shell --check --json path/to/setup.tcl
```

`--check` walks the entire script (following `source` and `include` recursively) and classifies every finding by file and line number:

| Kind | Exit code impact | Meaning |
| --- | --- | --- |
| `honoured` | Clean (`0`) | Implemented natively by InFormal |
| `ignored` | Clean (`0`) | Vendor-only engine/GUI/proofgrid flag accepted and ignored with a note |
| `error` | Fails (`1`) | Supported command invoked with an unimplemented semantic option |
| `unknown` | Fails (`1`) | Command not implemented in InFormal |
| `tcl` | Fails (`1`) | Tcl runtime error (missing variable or file) |

**Soundness guarantee:** Options that only tune proof grids or GUI display are ignored with a stderr note; options that would *change what is formally checked* are rejected as `error` so a migrated script never passes silently under different semantics.

---

## 2. Flavor Selection (`jg` vs. `vcf`)

InFormal auto-detects the flavor by scanning the script for VC Formal commands (`read_file`, `create_clock`, `create_reset`, `fvassert`, `check_fv`, `report_fv`, `set_fml_*`, `get_props`). Override explicitly when needed:

```sh
informal shell setup.tcl --flavor jg
informal shell setup.tcl --flavor vcf
```

Both flavors remain callable via explicit namespaces (`jg::prove`, `vcf::check_fv`, `informal::set_engines`), and all 7 Formal Apps (`check_*`) plus `help` work in both flavors. See the full [JG ↔ VCF Command Crosswalk](../reference/jg-vcf-crosswalk.md).

---

## 3. Filelists, Macros, and Include Paths

Standard VCS / JasperGold compile words work inside `analyze` and `read_file -vcs {...}`:

```tcl
# Filelists (`-f` / `-F`), environment variable expansion (`$RTL_ROOT`, `${IP_DIR}`),
# preprocessor macros (`+define+K=V`, `-define`, `-D`), and include paths (`+incdir+D`)
analyze -sv12 -f rtl/filelist.f +define+FORMAL+DEPTH=8 +incdir+rtl/inc
```

Relative paths inside a `-f` filelist resolve against the current working directory first, then relative to the filelist directory (matching VCS conventions).

---

## 4. Handling Common Migration Gaps

| Vendor construct | Recommended action in InFormal |
| --- | --- |
| `analyze -vhdl ...` | VHDL is rejected; convert module to SV or black-box with `elaborate -bbox_m {mod}` |
| Time-0 `initial` checks or `$isunknown` | Explicitly opt in with `set_lowering skip_initial_checks on` / `set_lowering isunknown_is_false on` |
| Proof-grid commands (`set_proofgrid_*`) | Accepted and ignored automatically — no edit needed |
| `set_engine_mode {Ht B N}` | Automatically mapped to InFormal engines (or override with `set_engines`) |
| `sim_run -stable` / `sim_save_reset` | Accepted automatically (`reset` models cycle-0 reset natively) |
| Large SRAM arrays (`> 4096` words) | Keep word-level (`informal-wbmc`/`informal-wkind` with Bitwuzla) or auto-box via `elaborate -bbox_a 4096` |

---

## Related References

- Side-by-side command matrix: [JG ↔ VCF Command Crosswalk](../reference/jg-vcf-crosswalk.md)
- Full Tcl command manual: [Tcl Command Reference](../reference/tcl-commands.md)
- Measured public vendor script compatibility: [docs/vendor-script-tally.md](../vendor-script-tally.md)
