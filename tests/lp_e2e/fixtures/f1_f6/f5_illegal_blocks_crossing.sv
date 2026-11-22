module f5_blk (
    input  logic       clk,
    input  logic       rst_n,
    input  logic [7:0] d,
    output logic [7:0] q
);
    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) q <= 8'h00;
        else        q <= d;
    end
endmodule

module f5_illegal_blocks_crossing (
    input  logic       clk,
    input  logic       rst_n,
    input  logic       en_a,
    input  logic       en_b,
    input  logic [7:0] din,
    output logic [7:0] dout
);
    logic [7:0] w_ab;

    f5_blk u_a (.clk(clk), .rst_n(rst_n), .d(din),  .q(w_ab));
    f5_blk u_b (.clk(clk), .rst_n(rst_n), .d(w_ab), .q(dout));
endmodule
