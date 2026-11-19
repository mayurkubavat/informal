module sva_ranges(input clk, input a, input b);
    // a[->2] means a occurs twice, and the second a occurs after the first.
    assert property (@(posedge clk) a[->1] |-> b);
    // a[=2] means a occurs twice, not necessarily consecutively, and b happens after the second a.
    assert property (@(posedge clk) a[=2] |-> b);
endmodule
