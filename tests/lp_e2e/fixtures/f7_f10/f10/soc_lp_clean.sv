module pmu_aon (
    input  logic clk,
    input  logic rst_n,
    input  logic sleep_req,
    output logic pwr_sw_en,
    output logic pwr_ack,
    output logic iso_en,
    output logic save,
    output logic restore
);
    localparam logic [2:0] S_RUN     = 3'd0;
    localparam logic [2:0] S_ISO_ON  = 3'd1;
    localparam logic [2:0] S_SAVE    = 3'd2;
    localparam logic [2:0] S_OFF     = 3'd3;
    localparam logic [2:0] S_ON      = 3'd4;
    localparam logic [2:0] S_RESTORE = 3'd5;
    localparam logic [2:0] S_ISO_OFF = 3'd6;

    logic [2:0] st;
    logic       ack_q;

    assign pwr_ack = ack_q;

    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            ack_q <= 1'b1;
        end else begin
            ack_q <= pwr_sw_en;
        end
    end

    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            st        <= S_RUN;
            pwr_sw_en <= 1'b1;
            iso_en    <= 1'b0;
            save      <= 1'b0;
            restore   <= 1'b0;
        end else begin
            case (st)
                S_RUN: begin
                    pwr_sw_en <= 1'b1;
                    iso_en    <= 1'b0;
                    save      <= 1'b0;
                    restore   <= 1'b0;
                    if (sleep_req) begin
                        iso_en <= 1'b1;
                        st     <= S_ISO_ON;
                    end
                end
                S_ISO_ON: begin
                    save <= 1'b1;
                    st   <= S_SAVE;
                end
                S_SAVE: begin
                    save      <= 1'b0;
                    pwr_sw_en <= 1'b0;
                    st        <= S_OFF;
                end
                S_OFF: begin
                    if (!sleep_req) begin
                        pwr_sw_en <= 1'b1;
                        st        <= S_ON;
                    end
                end
                S_ON: begin
                    if (pwr_ack) begin
                        restore <= 1'b1;
                        st      <= S_RESTORE;
                    end
                end
                S_RESTORE: begin
                    restore <= 1'b0;
                    iso_en  <= 1'b0;
                    st      <= S_ISO_OFF;
                end
                S_ISO_OFF: begin
                    st <= S_RUN;
                end
                default: begin
                    st        <= S_RUN;
                    pwr_sw_en <= 1'b1;
                    iso_en    <= 1'b0;
                    save      <= 1'b0;
                    restore   <= 1'b0;
                end
            endcase
        end
    end
endmodule

module core_island (
    input  logic       clk,
    input  logic       rst_n,
    input  logic       en,
    input  logic [7:0] data_in,
    output logic [7:0] core_out
);
    logic [7:0] reg_q;

    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            reg_q <= 8'h00;
        end else if (en) begin
            reg_q <= data_in;
        end
    end

    assign core_out = reg_q;
endmodule

module ret_island (
    input  logic       clk,
    input  logic       rst_n,
    input  logic       en,
    input  logic [7:0] din,
    output logic [7:0] ret_out
);
    logic [7:0] shadow_q;

    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            shadow_q <= 8'h00;
        end else if (en) begin
            shadow_q <= din;
        end
    end

    assign ret_out = shadow_q;
endmodule

module aon_sink (
    input  logic       clk,
    input  logic       rst_n,
    input  logic [7:0] aon_in,
    output logic [7:0] aon_out
);
    logic [7:0] out_q;

    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            out_q <= 8'h00;
        end else begin
            out_q <= aon_in;
        end
    end

    assign aon_out = out_q;
endmodule

module soc_lp (
    input  logic       clk,
    input  logic       rst_n,
    input  logic       req,
    input  logic       sleep_req,
    input  logic [7:0] data_in,
    output logic [7:0] data_out,
    output logic       iso_ack
);
    logic       pwr_sw_en;
    logic       pwr_ack;
    logic       iso_en;
    logic       save;
    logic       restore;
    logic [7:0] core_out;
    logic [7:0] ret_out;

    pmu_aon u_pmu (
        .clk       (clk),
        .rst_n     (rst_n),
        .sleep_req (sleep_req),
        .pwr_sw_en (pwr_sw_en),
        .pwr_ack   (pwr_ack),
        .iso_en    (iso_en),
        .save      (save),
        .restore   (restore)
    );

    core_island u_core (
        .clk      (clk),
        .rst_n    (rst_n),
        .en       (req && pwr_sw_en && !iso_en),
        .data_in  (data_in),
        .core_out (core_out)
    );

    ret_island u_ret (
        .clk     (clk),
        .rst_n   (rst_n),
        .en      (req && !iso_en),
        .din     (core_out),
        .ret_out (ret_out)
    );

    aon_sink u_aon_sink (
        .clk     (clk),
        .rst_n   (rst_n),
        .aon_in  (ret_out),
        .aon_out (data_out)
    );

    assign iso_ack = iso_en;
endmodule
