// Multi-task proof isolation: task t1 assumes mode == 0, task t2 assumes
// mode == 1, and `<embedded>` has neither assumption. Proving all properties
// in one run proves each task's assertion non-vacuously without leaking
// assumptions across tasks or into `<embedded>`.
//% clock clk
//% reset -expression {rst}
//% task -create t1
//% assume -task t1 -name m_t1 {!mode}
//% assert -task t1 -name a_t1 {q <= 2'd1}
//% task -create t2
//% assume -task t2 -name m_t2 {mode}
//% assert -task t2 -name a_t2 {q == 2'd0 || q == 2'd2}
//% assert -name a_emb {q <= 2'd1}
//% set_prove_time_limit 20s
//% prove -task t1
//% prove -task t2
//= t1::a_t1 proven
//= t2::a_t2 proven
//= a_emb cex
module task_isolation(
  input  logic       clk,
  input  logic       rst,
  input  logic       mode,
  input  logic       step,
  output logic [1:0] q
);
  always_ff @(posedge clk) begin
    if (rst) q <= 2'd0;
    else if (step) q <= mode ? 2'd2 : 2'd1;
  end
endmodule
