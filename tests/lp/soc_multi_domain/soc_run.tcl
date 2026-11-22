set here [file dirname [info script]]
analyze -sv [file join $here soc_top.sv]
elaborate -top soc_top
load_upf [file join $here soc_clean.upf]
check_lp -stage rtl
report_lp -summary
puts [report_lp -json]
