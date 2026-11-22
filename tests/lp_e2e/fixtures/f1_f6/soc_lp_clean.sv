module pmu_ctrl (
    input  logic clk,
    input  logic rst_n,
    input  logic sleep_core,
    input  logic sleep_ret,
    output logic pwr_core_en,
    output logic iso_core_en,
    output logic pwr_ret_en,
    output logic iso_ret_en,
    output logic ret_save,
    output logic ret_restore
);
    localparam logic [1:0] C_ON     = 2'd0;
    localparam logic [1:0] C_ISO_ON = 2'd1;
    localparam logic [1:0] C_OFF    = 2'd2;
    localparam logic [1:0] C_PWR_ON = 2'd3;

    localparam logic [2:0] R_ON      = 3'd0;
    localparam logic [2:0] R_ISO_ON  = 3'd1;
    localparam logic [2:0] R_SAVE    = 3'd2;
    localparam logic [2:0] R_OFF     = 3'd3;
    localparam logic [2:0] R_PWR_ON  = 3'd4;
    localparam logic [2:0] R_RESTORE = 3'd5;

    logic [1:0] core_st;
    logic [2:0] ret_st;

    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            core_st     <= C_ON;
            pwr_core_en <= 1'b1;
            iso_core_en <= 1'b0;
        end else begin
            case (core_st)
                C_ON: begin
                    pwr_core_en <= 1'b1;
                    iso_core_en <= 1'b0;
                    if (sleep_core) begin
                        iso_core_en <= 1'b1;
                        core_st     <= C_ISO_ON;
                    end
                end
                C_ISO_ON: begin
                    pwr_core_en <= 1'b0;
                    iso_core_en <= 1'b1;
                    core_st     <= C_OFF;
                end
                C_OFF: begin
                    pwr_core_en <= 1'b0;
                    iso_core_en <= 1'b1;
                    if (!sleep_core) begin
                        pwr_core_en <= 1'b1;
                        core_st     <= C_PWR_ON;
                    end
                end
                C_PWR_ON: begin
                    pwr_core_en <= 1'b1;
                    iso_core_en <= 1'b0;
                    core_st     <= C_ON;
                end
                default: begin
                    core_st     <= C_ON;
                    pwr_core_en <= 1'b1;
                    iso_core_en <= 1'b0;
                end
            endcase
        end
    end

    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            ret_st      <= R_ON;
            pwr_ret_en  <= 1'b1;
            iso_ret_en  <= 1'b0;
            ret_save    <= 1'b0;
            ret_restore <= 1'b0;
        end else begin
            case (ret_st)
                R_ON: begin
                    pwr_ret_en  <= 1'b1;
                    iso_ret_en  <= 1'b0;
                    ret_save    <= 1'b0;
                    ret_restore <= 1'b0;
                    if (sleep_ret) begin
                        iso_ret_en <= 1'b1;
                        ret_st     <= R_ISO_ON;
                    end
                end
                R_ISO_ON: begin
                    pwr_ret_en  <= 1'b1;
                    iso_ret_en  <= 1'b1;
                    ret_save    <= 1'b1;
                    ret_restore <= 1'b0;
                    ret_st      <= R_SAVE;
                end
                R_SAVE: begin
                    ret_save    <= 1'b0;
                    ret_restore <= 1'b0;
                    pwr_ret_en  <= 1'b0;
                    iso_ret_en  <= 1'b1;
                    ret_st      <= R_OFF;
                end
                R_OFF: begin
                    pwr_ret_en  <= 1'b0;
                    iso_ret_en  <= 1'b1;
                    ret_save    <= 1'b0;
                    ret_restore <= 1'b0;
                    if (!sleep_ret) begin
                        pwr_ret_en <= 1'b1;
                        ret_st     <= R_PWR_ON;
                    end
                end
                R_PWR_ON: begin
                    pwr_ret_en  <= 1'b1;
                    iso_ret_en  <= 1'b1;
                    ret_save    <= 1'b0;
                    ret_restore <= 1'b1;
                    ret_st      <= R_RESTORE;
                end
                R_RESTORE: begin
                    ret_restore <= 1'b0;
                    ret_save    <= 1'b0;
                    pwr_ret_en  <= 1'b1;
                    iso_ret_en  <= 1'b0;
                    ret_st      <= R_ON;
                end
                default: begin
                    ret_st      <= R_ON;
                    pwr_ret_en  <= 1'b1;
                    iso_ret_en  <= 1'b0;
                    ret_save    <= 1'b0;
                    ret_restore <= 1'b0;
                end
            endcase
        end
    end
endmodule

module switchable_core (
    input  logic        clk,
    input  logic        rst_n,
    input  logic [63:0] din,
    output logic [63:0] data_out
);
    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) data_out <= 64'h0;
        else        data_out <= din ^ 64'hA5A5_5A5A_1234_5678;
    end
endmodule

module middle_feedthrough (
    input  logic [63:0] ft_in,
    output logic [63:0] ft_out
);
    assign ft_out = ft_in;
endmodule

module ret_island (
    input  logic        clk,
    input  logic        rst_n,
    input  logic [63:0] din,
    output logic [63:0] q
);
    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) q <= 64'h0;
        else        q <= din;
    end
endmodule

module io_bridge (
    input  logic        clk,
    input  logic        rst_n,
    input  logic [63:0] din,
    output logic [63:0] dout
);
    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) dout <= 64'h0;
        else        dout <= din;
    end
endmodule

module soc_lp_clean (
    input  logic        clk,
    input  logic        rst_n,
    input  logic        sleep_core,
    input  logic        sleep_ret,
    input  logic [63:0] din,
    output logic [63:0] io_dout,
    output logic        pwr_ack
);
    logic        pwr_core_en, iso_core_en, pwr_ret_en, iso_ret_en, ret_save, ret_restore;
    logic [63:0] core_bus, ft_bus, ret_bus;

    pmu_ctrl u_pmu (
        .clk(clk),
        .rst_n(rst_n),
        .sleep_core(sleep_core),
        .sleep_ret(sleep_ret),
        .pwr_core_en(pwr_core_en),
        .iso_core_en(iso_core_en),
        .pwr_ret_en(pwr_ret_en),
        .iso_ret_en(iso_ret_en),
        .ret_save(ret_save),
        .ret_restore(ret_restore)
    );

    switchable_core u_core (
        .clk(clk),
        .rst_n(rst_n),
        .din(din),
        .data_out(core_bus)
    );

    middle_feedthrough u_mid (
        .ft_in(core_bus),
        .ft_out(ft_bus)
    );

    ret_island u_ret (
        .clk(clk),
        .rst_n(rst_n),
        .din(ft_bus),
        .q(ret_bus)
    );

    io_bridge u_io (
        .clk(clk),
        .rst_n(rst_n),
        .din(ret_bus),
        .dout(io_dout)
    );

    assign pwr_ack = pwr_core_en;
endmodule
