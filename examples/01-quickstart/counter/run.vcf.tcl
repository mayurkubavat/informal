# Lab 01 — Quickstart Counter (VC Formal flavor)
set_fml_appmode FPV
read_file -top counter -format sverilog -sva -vcs [list [file join [file dirname [info script]] counter.sv]]
create_clock clk -period 100
create_reset rst_n -sense low
sim_run -stable
sim_save_reset
set_fml_var fml_max_time 30s

fvassert a_max     -expr {q <= 4'd9}
fvassert a_step    -expr {en && !done |=> q == $past(q) + 4'd1}
fvassert a_hold    -expr {!en |=> $stable(q)}
fvassert a_no_wrap -expr {!wrap}
fvassert a_never9  -expr {q != 4'd9}

fvcover  c_done    -expr {$rose(done)}
fvcover  c_10      -expr {q == 4'd10}
fvassume m_en_burst -expr {en |=> en}

check_fv -block
report_fv -list
