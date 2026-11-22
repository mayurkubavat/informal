module pa_core_sec (
    input  logic       clk,
    input  logic       core_rst_n,
    input  logic       en,
    input  logic [7:0] din,
    output logic       core_valid,
    output logic [7:0] core_out
);
    logic [7:0] cfg_q;

    always_ff @(posedge clk or negedge core_rst_n) begin
        if (!core_rst_n) begin
            cfg_q <= 8'h00;
        end else if (en) begin
            cfg_q <= din;
        end
    end

    assign core_valid = 1'b1;
    assign core_out   = cfg_q;
endmodule

module pa_soc (
    input  logic       clk,
    input  logic       rst_n,
    input  logic       sleep_req,
    input  logic       we,
    input  logic [7:0] din,
    output logic       valid_o,
    output logic [7:0] dout
);
    localparam logic [2:0] ST_RUN      = 3'd0;
    localparam logic [2:0] ST_ISO_ON   = 3'd1;
    localparam logic [2:0] ST_SAVE     = 3'd2;
    localparam logic [2:0] ST_PWR_OFF  = 3'd3;
    localparam logic [2:0] ST_PWR_ON   = 3'd4;
    localparam logic [2:0] ST_RESTORE  = 3'd5;

    logic [2:0] state;
    logic       pwr_ctrl;
    logic       pwr_ack;
    logic       iso_en;
    logic       ret_save;
    logic       ret_restore;
    logic       core_valid;
    logic [7:0] core_out;

    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            pwr_ack <= 1'b1;
        end else begin
            pwr_ack <= pwr_ctrl;
        end
    end

    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            state       <= ST_RUN;
            pwr_ctrl    <= 1'b1;
            iso_en      <= 1'b0;
            ret_save    <= 1'b0;
            ret_restore <= 1'b0;
        end else begin
            case (state)
                ST_RUN: begin
                    pwr_ctrl    <= 1'b1;
                    ret_save    <= 1'b0;
                    ret_restore <= 1'b0;
                    if (sleep_req) begin
                        iso_en <= 1'b1;
                        state  <= ST_ISO_ON;
                    end else begin
                        iso_en <= 1'b0;
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
                    if (!sleep_req) begin
                        pwr_ctrl <= 1'b1;
                        state    <= ST_PWR_ON;
                    end
                end
                ST_PWR_ON: begin
                    if (pwr_ack) begin
                        ret_restore <= 1'b1;
                        state       <= ST_RESTORE;
                    end
                end
                ST_RESTORE: begin
                    ret_restore <= 1'b0;
                    iso_en      <= 1'b0;
                    state       <= ST_RUN;
                end
                default: begin
                    state       <= ST_RUN;
                    pwr_ctrl    <= 1'b1;
                    iso_en      <= 1'b0;
                    ret_save    <= 1'b0;
                    ret_restore <= 1'b0;
                end
            endcase
        end
    end

    pa_core_sec u_core (
        .clk        (clk),
        .core_rst_n (rst_n),
        .en         (!iso_en && pwr_ctrl && we),
        .din        (din),
        .core_valid (core_valid),
        .core_out   (core_out)
    );

    assign valid_o = core_valid;
    assign dout    = core_out;
endmodule
