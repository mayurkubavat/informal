module f5_uv_blk (
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

module f5_uv_tol (
    input  logic       clk,
    input  logic       rst_n,
    input  logic [7:0] din,
    output logic [7:0] dout
);
    logic [7:0] mid;

    f5_uv_blk u_src (.clk(clk), .rst_n(rst_n), .d(din), .q(mid));
    f5_uv_blk u_dst (.clk(clk), .rst_n(rst_n), .d(mid), .q(dout));
endmodule
