analyze -format sverilog [file join [file dirname [info script]] soc_lp_clean.sv]
elaborate soc_lp_clean
create_clock clk -period 10
create_reset rst_n -sense low
load_upf [file join [file dirname [info script]] soc_lp_clean.upf]
check_lp -stage upf
check_lp -stage rtl
check_lp -stage all -timeout 30s
puts [report_lp -summary]
puts [report_lp -json]
set wid [set_lp_waiver -add -rule {PORT_*} -comment {test waiver syntax}]
set_lp_waiver -list
if {[info exists wid] && $wid ne ""} {
    set_lp_waiver -remove $wid
}
compare_upf -golden [file join [file dirname [info script]] soc_lp_clean.upf] -revised [file join [file dirname [info script]] soc_lp_clean.upf]
check_lpv -stage formal
