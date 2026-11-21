# Formal Coverage & UNR Lab (`check_cov`)
clear -all
analyze -sv12 [file join [file dirname [info script]] dut.sv]
elaborate -top cov_dut
clock clk
reset -expression {!rst_n}
set_prove_time_limit 30s

assume -name m_no_mode3 {mode != 2'b11}
assert -name a_chk      {checked_q |-> $past(mode == 2'b01) || !$past(rst_n)}

check_cov -init -model {coi branch toggle}
prove -all
check_cov -measure

# Waive intentionally unobserved debug shadow register
check_cov -waiver -add -type coi -instance shadow_unobs_q -comment {debug shadow}
puts [check_cov -report]
