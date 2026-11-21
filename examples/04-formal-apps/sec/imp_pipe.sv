// SEC Retimed 2-Stage Implementation (-latency 1 relative to mac_spec)
module mac_pipe (
    input  logic       clk,
    input  logic       rst_n,
    input  logic       en,
    input  logic [7:0] a,
    input  logic [7:0] b,
    output logic [7:0] acc_out
);
    logic [7:0] acc_q;
    logic [7:0] out_stage_q;
    always_ff @(posedge clk) begin
        if (!rst_n) begin
            acc_q       <= 8'd0;
            out_stage_q <= 8'd0;
        end else begin
            if (en) acc_q <= acc_q + (a ^ b) + 8'd1;
            out_stage_q <= acc_q;
        end
    end
    assign acc_out = out_stage_q;
endmodule
