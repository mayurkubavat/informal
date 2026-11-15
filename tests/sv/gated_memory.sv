// A small memory written on a gated clock: its words are registers on
// that clock (memories up to 4096 words are), so a write happens at the
// rising edges of clk where the enable register is high, and a read
// shows the word as written.
//% clock clk
//% reset -expression {rst}
//% assert -name x_read {$past(we_q) && !$past(rst) |-> mem[$past(wa_q)] == $past(wd_q)}
//% assert -name x_keep {!$past(we_q) && !$past(rst) |-> mem[2'd1] == $past(mem[2'd1])}
//% assert -name x_always {!$past(rst) |-> mem[$past(wa_q)] == $past(wd_q)}
//% cover -name c_two {mem[2'd0] == 4'd5 && mem[2'd3] == 4'd9}
//= x_read proven
//= x_keep proven
//= x_always cex
//= c_two covered
module gated_memory(input logic clk, input logic rst, input logic we,
                    input logic [1:0] wa, input logic [3:0] wd);
  logic       we_q, we_l, g;
  logic [1:0] wa_q;
  logic [3:0] wd_q;
  logic [3:0] mem [0:3];
  always_ff @(posedge clk)
    if (rst) we_q <= 1'b0;
    else begin we_q <= we; wa_q <= wa; wd_q <= wd; end
  // The clock-gating cell: the enable is held while clk is high.
  always_latch if (!clk) we_l <= we_q;
  assign g = clk & we_l;
  always_ff @(posedge g) mem[wa_q] <= wd_q;
endmodule
