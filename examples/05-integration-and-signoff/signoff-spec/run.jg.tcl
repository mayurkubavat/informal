# Lab 05 — Production CI Signoff Script
clear -all
set dir [file dirname [info script]]
analyze -sv12 [file join $dir signoff_counter.sv]
elaborate -top signoff_counter -create_related_covers {precondition witness}
clock clk
reset -expression {!rst_n}
set_prove_time_limit 30s

assert -name a_max  {q <= 4'd9}
assert -name a_step {en && !done |=> q == $past(q) + 4'd1}
assert -name a_hold {!en |=> $stable(q)}
cover  -name c_done {$rose(done)}

# Environment check before proving
set env_res [check_assumptions -depth 15 -conflict -dead_end]
puts "Environment check: $env_res"

prove -all

# Fail non-zero in CI if any assertion fails/times out or required cover is unreachable
set bad_asserts [concat \
    [get_property_list -include {type assert status cex}] \
    [get_property_list -include {type assert status undetermined}] \
    [get_property_list -include {type assert status error}]]
set bad_covers [get_property_list -include {type cover status unreachable}]

if {[llength $bad_asserts] > 0 || [llength $bad_covers] > 0} {
    error "CI signoff gate failed: bad_asserts={$bad_asserts} bad_covers={$bad_covers}"
}
puts "SIGNOFF GATE: PASS"
