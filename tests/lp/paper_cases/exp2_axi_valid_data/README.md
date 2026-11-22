# Experiment 2: 64-Bit AXI4-Lite / TL-UL Valid-Isolated / Data-Unisolated Bus

Motivating industrial low-power verification study:
- `u_src` (`PD_SRC`, switchable `0.9V / OFF`) drives a 1-bit `m_valid` control signal and a 64-bit `m_data[63:0]` payload to always-on `u_dst` (`PD_DST`, `0.9V`).
- Only `m_valid` is isolated (clamped `0` by `u_pmu/pmu_iso_en` in `PD_TOP`); `m_data[63:0]` is intentionally unisolated because `u_dst` samples `m_data` only when `m_valid && m_ready`.

## Stage 2 Static Results (`stage2_static.tcl`)
Run:
```bash
./target/debug/informal shell tests/lp/paper_cases/exp2_axi_valid_data/stage2_static.tcl
```
- `u_src/m_valid` passes all static isolation checks (`ISO_SUPPLY_MISMATCH`, `ISO_CTRL_UNPOWERED`, `ISO_CTRL_POLARITY_MISMATCH`, `ISO_CLAMP_MISMATCH`).
- `u_src/m_data[63:0]` is reported as **1** grouped `CrossingGroup` `ISO_MISSING` violation (`width_bits = 64`).
- In WS4 (`check_lp -prove_waivers` / `-prove_corruption`), the 2-copy conditional corruption miter proves that `m_data[63:0]` cannot corrupt `acc_q` or `reconv_masked_q` (`(m_data[0] & ~m_data[0]) ^ acc_q[0]`) under `BAD_VALID_CLAMP = 0`, producing a certified `FormallyWaived` invariant, and produces a concrete `FormalCex` VCD when `BAD_VALID_CLAMP = 1`.
