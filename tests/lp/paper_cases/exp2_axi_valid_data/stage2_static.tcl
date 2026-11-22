set here [file dirname [info script]]
analyze -sv [file join $here axi_valid_data.sv]
elaborate -top axi_valid_data
load_upf [file join $here axi_valid_data.upf]
check_lp -stage rtl
report_lp -verbose
puts [report_lp -json]
