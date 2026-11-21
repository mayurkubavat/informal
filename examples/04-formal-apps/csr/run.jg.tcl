# Formal CSR / RAL Verification Lab (`check_csr`)
clear -all
set dir [file dirname [info script]]
analyze -sv12 [file join $dir csr_block.sv]
elaborate -top csr_block
clock clk
reset -expression {!rst_n}
set_prove_time_limit 30s

check_csr -load [file join $dir regs.hjson] -prove
puts [check_csr -report]
