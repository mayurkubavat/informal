module spec_sw_core (
    input  logic       clk,
    input  logic       rst_n,
    input  logic       ret_save,
    input  logic       ret_restore,
    input  logic [7:0] din,
    output logic [7:0] core_q
);
    logic [7:0] q_reg;
    logic [7:0] q_shadow;

    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            q_reg    <= 8'h00;
            q_shadow <= 8'h00;
        end else begin
            if (ret_save) begin
                q_shadow <= q_reg;
            end
            q_reg <= ret_restore ? q_shadow : (din + 8'h01);
        end
    end

    assign core_q = q_reg;
endmodule

module t3_spec_lp (
    input  logic       clk,
    input  logic       rst_n,
    input  logic       VDD_AON,
    input  logic       VSS,
    input  logic       sleep_req,
    input  logic [7:0] din,
    output logic [7:0] dout
);
    typedef enum logic [2:0] {
        ST_RUN     = 3'd0,
        ST_ISO     = 3'd1,
        ST_SAVE    = 3'd2,
        ST_OFF     = 3'd3,
        ST_ON      = 3'd4,
        ST_RESTORE = 3'd5
    } st_t;

    st_t st_q;
    logic iso_en;
    logic pwr_en;
    logic pwr_ack;
    logic ret_save;
    logic ret_restore;
    logic [7:0] core_q_w;

    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            st_q <= ST_RUN;
        end else begin
            case (st_q)
                ST_RUN:     if (sleep_req) st_q <= ST_ISO;
                ST_ISO:     st_q <= ST_SAVE;
                ST_SAVE:    st_q <= ST_OFF;
                ST_OFF:     if (!sleep_req) st_q <= ST_ON;
                ST_ON:      st_q <= ST_RESTORE;
                ST_RESTORE: st_q <= ST_RUN;
                default:    st_q <= ST_RUN;
            endcase
        end
    end

    assign iso_en      = (st_q != ST_RUN);
    assign ret_save    = (st_q == ST_SAVE);
    assign pwr_en      = (st_q != ST_OFF);
    assign pwr_ack     = pwr_en;
    assign ret_restore = (st_q == ST_RESTORE);

    spec_sw_core u_sw (
        .clk         (clk),
        .rst_n       (rst_n),
        .ret_save    (ret_save),
        .ret_restore (ret_restore),
        .din         (din),
        .core_q      (core_q_w)
    );

    assign dout = iso_en ? 8'h00 : core_q_w;
endmodule
