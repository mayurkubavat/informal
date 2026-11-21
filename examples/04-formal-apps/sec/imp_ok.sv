// SEC/LEC Equivalent Refactored Implementation
module mac_imp (
    input  logic       clk,
    input  logic       rst_n,
    input  logic       en,
    input  logic [7:0] a,
    input  logic [7:0] b,
    output logic [7:0] acc_out
);
    logic [7:0] acc_q;
    wire  [7:0] delta = ((a & ~b) | (~a & b)) + 8'h01;
    always_ff @(posedge clk) begin
        if (!rst_n) acc_q <= 8'd0;
        else        acc_q <= en ? (acc_q + delta) : acc_q;
    end
    assign acc_out = acc_q;
endmodule
