module core_blk_b2 (
    input  logic       clk,
    input  logic       core_rst_n,
    input  logic       core_clk_en,
    input  logic       we,
    input  logic [7:0] din,
    output logic [7:0] core_out
);
    logic [7:0] cfg_q;

    always_ff @(posedge clk or negedge core_rst_n) begin
        if (!core_rst_n) begin
            cfg_q <= 8'h00;
        end else if (core_clk_en && we) begin
            cfg_q <= din;
        end
    end

    assign core_out = cfg_q;
endmodule

module bug_iso_drop_early (
    input  logic       clk,
    input  logic       rst_n,
    input  logic       sleep_req,
    input  logic       we,
    input  logic [7:0] din,
    output logic [7:0] dout
);
    localparam logic [2:0] ST_RUN     = 3'd0;
    localparam logic [2:0] ST_ISO_ON  = 3'd1;
    localparam logic [2:0] ST_SAVE    = 3'd2;
    localparam logic [2:0] ST_PWR_OFF = 3'd3;
    localparam logic [2:0] ST_PWR_ON  = 3'd4;
    localparam logic [2:0] ST_ISO_OFF = 3'd5;

    logic [2:0] state;
    logic       pwr_ctrl;
    logic [1:0] pwr_ack_pipe;
    logic       pwr_ack;
    logic       iso_en;
    logic       ret_save;
    logic       ret_restore;
    logic       core_clk_en;
    logic [7:0] core_out;

    assign pwr_ack = pwr_ack_pipe[1];

    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            pwr_ack_pipe <= 2'b11;
        end else begin
            pwr_ack_pipe <= {pwr_ack_pipe[0], pwr_ctrl};
        end
    end

    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            state       <= ST_RUN;
            pwr_ctrl    <= 1'b1;
            iso_en      <= 1'b0;
            ret_save    <= 1'b0;
            ret_restore <= 1'b0;
            core_clk_en <= 1'b1;
        end else begin
            case (state)
                ST_RUN: begin
                    pwr_ctrl    <= 1'b1;
                    iso_en      <= 1'b0;
                    ret_save    <= 1'b0;
                    ret_restore <= 1'b0;
                    if (sleep_req) begin
                        core_clk_en <= 1'b0;
                        iso_en      <= 1'b1;
                        state       <= ST_ISO_ON;
                    end else begin
                        core_clk_en <= 1'b1;
                    end
                end
                ST_ISO_ON: begin
                    ret_save <= 1'b1;
                    state    <= ST_SAVE;
                end
                ST_SAVE: begin
                    ret_save <= 1'b0;
                    pwr_ctrl <= 1'b0;
                    state    <= ST_PWR_OFF;
                end
                ST_PWR_OFF: begin
                    if (!sleep_req && !pwr_ack) begin
                        pwr_ctrl <= 1'b1;
                        state    <= ST_PWR_ON;
                    end
                end
                ST_PWR_ON: begin
                    // BUG: drops iso_en immediately without waiting for 2-cycle pwr_ack!
                    ret_restore <= 1'b1;
                    iso_en      <= 1'b0;
                    state       <= ST_ISO_OFF;
                end
                ST_ISO_OFF: begin
                    ret_restore <= 1'b0;
                    core_clk_en <= 1'b1;
                    state       <= ST_RUN;
                end
                default: begin
                    state       <= ST_RUN;
                    pwr_ctrl    <= 1'b1;
                    iso_en      <= 1'b0;
                    ret_save    <= 1'b0;
                    ret_restore <= 1'b0;
                    core_clk_en <= 1'b1;
                end
            endcase
        end
    end

    core_blk_b2 u_core (
        .clk         (clk),
        .core_rst_n  (rst_n),
        .core_clk_en (core_clk_en),
        .we          (we),
        .din         (din),
        .core_out    (core_out)
    );

    assign dout = core_out;
endmodule
