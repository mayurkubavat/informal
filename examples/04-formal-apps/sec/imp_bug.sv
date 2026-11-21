// SEC Buggy Implementation (subtle corner bug when en && a == 8'hA5)
module mac_bug (
    input  logic       clk,
    input  logic       rst_n,
    input  logic       en,
    input  logic [7:0] a,
    input  logic [7:0] b,
    output logic [7:0] acc_out
);
    logic [7:0] acc_q;
    always_ff @(posedge clk) begin
        if (!rst_n)  acc_q <= 8'd0;
        else if (en) acc_q <= acc_q + (a ^ b) + ((a == 8'hA5) ? 8'd2 : 8'd1);
    end
    assign acc_out = acc_q;
endmodule
