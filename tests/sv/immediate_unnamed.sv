// Unlabelled, multi-instance, loop-unrolled, and $error procedural immediate
// checks are each given a unique hierarchical name and proved end-to-end.
//% clock clk
//% reset -expression {!rst_n}
//= immediate_unnamed.a_loop proven
//= immediate_unnamed.a_loop_2 proven
//= immediate_unnamed._assert_1 proven
//= immediate_unnamed._assert_2 cex
//= immediate_unnamed._cover_1 covered
//= immediate_unnamed.u0.a_inst proven
//= immediate_unnamed.u0._assert_3 proven
//= immediate_unnamed.u1.a_inst proven
//= immediate_unnamed.u1._assert_4 cex
module imm_leaf #(parameter bit BAD = 1'b0) (
    input logic clk,
    input logic rst_n,
    input logic [3:0] q
);
  always_ff @(posedge clk) begin
    if (rst_n) begin
      a_inst: assert (q <= 4'd9);
      assert (!BAD || q != 4'd3);
    end
  end
endmodule

module immediate_unnamed(input logic clk, input logic rst_n, output logic [3:0] q);
  always_ff @(posedge clk) if (!rst_n) q <= 4'd0; else q <= (q == 4'd9) ? 4'd0 : q + 4'd1;

  imm_leaf #(.BAD(1'b0)) u0 (.clk(clk), .rst_n(rst_n), .q(q));
  imm_leaf #(.BAD(1'b1)) u1 (.clk(clk), .rst_n(rst_n), .q(q));

  always_ff @(posedge clk) begin
    if (rst_n) begin
      for (int i = 0; i < 2; i++) begin
        a_loop: assert (q[i] == q[i]);
      end
      if (q > 4'd9) $error("unreachable overflow");
      assert (q != 4'd7);
      cover (q == 4'd4);
    end
  end
endmodule
