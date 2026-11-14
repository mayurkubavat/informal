// IEEE 1800-2017 11.4.6: ? / x / z bits of the right operand of ==? match anything.
//% clock clk
//% assert -name a_eq {(addr ==? 4'b1??0) == (addr[3] && !addr[0])}
//% assert -name a_neq {(addr !=? 4'b1xx0) == !(addr[3] && !addr[0])}
//= a_eq proven
//= a_neq proven
module wildcard_eq(input logic clk, input logic [3:0] addr);
  wire unused = clk;
endmodule
