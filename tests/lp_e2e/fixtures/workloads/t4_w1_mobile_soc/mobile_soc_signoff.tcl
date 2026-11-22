analyze -sv12 mobile_soc.sv
elaborate -top mobile_soc_top
clock clk
reset -expression {!rst_n}
load_upf mobile_soc_clean.upf
check_lp -stage design
report_lp -summary
