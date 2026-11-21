# Formal Sequential & Combinational Equivalence Lab (`check_sec`)
clear -all
set dir [file dirname [info script]]
set spec [file join $dir spec.sv]

puts "=== 1. Sequential SEC (spec vs. refactored imp_ok) ==="
check_sec -setup -spec $spec -imp [file join $dir imp_ok.sv] -clock clk -reset {!rst_n} -prove
puts [check_sec -report]

puts "=== 2. Combinational Keypoint LEC (-lec) ==="
check_sec -setup -lec -spec $spec -imp [file join $dir imp_ok.sv] -clock clk -prove
puts [check_sec -report]

puts "=== 3. Pipelined / Retimed SEC (-latency 1) ==="
check_sec -setup -latency 1 -spec $spec -imp [file join $dir imp_pipe.sv] -clock clk -reset {!rst_n} -prove
puts [check_sec -report]

puts "=== 4. Bug Detection (spec vs. imp_bug) + why_cex ==="
check_sec -setup -spec $spec -imp [file join $dir imp_bug.sv] -clock clk -reset {!rst_n} -prove
puts [check_sec -report]
puts [why_cex -property sec_out_acc_out]
