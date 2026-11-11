// IEEE 1800-2017 11.8.2: in an unsigned context a signed operand is
// zero-extended (4'sb1111 + 8'd1 is 8'h10, not 8'h00).
//% clock clk
//% assert -name a_mixed {sum == 8'h10}
//% assert -name a_signed {ssum == 8'sh00}
//% assert -name a_assign {wide == 8'hff}
//= a_mixed proven
//= a_signed proven
//= a_assign proven
module signed_context(input logic clk);
  logic signed [3:0] s;
  logic [7:0] u;
  logic signed [7:0] su;
  wire [7:0] sum = s + u;
  wire signed [7:0] ssum = s + su;
  wire [7:0] wide = s;      // assignment: extends by the right-hand side's type
  assign s = -4'sd1;
  assign u = 8'd1;
  assign su = 8'sd1;
endmodule
