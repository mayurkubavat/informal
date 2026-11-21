# Lab 02 — FPV Fundamentals (FIFO)
clear -all
analyze -sv12 [file join [file dirname [info script]] fifo.sv]
elaborate -top fifo -create_related_covers {precondition witness}
clock clk
reset -expression {!rst_n}
set_prove_time_limit 30s

# 1. Audit formal setup and design statistics before proving
check_fv_setup
get_design_info

# 2. Structural & temporal assertions (including sub-instance XMR)
assert -name a_count_max   {count <= 3'd4}
assert -name a_full_empty  {!(full && empty)}
assert -name a_ptr_diff    {(wp - rp) == count[1:0]}
assert -name a_no_overflow {full && push && !pop |=> full}
assert -name a_status      {status == 2'd2 |-> full}
assert -name a_data        {push && empty && !pop ##1 !pop |-> dout == $past(din)}
assert -name a_sub_ptr     {u_wp.p == wp}

# Reachability covers proving the design is not over-constrained
cover  -name c_full        {full}
cover  -name c_status3     {status == 2'd3}
cover  -name c_count5      {count == 3'd5}

# 3. Verify assumption environment consistency & 2QBF dead-end freedom
puts "Environment check: [check_assumptions -depth 12 -conflict -dead_end]"

# 4. Prove main signoff task (<embedded>) with proof cores
prove -all -with_proof_core
report -detail -property a_data

# 5. Isolated exploratory task with a task-local failing assertion + why_cex
task -create CexDemo -copy {a_count_max} -set
assert -name a_wrong {count != 3'd3}
prove -task CexDemo
puts "=== Causal slice for CexDemo::a_wrong ==="
puts [why_cex -property CexDemo::a_wrong]
task -set <embedded>
