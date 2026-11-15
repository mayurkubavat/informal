// A clock divided in the design: `half` toggles on clk, and the counter
// `s` runs on half's rising edges, so on every second rising edge of clk.
// A register on the divided clock takes the data from before the edge
// that made half rise: `snap` gets the fast counter's old value, which is
// always even here (a simulator, which runs the divided clock's block
// after that edge's updates, would show the new, odd one).
//% clock clk
//% reset -expression {rst}
//% assert -name x_rate {$rose(half) |-> s == $past(s) + 4'd1}
//% assert -name x_rest {!$rose(half) |-> s == $past(s)}
//% assert -name x_even {seen |-> !snap[0]}
//% assert -name x_odd {seen |-> snap[0]}
//% cover -name c_three {s == 4'd3}
//= x_rate proven
//= x_rest proven
//= x_even proven
//= x_odd cex
//= c_three covered
module divided_clock(input logic clk, input logic rst);
  logic       half;
  logic       seen = 1'b0;
  logic [3:0] f, snap;
  logic [3:0] s = 4'd0;
  always_ff @(posedge clk) if (rst) half <= 1'b0; else half <= ~half;
  always_ff @(posedge clk) if (rst) f <= 4'd0; else f <= f + 4'd1;
  always_ff @(posedge half) begin
    s <= s + 4'd1;
    snap <= f;
    seen <= 1'b1;
  end
endmodule
