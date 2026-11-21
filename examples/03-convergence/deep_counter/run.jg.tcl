# Lab 03 — Convergence Engineering (Watchdog + Heavy Multiplier)
clear -all
analyze -sv12 [file join [file dirname [info script]] deep_counter.sv]
elaborate -top deep_counter
clock clk
reset -expression {!rst_n}
set_prove_time_limit 30s

# Enable SHA-256 content-addressed proof caching across repeated runs
set_prove_cache on

# Target properties:
# 1. Safety invariant over the 24-bit watchdog counter
assert -name a_wdog_bound    {wdog_cnt <= 24'd1000000}
# 2. Downstream safety check relying on the bound invariant
assert -name a_expired_exact {wdog_expired |-> wdog_cnt == 24'd1000000}
# 3. Reachability cover at 1,000,000 cycles (impossible by raw BMC without abstraction!)
cover  -name c_wdog_expired  {wdog_expired}
# 4. Cover touching the 16x16 multiplier so complexity profiler flags `u_mul`
cover  -name c_prod_lsb      {mul_prod[0] == 1'b0}

# Step 1: Profile complexity hotspots in the active cone of influence
puts "=== Complexity Profile ==="
puts [get_fv_complexity]

# Step 2: Ask blackbox_assistant to identify & box heavy arithmetic submodule `u_mul`
puts "=== Blackbox Assistant ==="
puts [blackbox_assistant -apply -json]

# Step 3: Prove helper invariant first and promote cleanly via `assume -from_assert`
prove -property a_wdog_bound
assume -from_assert a_wdog_bound

# Step 4: Fast-forward the 24-bit watchdog with sound over-approximate counter abstraction
abstract -counter wdog_cnt -threshold 1000000
puts "=== Active Abstractions ==="
puts [report_abstractions]

# Step 5: Prove remaining properties & reachability covers (completes in < 1s!)
prove -all

# Step 6: Demonstrate task-scoped `stopat` abstraction without modifying `<embedded>`
task -create FreeCnt -copy {a_expired_exact} -set
stopat wdog_cnt
prove -task FreeCnt
task -set <embedded>
