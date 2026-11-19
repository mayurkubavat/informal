clear -all
analyze -sv12 tests/sv/sva_ranges.sv
elaborate -top sva_ranges
# clock clk
assert -name a_cmd {a |-> b}
prove -all
