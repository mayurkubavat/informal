# Formal X-Propagation Lab (`check_xprop`)
clear -all
analyze -sv12 [file join [file dirname [info script]] xprop_dut.sv]
elaborate -top xprop_dut
clock clk
reset -expression {!rst_n}
set_prove_time_limit 20s

check_xprop -init -targets {outputs} -prove
puts "=== Before waiver ==="
puts [check_xprop -report]

check_xprop -waiver -add -instance leaky_out -comment {datapath qualified by valid}
puts "=== After waiver ==="
puts [check_xprop -report]
