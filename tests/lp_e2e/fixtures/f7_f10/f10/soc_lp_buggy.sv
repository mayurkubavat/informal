module pmu_aon_bug (
    input  logic clk,
    input  logic rst_n,
    input  logic sleep_req,
    output logic pwr_sw_en,
    output logic pwr_ack,
    output logic iso_en,
    output logic save,
    output logic restore
);
    localparam logic [1:0] S_RUN  = 2'd0;
    localparam logic [1:0] S_OFF  = 2'd1;
    localparam logic [1:0] S_WAKE = 2'd2;

    logic [1:0] st;

    assign pwr_ack = pwr_sw_en;

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
                        // Drops power before isolating
                        pwr_sw_en <= 1'b0;
                        st        <= S_OFF;
                    end
                end
                S_OFF: begin
                    iso_en <= 1'b1;
                    if (!sleep_req) begin
                        pwr_sw_en <= 1'b1;
                        iso_en    <= 1'b0;
                        st        <= S_WAKE;
                    end
                end
                S_WAKE: begin
                    restore <= 1'b1;
                    st      <= S_RUN;
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

module core_island_bug (
    input  logic       clk,
    input  logic       rst_n,
    input  logic       en,
    input  logic [7:0] data_in,
    output logic [7:0] core_out,
    output logic       core_alert
);
    logic [7:0] reg_q;
    logic       alert_q;

    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            reg_q   <= 8'h00;
            alert_q <= 1'b0;
        end else if (en) begin
            reg_q   <= data_in;
            alert_q <= data_in[0];
        end
    end

    assign core_out   = reg_q;
    assign core_alert = alert_q;
endmodule

module ret_island_bug (
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

module aon_sink_bug (
    input  logic       clk,
    input  logic       rst_n,
    input  logic [7:0] aon_in,
    input  logic       alert_in,
    output logic [7:0] aon_out
);
    logic [7:0] out_q;

    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            out_q <= 8'h00;
        end else begin
            out_q <= alert_in ? (aon_in ^ 8'hff) : aon_in;
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
    logic       core_alert;
    logic [7:0] ret_out;

    pmu_aon_bug u_pmu (
        .clk       (clk),
        .rst_n     (rst_n),
        .sleep_req (sleep_req),
        .pwr_sw_en (pwr_sw_en),
        .pwr_ack   (pwr_ack),
        .iso_en    (iso_en),
        .save      (save),
        .restore   (restore)
    );

    core_island_bug u_core (
        .clk        (clk),
        .rst_n      (rst_n),
        .en         (req && pwr_sw_en),
        .data_in    (data_in),
        .core_out   (core_out),
        .core_alert (core_alert)
    );

    ret_island_bug u_ret (
        .clk     (clk),
        .rst_n   (rst_n),
        .en      (req),
        .din     (core_out),
        .ret_out (ret_out)
    );

    // Unisolated crossing PD_CORE -> PD_AON (`core_alert` & `core_out`)
    aon_sink_bug u_aon_sink (
        .clk      (clk),
        .rst_n    (rst_n),
        .aon_in   (ret_out),
        .alert_in (core_alert),
        .aon_out  (data_out)
    );

    assign iso_ack = iso_en;
endmodule
