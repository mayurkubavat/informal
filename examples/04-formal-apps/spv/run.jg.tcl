# Formal Security Path Verification Lab (`check_spv`)
clear -all
analyze -sv12 [file join [file dirname [info script]] crypto_wrap.sv]
elaborate -top crypto_wrap
clock clk
reset -expression {!rst_n}
set_prove_time_limit 20s

check_spv -create -name iso_pub          -from secret_key -to pub_status_o
check_spv -create -name iso_dbg_locked   -from secret_key -to debug_port_o -cond {!debug_unlock}
check_spv -create -name leak_dbg_open    -from secret_key -to debug_port_o
check_spv -create -name leak_covert_2cyc -from secret_key -to covert_port_o

check_spv -prove
puts [check_spv -report]
