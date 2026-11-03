# JasperGold-style setup for the FIFO.
clear -all
analyze -sv12 [file join [file dirname [info script]] fifo.sv]
elaborate -top fifo
clock clk
reset -expression {!rst_n}
set_prove_time_limit 60s

assert -name a_count_max   {count <= 3'd4}
assert -name a_full_empty  {!(full && empty)}
assert -name a_ptr_diff    {(wp - rp) == count[1:0]}
assert -name a_no_overflow {full && push && !pop |=> full}
assert -name a_status      {status == 2'd2 |-> full}
assert -name a_data        {push && empty && !pop ##1 !pop |-> dout == $past(din)}
assert -name a_wrong       {count != 3'd3}
# Internal signal of a sub-instance (slang front end only).
if {[info exists ::fdv_frontend_yosys] == 0} {
    assert -name a_sub_ptr {u_wp.p == wp}
}
cover  -name c_full        {full}
cover  -name c_status3     {status == 2'd3}
cover  -name c_count5      {count == 3'd5}
prove -all
