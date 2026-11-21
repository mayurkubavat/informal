# Formal Bus Protocol VIP Lab (`check_protocol`)
clear -all
analyze -sv12 [file join [file dirname [info script]] apb_slave.sv]
elaborate -top apb_slave
clock clk
reset -expression {!rst_n}
set_prove_time_limit 30s

check_protocol -create -protocol apb -role slave -prove
puts [check_protocol -report]
