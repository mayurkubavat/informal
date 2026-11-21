# Formal App Lab — Security Path Verification (`check_spv`)

Proves 2-safety non-interference for `secret_key`:
- `iso_pub`: `proven` (no path to public status)
- `iso_dbg_locked`: `proven` when `!debug_unlock`
- `leak_dbg_open`: `cex` (unconditional debug leak)
- `leak_covert_2cyc`: `cex` (catches a 2-cycle covert storage channel through `s1 -> s2`)

```sh
informal shell examples/04-formal-apps/spv/run.jg.tcl
```
