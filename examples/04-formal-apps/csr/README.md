# Formal App Lab — CSR / RAL Verification (`check_csr`)

Automatically synthesizes and proves 18+ register assertions from OpenTitan-style `regs.hjson` covering reset values, `RW`, `RO`, `RW0C`, `RW1C`, `WO` read-zero, `regwen` write-enable locks, and non-aliasing.

```sh
informal shell examples/04-formal-apps/csr/run.jg.tcl
```

For machine-readable CI output, use `check_csr -report -json`.
