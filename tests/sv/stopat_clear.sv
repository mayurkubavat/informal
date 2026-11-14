// stopat -clear removes the cut points again.
//% clock clk
//% reset -expression {rst}
//% stopat cnt
//% stopat -clear
//% assert -name a_reg {cnt <= 4'd9}
//= a_reg proven
module stopat_clear(input logic clk, input logic rst);
  logic [3:0] cnt;
  always_ff @(posedge clk) if (rst) cnt <= 4'd0; else if (cnt != 4'd9) cnt <= cnt + 4'd1;
endmodule
