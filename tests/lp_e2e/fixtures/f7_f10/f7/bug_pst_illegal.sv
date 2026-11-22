module pst_core_b4 (
    input  logic       clk,
    input  logic       rst_n,
    input  logic       en,
    input  logic [7:0] din,
    output logic [7:0] core_out
);
    logic [7:0] cfg_q;

    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            cfg_q <= 8'h00;
        end else if (en) begin
            cfg_q <= din;
        end
    end

    assign core_out = cfg_q;
endmodule

module bug_pst_illegal (
    input  logic       clk,
    input  logic       rst_n,
    input  logic       dbg_force_off,
    input  logic [1:0] mode_req,
    input  logic [7:0] din,
    output logic [7:0] dout
);
    localparam logic [1:0] S_RUN   = 2'd0;
    localparam logic [1:0] S_RET   = 2'd1;
    localparam logic [1:0] S_SLEEP = 2'd2;

    logic [1:0] pst_state;
    logic       pwr_ctrl;
    logic       pwr_ack;
    logic       iso_en;
    logic       ret_en;
    logic       ret_save;
    logic       ret_restore;
    logic [7:0] core_out;

    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            pst_state   <= S_RUN;
            pwr_ctrl    <= 1'b1;
            pwr_ack     <= 1'b1;
            iso_en      <= 1'b0;
            ret_en      <= 1'b0;
            ret_save    <= 1'b0;
            ret_restore <= 1'b0;
        end else begin
            pwr_ack <= pwr_ctrl;
            case (pst_state)
                S_RUN: begin
                    ret_restore <= 1'b0;
                    if (dbg_force_off) begin
                        // BUG: enters illegal PST state (!pwr_ctrl && !iso_en)!
                        pwr_ctrl  <= 1'b0;
                        iso_en    <= 1'b0;
                        ret_en    <= 1'b0;
                        pst_state <= S_SLEEP;
                    end else if (mode_req != 2'd0) begin
                        iso_en    <= 1'b1;
                        ret_en    <= 1'b1;
                        ret_save  <= 1'b1;
                        pst_state <= S_RET;
                    end else begin
                        pwr_ctrl <= 1'b1;
                        iso_en   <= 1'b0;
                        ret_en   <= 1'b0;
                        ret_save <= 1'b0;
                    end
                end
                S_RET: begin
                    ret_save <= 1'b0;
                    if (mode_req == 2'd2) begin
                        pwr_ctrl  <= 1'b0;
                        iso_en    <= 1'b1;
                        ret_en    <= 1'b1;
                        pst_state <= S_SLEEP;
                    end else if (mode_req == 2'd0 && pwr_ack) begin
                        iso_en    <= 1'b0;
                        ret_en    <= 1'b0;
                        pst_state <= S_RUN;
                    end
                end
                S_SLEEP: begin
                    if (mode_req != 2'd2 && !dbg_force_off) begin
                        pwr_ctrl    <= 1'b1;
                        iso_en      <= 1'b1;
                        ret_en      <= 1'b1;
                        ret_restore <= 1'b1;
                        pst_state   <= S_RET;
                    end
                end
                default: begin
                    pst_state   <= S_RUN;
                    pwr_ctrl    <= 1'b1;
                    iso_en      <= 1'b0;
                    ret_en      <= 1'b0;
                    ret_save    <= 1'b0;
                    ret_restore <= 1'b0;
                end
            endcase
        end
    end

    pst_core_b4 u_core (
        .clk      (clk),
        .rst_n    (rst_n),
        .en       (pwr_ctrl && !iso_en),
        .din      (din),
        .core_out (core_out)
    );

    assign dout = core_out;
endmodule
