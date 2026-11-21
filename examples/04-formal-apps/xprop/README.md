# Formal App Lab — Exact X-Propagation (`check_xprop`)

Demonstrates exact 2-copy self-composition X-propagation:
- `clean_out`: `proven` (reset properly)
- `reconv_out`: `proven` (no false ternary mux X-pessimism!)
- `leaky_out`: `cex` (reset-less flop leaks power-on X), then `waived` via `check_xprop -waiver -add`.

```sh
informal shell examples/04-formal-apps/xprop/run.jg.tcl
```
