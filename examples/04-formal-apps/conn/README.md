# Formal App Lab — Connectivity (`check_conn`)

Verifies muxed conditional paths (`mux_conn` → `proven`), 1-cycle pipelined connections (`pipe_conn` → `proven`), and catches an inverted-LSB wiring bug (`bad_conn` → `cex`) directly from `conn.csv`.

```sh
informal shell examples/04-formal-apps/conn/run.jg.tcl
```
