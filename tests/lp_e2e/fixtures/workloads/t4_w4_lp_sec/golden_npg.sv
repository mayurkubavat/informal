module golden_npg (
    input  logic       clk,
    input  logic       rst_n,
    input  logic       VDD_AON,
    input  logic       VSS,
    input  logic       sleep_req,
    input  logic [7:0] din,
    output logic [7:0] dout,
    output logic       valid_out
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
    logic [7:0] acc_q;
    logic [7:0] bias_q;

    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            st_q   <= ST_RUN;
            acc_q  <= 8'h00;
            bias_q <= 8'h00;
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
            if (st_q == ST_RUN) begin
                acc_q  <= acc_q + din;
                bias_q <= din ^ 8'h33;
            end
        end
    end

    assign dout      = (st_q == ST_RUN) ? (acc_q ^ bias_q) : 8'h00;
    assign valid_out = (st_q == ST_RUN);
endmodule
