// A request answered one cycle later, for the precondition covers
// (crates/informal-fpv/tests/vacuity.rs). `spare` is a register no
// property reads, a place for a stopat that does not change the proofs.
module request(input logic clk, rst, req, output logic ack, spare);
  always_ff @(posedge clk) begin
    ack <= rst ? 1'b0 : req;
    spare <= rst ? 1'b0 : ~spare;
  end
  a_ack: assert property (@(posedge clk) disable iff (rst) req |=> ack);
  a_off: assert property (@(posedge clk) disable iff (1'b1) req |=> ack);
  a_plain: assert property (@(posedge clk) disable iff (rst) !rst |=> 1'b1);
endmodule
