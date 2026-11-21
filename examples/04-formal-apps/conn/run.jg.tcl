# Formal Connectivity Lab (`check_conn`)
clear -all
set dir [file dirname [info script]]
analyze -sv12 [file join $dir soc_top.sv]
elaborate -top soc_top
clock clk
reset -expression {!rst_n}
set_prove_time_limit 30s

check_conn -load [file join $dir conn.csv]
puts "Loaded rules: [check_conn -list]"
prove -task Connectivity
puts [check_conn -report]
