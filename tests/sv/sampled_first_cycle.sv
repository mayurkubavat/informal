// No reset: $rose / $past have no sampled history in the first cycle, so a
// property that uses them is first checked once that history is real (as
// JasperGold does; it used to read a free value there and fail at cycle 0).
//% clock clk
//% assert -name a_rose {$rose(req) |-> !prev}
//% assert -name a_past {prev2 == $past(req, 2) || !seen2}
//% assert -name a_nested {$past($rose(req)) |-> $past(!prev)}
//% assert -name a_real {$rose(req) |-> prev}
//= a_rose proven
//= a_past proven
//= a_nested proven
//= a_real cex
module sampled_first_cycle(input logic clk, input logic req);
  logic prev = 1'b1, prev2 = 1'b0;
  logic [1:0] n = 2'd0;
  wire seen2 = n == 2'd2;
  always_ff @(posedge clk) begin
    prev <= req;
    prev2 <= prev;
    if (n != 2'd2) n <= n + 2'd1;
  end
endmodule
