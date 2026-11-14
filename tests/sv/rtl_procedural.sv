// A concurrent assertion in a clocked block under a condition starts an
// attempt only when the flow reaches it: `if (en) assert property (p)` is
// `en |-> p`, and one in the else branch of a reset is off during reset.
//% clock clk
//% reset -expression {rst}
//% assert -name x_step {en |=> q == $past(q) + 4'd1}
//% assert -name x_any {q == $past(q) + 4'd1}
//= x_step proven
//= x_any cex
//= rtl_procedural.a_step proven
//= rtl_procedural.a_hold proven
//= rtl_procedural.a_wrong cex
//= rtl_procedural.a_case proven
module rtl_procedural(input logic clk, input logic rst, input logic en, output logic [3:0] q);
  always_ff @(posedge clk)
    if (rst) q <= 4'd0;
    else begin
      if (en) begin
        q <= q + 4'd1;
        a_step: assert property (1'b1 |=> q == $past(q) + 4'd1);
      end else begin
        a_hold: assert property (1'b1 |=> $stable(q));
      end
      a_wrong: assert property (1'b1 |=> q == $past(q) + 4'd1);
      case (q)
        4'd15: a_case: assert property (en |=> q == 4'd0);
        default: ;
      endcase
    end
endmodule
