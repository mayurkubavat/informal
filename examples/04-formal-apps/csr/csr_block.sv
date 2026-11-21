// Formal CSR / RAL Verification Lab (`check_csr`)
// Implements CFG_REGWEN (RW0C), CTRL (RW + RO gated by CFG_REGWEN),
// IRQ_STATE (RW1C), and KEY_WO (WO, reads 0).
module csr_block (
    input  logic        clk,
    input  logic        rst_n,
    input  logic        we,
    input  logic        re,
    input  logic [7:0]  addr,
    input  logic [31:0] wdata,
    output logic [31:0] rdata
);
    logic       cfg_regwen_q;
    logic [3:0] ctrl_mode_q;
    logic [3:0] ctrl_rev_id_q;
    logic [3:0] irq_state_pending_q;
    logic [7:0] key_wo_secret_q;

    always_ff @(posedge clk) begin
        if (!rst_n) begin
            cfg_regwen_q        <= 1'b1;
            ctrl_mode_q         <= 4'h5;
            ctrl_rev_id_q       <= 4'hA;
            irq_state_pending_q <= 4'hF;
            key_wo_secret_q     <= 8'h00;
        end else if (we) begin
            case (addr)
                8'h00: cfg_regwen_q        <= cfg_regwen_q & wdata[0];
                8'h04: if (cfg_regwen_q) ctrl_mode_q <= wdata[3:0];
                8'h08: irq_state_pending_q <= irq_state_pending_q & ~wdata[3:0];
                8'h0C: key_wo_secret_q     <= wdata[7:0];
                default: ;
            endcase
        end
    end

    always_comb begin
        rdata = 32'h0;
        if (re && !we) begin
            case (addr)
                8'h00: rdata = {31'd0, cfg_regwen_q};
                8'h04: rdata = {24'd0, ctrl_rev_id_q, ctrl_mode_q};
                8'h08: rdata = {28'd0, irq_state_pending_q};
                8'h0C: rdata = 32'h0; // Write-only reads zero
                default: rdata = 32'h0;
            endcase
        end
    end
endmodule
