// stopat cuts a signal from its drivers: it is free on every step. A
// property that relied on the driver can now fail; one that does not read
// the cut signal still holds, and so does the signal the cut one copied.
//% clock clk
//% reset -expression {rst}
//% stopat u.o cnt
//% assert -name a_through_cut {y == d + 4'd1}
//% assert -name a_cut_reg {cnt <= 4'd9}
//% assert -name a_beside {z == (d ^ 4'h5)}
//% assert -name a_copied {w == (d ^ 4'h5)}
//= a_through_cut cex
//= a_cut_reg cex
//= a_beside proven
//= a_copied proven
module stopat_child(input logic [3:0] i, output logic [3:0] o);
  assign o = i + 4'd1;
endmodule
module stopat(input logic clk, input logic rst, input logic [3:0] d);
  logic [3:0] y, z, w, cnt;
  stopat_child u(.i(d), .o(y));
  assign w = d ^ 4'h5;
  assign z = w;
  always_ff @(posedge clk) if (rst) cnt <= 4'd0; else if (cnt != 4'd9) cnt <= cnt + 4'd1;
endmodule
