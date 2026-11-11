// Fields of one packed struct driven from an always_ff and two always_comb blocks.
//% clock clk
//% reset -expression {rst}
//% assert -name a_data {pkt.valid == 1'b0 || pkt.data == 8'h3c}
//% assert -name a_comb {c.lo == d && c.hi == ~d}
//= a_data proven
//= a_comb proven
module multi_driver_struct(input logic clk, input logic rst, input logic [3:0] d);
  typedef struct packed { logic valid; logic [7:0] data; } pkt_t;
  typedef struct packed { logic [3:0] hi; logic [3:0] lo; } c_t;
  pkt_t pkt;
  c_t c;
  always_ff @(posedge clk) if (rst) pkt.valid <= 1'b0; else pkt.valid <= 1'b1;
  always_ff @(posedge clk) if (rst) pkt.data <= 8'h00; else pkt.data <= 8'h3c;
  always_comb c.lo = d;
  always_comb c.hi = ~d;
endmodule
