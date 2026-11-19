// Two assumptions that conflict once the counter reaches 2: m_on keeps the
// counter counting and m_stop forbids it at 2. No trace gets past that
// point, so a_le (which fails at 6 without m_stop) is proved vacuously.
module conflict(input logic clk, rst, en, output logic [3:0] cnt);
  always_ff @(posedge clk) if (rst) cnt <= 0; else if (en) cnt <= cnt + 1;
  m_on: assume property (@(posedge clk) disable iff (rst) en);
  m_stop: assume property (@(posedge clk) disable iff (rst) cnt == 2 |-> !en);
  a_le: assert property (@(posedge clk) disable iff (rst) cnt <= 5);
endmodule
