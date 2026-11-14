# Vendor script tally

What `informal shell --check` reports over public JasperGold and VC Formal scripts
(`scripts/vendor_tcl_corpus.py`), as of 2026-11-14. Counts are scripts, not uses.
Regenerate after the shell learns a command; the corpus is kept outside the repo.

Sources (repository, commit, scripts kept):

- lowRISC/opentitan 0d2b191a4a78d4c3fcb487e116eb2e7b54223841 10 scripts
- lowRISC/ibex 3d524f19ee8e464b3fb0e29c77dc9ff45934815a 1 scripts
- black-parrot/black-parrot d7f3156173c857271dc790766f77f5aaa3813dda 1 scripts

12 scripts, 0 would run to the end, 0 not checked

## Commands informal does not have (24)

| scripts | what |
| ---: | --- |
| 5 | `check_cov` |
| 4 | `set_proofgrid_per_engine_max_local_jobs` |
| 2 | `custom_engine` |
| 2 | `set_proofgrid_max_local_jobs` |
| 2 | `set_prove_cache` |
| 2 | `set_prove_cache_mode` |
| 2 | `set_prove_cache_path` |
| 2 | `set_prove_per_property_time_limit` |
| 1 | `blackbox_assistant` |
| 1 | `check_conn` |
| 1 | `check_fv_setup` |
| 1 | `echo` |
| 1 | `get_fv_complexity` |
| 1 | `get_reset_info` |
| 1 | `get_version` |
| 1 | `report_abstractions` |
| 1 | `report_fv_setup` |
| 1 | `set_abstractions` |
| 1 | `set_app_var` |
| 1 | `set_change_at` |
| 1 | `set_grid_usage` |
| 1 | `set_port_direction_handling` |
| 1 | `sizeof_collection` |
| 1 | `vcf_dvsim_report` |

## Commands or options informal rejects (10)

| scripts | what |
| ---: | --- |
| 4 | `assert -regexp` |
| 1 | `assume -from_assert` |
| 1 | `clock_cmd -both_edges` |
| 1 | `clock_cmd -rate` |
| 1 | `fvassume -expr` |
| 1 | `fvassume -regexp` |
| 1 | `get_property_list -regexp` |
| 1 | `get_props -usage` |
| 1 | `report -csv` |
| 1 | `reset -expr` |

## Accepted and ignored (8)

| scripts | what |
| ---: | --- |
| 6 | `task` |
| 5 | `stopat -task` |
| 2 | `elaborate -disable_auto_bbox` |
| 2 | `prove -task` |
| 1 | `check_fv -block` |
| 1 | `report -task` |
| 1 | `set_fml_var fml_max_mem` |
| 1 | `stopat -env` |

## Other Tcl errors (environment, missing procs or files) (19)

| scripts | what |
| ---: | --- |
| 3 | `can't read "...": no such variable` |
| 3 | `cannot read ${COMMON_MSG_TCL_PATH}` |
| 3 | `environment variable COMMON_MSG_TCL_PATH is not set` |
| 3 | `environment variable DUT_TOP is not set` |
| 3 | `no files matched glob pattern "..."` |
| 2 | `cannot read build/psgen.tcl` |
| 2 | `environment variable COV is not set` |
| 2 | `environment variable FPV_DEFINES is not set` |
| 2 | `environment variable LOWRISC_SAIL_SRC is not set` |
| 1 | `Expected exactly one property for reject_preserves_next; found ` |
| 1 | `No AsyncOn localparam in tb` |
| 1 | `Set BP_DIR to the patched BlackParrot checkout.` |
| 1 | `cannot read ${COMMON_RPT_TCL_PATH}` |
| 1 | `environment variable BBOX_CMD is not set` |
| 1 | `environment variable BP_DIR is not set` |
| 1 | `environment variable COMMON_RPT_TCL_PATH is not set` |
| 1 | `environment variable CONN_CSVS is not set` |
| 1 | `environment variable STOPATS is not set` |
| 1 | `environment variable TASK is not set` |

## Commands honoured (uses)

`task` 18, `analyze` 13, `stopat` 7, `clear` 5, `get_design_info` 5, `elaborate` 4, `assert` 4, `clock_cmd` 3, `prove` 3, `report` 3, `reset` 3, `set_fml_var` 2, `get_property_list` 1, `set_prove_time_limit` 1, `set_proofgrid_mode` 1, `check_fv` 1, `create_clock` 1, `create_reset` 1, `report_fv` 1, `set_fml_appmode` 1, `sim_run` 1, `sim_save_reset` 1, `assume` 0, `fvassume` 0, `get_props` 0
