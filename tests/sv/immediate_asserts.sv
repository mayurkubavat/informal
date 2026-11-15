// An immediate assertion or cover in an always block that has a label of
// its own is a property of the design, listed under that label and proved
// like any other. (They used to be in the model and missing from the table,
// so `prove -all` ran nothing and said nothing.)
//% clock clk
//% reset -expression {!rst_n}
//= a_le9 proven
//= a_never9 cex
//= c_five covered
module immediate_asserts(input logic clk, input logic rst_n, output logic [3:0] q);
  always_ff @(posedge clk) if (!rst_n) q <= 4'd0; else q <= (q == 4'd9) ? 4'd0 : q + 4'd1;
  always_ff @(posedge clk) begin
    if (rst_n) begin
      a_le9: assert (q <= 4'd9);
      a_never9: assert (q != 4'd9);
      c_five: cover (q == 4'd5);
    end
  end
endmodule
