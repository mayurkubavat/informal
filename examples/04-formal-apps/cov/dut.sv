// Formal Coverage & UNR Lab (`check_cov`)
module cov_dut (
    input  logic       clk,
    input  logic       rst_n,
    input  logic [1:0] mode,
    output logic       checked_q,
    output logic       shadow_unobs_q,
    output logic       stuck_zero_q,
    output logic       dead_branch_y
);
    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            checked_q      <= 1'b0;
            shadow_unobs_q <= 1'b0;
            stuck_zero_q   <= 1'b0;
        end else begin
            checked_q      <= (mode == 2'b01);
            shadow_unobs_q <= (mode == 2'b10);
            stuck_zero_q   <= 1'b0; // stuck at zero -> UNR toggle
        end
    end

    always_comb begin
        if (mode == 2'b11) // unreachable under assumption `m_no_mode3` -> UNR branch
            dead_branch_y = 1'b1;
        else
            dead_branch_y = checked_q;
    end
endmodule
