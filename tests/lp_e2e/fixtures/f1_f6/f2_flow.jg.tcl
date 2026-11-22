clear -all
analyze -sv12 [file join [file dirname [info script]] f2_design.sv]
elaborate -top f2_design
clock clk
reset -expression {!rst_n}
set_prove_time_limit 20s

assert -name a_sub_xor {(en && (din == 8'h00)) |=> (dout == 8'h01)}
cover  -name c_busy    {busy}
check_cov -init -model {coi branch}
check_cov -measure
check_xprop -init -targets {outputs} -prove
prove -all
