module f3_why_cex_mux (
    input  logic       clk,
    input  logic       rst_n,
    input  logic       sel_arm1,
    input  logic       trigger_bug,
    input  logic [7:0] heavy_unused_in,
    output logic       err_flag,
    output logic       safe_ok
);
    logic       bug_q;
    logic [7:0] unused_lane_q;

    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            bug_q         <= 1'b0;
            unused_lane_q <= 8'h00;
        end else begin
            bug_q         <= trigger_bug;
            unused_lane_q <= heavy_unused_in + 8'h01;
        end
    end

    assign err_flag = sel_arm1 ? (unused_lane_q == 8'hFF) : bug_q;
    assign safe_ok  = 1'b1;
endmodule
