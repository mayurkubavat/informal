module pmu_ctrl #(
    parameter int BUG_MODE = 0
) (
    input  logic clk,
    input  logic rst_n,
    input  logic sleep_req,
    output logic pwr_en,
    output logic iso_en,
    output logic ret_save,
    output logic ret_restore,
    output logic wake_init
);
    typedef enum logic [2:0] {
        ST_RUN     = 3'd0,
        ST_ISO     = 3'd1,
        ST_SAVE    = 3'd2,
        ST_OFF     = 3'd3,
        ST_ON      = 3'd4,
        ST_RESTORE = 3'd5,
        ST_UNISO   = 3'd6,
        ST_BAD_OFF = 3'd7
    } pmu_st_t;

    pmu_st_t st_q;

    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            st_q <= ST_RUN;
        end else begin
            case (st_q)
                ST_RUN: begin
                    if (sleep_req) begin
                        st_q <= (BUG_MODE == 2) ? ST_BAD_OFF : ST_ISO;
                    end
                end
                ST_BAD_OFF: st_q <= ST_ISO;
                ST_ISO:     st_q <= ST_SAVE;
                ST_SAVE:    st_q <= ST_OFF;
                ST_OFF:     if (!sleep_req) st_q <= ST_ON;
                ST_ON:      st_q <= ST_RESTORE;
                ST_RESTORE: st_q <= ST_UNISO;
                ST_UNISO:   st_q <= ST_RUN;
                default:    st_q <= ST_RUN;
            endcase
        end
    end

    assign iso_en      = (st_q != ST_RUN) && (st_q != ST_BAD_OFF);
    assign ret_save    = (st_q == ST_SAVE);
    assign pwr_en      = (st_q != ST_OFF) && (st_q != ST_BAD_OFF);
    assign ret_restore = (st_q == ST_RESTORE);
    assign wake_init   = (st_q == ST_UNISO);
endmodule

module ret_core #(
    parameter int BUG_MODE = 0
) (
    input  logic       clk,
    input  logic       rst_n,
    input  logic       wake_init,
    input  logic       load_pipe,
    input  logic       use_pipe,
    input  logic [7:0] din,
    output logic [7:0] core_out
);
    logic [7:0] cfg_q;
    logic [7:0] pipe_q;

    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            cfg_q  <= 8'h00;
            pipe_q <= 8'h00;
        end else begin
            if (load_pipe) begin
                cfg_q <= din;
            end
            if (wake_init && (BUG_MODE != 1)) begin
                pipe_q <= 8'h00;
            end else if (load_pipe) begin
                pipe_q <= din ^ 8'h3C;
            end
        end
    end

    assign core_out = use_pipe ? pipe_q : cfg_q;
endmodule

module t3_partial_ret_soc #(
    parameter int BUG_MODE = 0
) (
    input  logic       clk,
    input  logic       rst_n,
    input  logic       VDD_AON,
    input  logic       VSS,
    input  logic       sleep_req,
    input  logic       load_pipe,
    input  logic       use_pipe,
    input  logic [7:0] din,
    output logic [7:0] dout
);
    logic pwr_en_w;
    logic pwr_ack_w;
    logic iso_en_w;
    logic ret_save_w;
    logic ret_restore_w;
    logic wake_init_w;
    logic [7:0] core_out_w;

    assign pwr_ack_w = pwr_en_w;

    pmu_ctrl #(.BUG_MODE(BUG_MODE)) u_pmu (
        .clk         (clk),
        .rst_n       (rst_n),
        .sleep_req   (sleep_req),
        .pwr_en      (pwr_en_w),
        .iso_en      (iso_en_w),
        .ret_save    (ret_save_w),
        .ret_restore (ret_restore_w),
        .wake_init   (wake_init_w)
    );

    ret_core #(.BUG_MODE(BUG_MODE)) u_core (
        .clk       (clk),
        .rst_n     (rst_n),
        .wake_init (wake_init_w),
        .load_pipe (load_pipe),
        .use_pipe  (use_pipe),
        .din       (din),
        .core_out  (core_out_w)
    );

    assign dout = iso_en_w ? 8'h00 : core_out_w;
endmodule
