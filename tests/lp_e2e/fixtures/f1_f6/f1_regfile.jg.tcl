clear -all
analyze -sv12 [file join [file dirname [info script]] f1_regfile.sv]
elaborate -top f1_regfile
clock clk
reset -expression {!rst_n}
set_prove_time_limit 20s

assert -name a_bound   {cnt <= 3'd6}
assert -name a_clr     {clr |=> cnt == 3'd0}
assert -name a_shadow  {q == shadow_q}
cover  -name c_full    {full}
prove -all
