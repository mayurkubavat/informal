module lpv_pmu_bad_seq (
    input  logic clk,
    input  logic rst_n,
    input  logic sleep_req,
    output logic pwr_sw_en,
    output logic iso_en,
    output logic ret_save,
    output logic ret_restore
);
    typedef enum logic [2:0] {
        ST_RUN     = 3'd0,
        ST_BAD_OFF = 3'd1,
        ST_ISO     = 3'd2,
        ST_SAVE    = 3'd3,
        ST_SLEEP   = 3'd4,
        ST_PWRON   = 3'd5,
        ST_RESTORE = 3'd6
    } pmu_state_t;

    pmu_state_t st_q;

    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            st_q <= ST_RUN;
        end else begin
            case (st_q)
                // Seeded PMU bug: drops pwr_sw_en 1 cycle BEFORE iso_en asserts!
                ST_RUN:     if (sleep_req) st_q <= ST_BAD_OFF;
                ST_BAD_OFF: st_q <= ST_ISO;
                ST_ISO:     st_q <= ST_SAVE;
                ST_SAVE:    st_q <= ST_SLEEP;
                ST_SLEEP:   if (!sleep_req) st_q <= ST_PWRON;
                ST_PWRON:   st_q <= ST_RESTORE;
                ST_RESTORE: st_q <= ST_RUN;
                default:    st_q <= ST_RUN;
            endcase
        end
    end

    assign iso_en      = (st_q == ST_ISO) || (st_q == ST_SAVE) || (st_q == ST_SLEEP) || (st_q == ST_PWRON) || (st_q == ST_RESTORE);
    assign ret_save    = (st_q == ST_SAVE);
    assign pwr_sw_en   = (st_q != ST_SLEEP) && (st_q != ST_BAD_OFF);
    assign ret_restore = (st_q == ST_RESTORE);
endmodule

module lpv_core_bad_seq (
    input  logic       clk,
    input  logic       rst_n,
    input  logic       load_en,
    input  logic       use_scratch,
    input  logic [7:0] din,
    output logic [7:0] core_dout
);
    logic [7:0] ret_state_q;
    logic [7:0] scratch_q;

    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            ret_state_q <= 8'h00;
            scratch_q   <= 8'h00;
        end else if (load_en) begin
            ret_state_q <= din + 8'h01;
            scratch_q   <= din ^ 8'h55;
        end
    end

    assign core_dout = use_scratch ? scratch_q : ret_state_q;
endmodule

module lpv_pmu_soc (
    input  logic       clk,
    input  logic       rst_n,
    input  logic       VDD_AON,
    input  logic       VSS,
    input  logic       sleep_req,
    input  logic       load_en,
    input  logic       use_scratch,
    input  logic [7:0] din,
    output logic [7:0] dout,
    output logic       pwr_ack
);
    logic pwr_sw_en_w;
    logic iso_en_w;
    logic ret_save_w;
    logic ret_restore_w;
    logic [7:0] core_dout_w;

    assign pwr_ack = pwr_sw_en_w;

    lpv_pmu_bad_seq u_pmu (
        .clk         (clk),
        .rst_n       (rst_n),
        .sleep_req   (sleep_req),
        .pwr_sw_en   (pwr_sw_en_w),
        .iso_en      (iso_en_w),
        .ret_save    (ret_save_w),
        .ret_restore (ret_restore_w)
    );

    lpv_core_bad_seq u_core (
        .clk         (clk),
        .rst_n       (rst_n),
        .load_en     (load_en),
        .use_scratch (use_scratch),
        .din         (din),
        .core_dout   (core_dout_w)
    );

    assign dout = iso_en_w ? 8'h00 : core_dout_w;
endmodule
