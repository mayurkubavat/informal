// A latch whose data reads its own output oscillates while it is open:
// rejected, not stepped once.
//% clock clk
//% assert -name a1 {q == 4'd0 || q != 4'd0}
//= a1 error
module latch_loop(input logic clk, input logic en, output logic [3:0] q);
  logic [3:0] r;
  always_ff @(posedge clk) r <= q;
  always_latch if (en) q <= q + 4'd1;
endmodule
