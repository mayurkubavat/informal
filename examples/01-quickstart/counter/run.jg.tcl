# Lab 01 — Quickstart Counter (JasperGold flavor)
clear -all
analyze -sv12 [file join [file dirname [info script]] counter.sv]
elaborate -top counter
clock clk
reset -expression {!rst_n}
set_prove_time_limit 30s

# Safety assertions
assert -name a_max     {q <= 4'd9}
assert -name a_step    {en && !done |=> q == $past(q) + 4'd1}
assert -name a_hold    {!en |=> $stable(q)}
assert -name a_no_wrap {!wrap}
# Intentional failing assertion to demonstrate CEX generation & why_cex
assert -name a_never9  {q != 4'd9}

# Reachability covers (anti-vacuity + negative reachability check)
cover  -name c_done    {$rose(done)}
cover  -name c_10      {q == 4'd10}

# Primary-input environment assumption
assume -name m_en_burst {en |=> en}

prove -all
echo "=== Causal root-cause slice for a_never9 ==="
puts [why_cex -property a_never9]
