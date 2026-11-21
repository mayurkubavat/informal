# Formal App Lab — Coverage & UNR (`check_cov`)

Demonstrates COI observability (`checked_q` covered, `shadow_unobs_q` uncovered then waived), formal branch UNR (`mode == 2'b11` proved unreachable under `m_no_mode3`), and toggle UNR (`stuck_zero_q`).

```sh
informal shell examples/04-formal-apps/cov/run.jg.tcl
```

To generate an HTML coverage report, append:

```tcl
check_cov -report -html -report_file cov_report.html -no_return -force
```
