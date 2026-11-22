module clamp_src (
    input  logic       clk,
    input  logic       rst_n,
    input  logic       we,
    input  logic [7:0] din,
    output logic [7:0] sig_zero,
    output logic [7:0] sig_one,
    output logic [7:0] sig_latch
);
    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            sig_zero  <= 8'h00;
            sig_one   <= 8'hff;
            sig_latch <= 8'h5a;
        end else if (we) begin
            sig_zero  <= din;
            sig_one   <= din ^ 8'hff;
            sig_latch <= din + 8'h01;
        end
    end
endmodule

module iso_clamp_modes (
    input  logic       clk,
    input  logic       rst_n,
    input  logic       sleep_req,
    input  logic       we,
    input  logic [7:0] din,
    output logic [7:0] obs_zero,
    output logic [7:0] obs_one,
    output logic [7:0] obs_latch
);
    localparam logic [2:0] ST_RUN     = 3'd0;
    localparam logic [2:0] ST_ISO_ON  = 3'd1;
    localparam logic [2:0] ST_OFF     = 3'd2;
    localparam logic [2:0] ST_ON      = 3'd3;
    localparam logic [2:0] ST_ISO_OFF = 3'd4;

    logic [2:0] state;
    logic       pwr_ctrl;
    logic       pwr_ack;
    logic       iso_en;
    logic       ret_save;
    logic       ret_restore;
    logic [7:0] sig_zero;
    logic [7:0] sig_one;
    logic [7:0] sig_latch;

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
                    iso_en      <= 1'b0;
                    ret_save    <= 1'b0;
                    ret_restore <= 1'b0;
                    if (sleep_req) begin
                        iso_en   <= 1'b1;
                        ret_save <= 1'b1;
                        state    <= ST_ISO_ON;
                    end
                end
                ST_ISO_ON: begin
                    ret_save <= 1'b0;
                    pwr_ctrl <= 1'b0;
                    state    <= ST_OFF;
                end
                ST_OFF: begin
                    if (!sleep_req) begin
                        pwr_ctrl <= 1'b1;
                        state    <= ST_ON;
                    end
                end
                ST_ON: begin
                    if (pwr_ack) begin
                        ret_restore <= 1'b1;
                        state       <= ST_ISO_OFF;
                    end
                end
                ST_ISO_OFF: begin
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

    clamp_src u_src (
        .clk       (clk),
        .rst_n     (rst_n),
        .we        (we && pwr_ctrl && !iso_en),
        .din       (din),
        .sig_zero  (sig_zero),
        .sig_one   (sig_one),
        .sig_latch (sig_latch)
    );

    assign obs_zero  = sig_zero;
    assign obs_one   = sig_one;
    assign obs_latch = sig_latch;
endmodule
