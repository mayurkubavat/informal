// IEEE 1800-2017 9.2.2.4: two always_ff blocks may drive disjoint slices of
// one variable (the second block's used to replace the first's).
//% clock clk
//% reset -expression {rst}
//% assert -name a_lo {v[3:0] == 4'h0 || v[3:0] == 4'h5}
//% assert -name a_hi {v[7:4] == 4'h0 || v[7:4] == 4'ha}
//% assert -name a_both {v == 8'h00 || v == 8'ha5}
//% assert -name a_zero {v == 8'h00}
//= a_lo proven
//= a_hi proven
//= a_both proven
//= a_zero cex
module multi_driver_ff(input logic clk, input logic rst);
  logic [7:0] v;
  always_ff @(posedge clk) if (rst) v[3:0] <= 4'h0; else v[3:0] <= 4'h5;
  always_ff @(posedge clk) if (rst) v[7:4] <= 4'h0; else v[7:4] <= 4'ha;
endmodule
