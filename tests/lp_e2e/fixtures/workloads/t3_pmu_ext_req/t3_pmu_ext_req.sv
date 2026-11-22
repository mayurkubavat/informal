module pmu_ext_sw (
    input  logic       clk,
    input  logic       rst_n,
    input  logic [7:0] din,
    output logic [7:0] data_o,
    output logic [7:0] dbg_uniso_o
);
    logic [7:0] q_data;
    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            q_data <= 8'h00;
        end else begin
            q_data <= din + 8'h01;
        end
    end
    assign data_o      = q_data;
    assign dbg_uniso_o = q_data ^ 8'hF0;
endmodule

module t3_pmu_ext_req (
    input  logic       clk,
    input  logic       rst_n,
    input  logic       VDD_AON,
    input  logic       VSS,
    input  logic       sleep_req,
    input  logic       wake_req,
    input  logic [7:0] din,
    output logic [7:0] dout,
    output logic [7:0] dbg_out
);
    typedef enum logic [2:0] {
        PM_ON      = 3'd0,
        PM_ISO     = 3'd1,
        PM_OFF     = 3'd2,
        PM_WAKE    = 3'd3,
        PM_BAD_OFF = 3'd4
    } pm_t;

    pm_t st_q;
    logic iso_en;
    logic pwr_en;
    logic pwr_ack;
    logic [7:0] data_w;
    logic [7:0] dbg_w;

    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            st_q <= PM_ON;
        end else begin
            case (st_q)
                PM_ON: begin
                    // Relies on external system constraint !(sleep_req && wake_req)
                    if (sleep_req && wake_req) begin
                        st_q <= PM_BAD_OFF;
                    end else if (sleep_req) begin
                        st_q <= PM_ISO;
                    end
                end
                PM_BAD_OFF: st_q <= PM_WAKE;
                PM_ISO:     st_q <= PM_OFF;
                PM_OFF:     if (wake_req) st_q <= PM_WAKE;
                PM_WAKE:    st_q <= PM_ON;
                default:    st_q <= PM_ON;
            endcase
        end
    end

    assign iso_en  = (st_q == PM_ISO) || (st_q == PM_OFF) || (st_q == PM_WAKE);
    assign pwr_en  = (st_q != PM_OFF) && (st_q != PM_BAD_OFF);
    assign pwr_ack = pwr_en;

    pmu_ext_sw u_sw (
        .clk         (clk),
        .rst_n       (rst_n),
        .din         (din),
        .data_o      (data_w),
        .dbg_uniso_o (dbg_w)
    );

    assign dout    = iso_en ? 8'h00 : data_w;
    assign dbg_out = dbg_w;
endmodule
