module pa_pmu (
    input  logic clk,
    input  logic rst_n,
    input  logic sleep_req,
    output logic run_en,
    output logic iso_en,
    output logic pwr_sw,
    output logic ret_save,
    output logic ret_restore
);
    typedef enum logic [2:0] {
        ST_RUN     = 3'd0,
        ST_ISO     = 3'd1,
        ST_SAVE    = 3'd2,
        ST_SLEEP   = 3'd3,
        ST_PWRON   = 3'd4,
        ST_RESTORE = 3'd5
    } st_t;

    st_t st_q;

    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            st_q <= ST_RUN;
        end else begin
            case (st_q)
                ST_RUN:     if (sleep_req) st_q <= ST_ISO;
                ST_ISO:     st_q <= ST_SAVE;
                ST_SAVE:    st_q <= ST_SLEEP;
                ST_SLEEP:   if (!sleep_req) st_q <= ST_PWRON;
                ST_PWRON:   st_q <= ST_RESTORE;
                ST_RESTORE: st_q <= ST_RUN;
                default:    st_q <= ST_RUN;
            endcase
        end
    end

    assign run_en      = (st_q == ST_RUN);
    assign iso_en      = (st_q != ST_RUN);
    assign ret_save    = (st_q == ST_SAVE);
    assign pwr_sw      = (st_q != ST_SLEEP);
    assign ret_restore = (st_q == ST_RESTORE);
endmodule

module pa_core (
    input  logic       clk,
    input  logic       rst_n,
    input  logic       run_en,
    input  logic [7:0] din,
    output logic [7:0] core_acc
);
    logic [7:0] acc_q;
    logic [7:0] bias_q;

    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            acc_q  <= 8'h00;
            bias_q <= 8'h00;
        end else if (run_en) begin
            acc_q  <= acc_q + din;
            bias_q <= din ^ 8'h33;
        end
    end

    assign core_acc = acc_q ^ bias_q;
endmodule

module pa_rtl (
    input  logic       clk,
    input  logic       rst_n,
    input  logic       VDD_AON,
    input  logic       VSS,
    input  logic       sleep_req,
    input  logic [7:0] din,
    output logic [7:0] dout,
    output logic       valid_out
);
    logic run_en_w;
    logic iso_en_w;
    logic pwr_sw_w;
    logic pwr_ack_w;
    logic ret_save_w;
    logic ret_restore_w;
    logic [7:0] core_acc_w;

    assign pwr_ack_w = pwr_sw_w;

    pa_pmu u_pmu (
        .clk         (clk),
        .rst_n       (rst_n),
        .sleep_req   (sleep_req),
        .run_en      (run_en_w),
        .iso_en      (iso_en_w),
        .pwr_sw      (pwr_sw_w),
        .ret_save    (ret_save_w),
        .ret_restore (ret_restore_w)
    );

    pa_core u_core (
        .clk      (clk),
        .rst_n    (rst_n),
        .run_en   (run_en_w),
        .din      (din),
        .core_acc (core_acc_w)
    );

    assign dout      = iso_en_w ? 8'h00 : core_acc_w;
    assign valid_out = !iso_en_w;
endmodule
