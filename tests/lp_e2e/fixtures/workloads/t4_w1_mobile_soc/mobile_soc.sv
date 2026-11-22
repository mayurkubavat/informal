module aon_pmu (
    input  logic       clk,
    input  logic       rst_n,
    input  logic       sleep_req,
    output logic       iso_cpu,
    output logic       iso_sram,
    output logic       psw_cpu,
    output logic       psw_sram,
    output logic       ret_save,
    output logic       ret_restore,
    output logic [7:0] aon_hb
);
    typedef enum logic [2:0] {
        ST_RUN     = 3'd0,
        ST_ISO     = 3'd1,
        ST_SAVE    = 3'd2,
        ST_SLEEP   = 3'd3,
        ST_WAKE    = 3'd4,
        ST_RESTORE = 3'd5
    } state_t;

    state_t st_q;
    logic [7:0] hb_q;

    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            st_q <= ST_RUN;
            hb_q <= 8'h00;
        end else begin
            hb_q <= hb_q + 8'h01;
            case (st_q)
                ST_RUN:     if (sleep_req) st_q <= ST_ISO;
                ST_ISO:     st_q <= ST_SAVE;
                ST_SAVE:    st_q <= ST_SLEEP;
                ST_SLEEP:   if (!sleep_req) st_q <= ST_WAKE;
                ST_WAKE:    st_q <= ST_RESTORE;
                ST_RESTORE: st_q <= ST_RUN;
                default:    st_q <= ST_RUN;
            endcase
        end
    end

    assign iso_cpu     = (st_q != ST_RUN);
    assign iso_sram    = (st_q != ST_RUN);
    assign psw_cpu     = (st_q != ST_SLEEP);
    assign psw_sram    = (st_q != ST_SLEEP);
    assign ret_save    = (st_q == ST_SAVE);
    assign ret_restore = (st_q == ST_RESTORE);
    assign aon_hb      = hb_q;
endmodule

module cpu_cluster (
    input  logic        clk,
    input  logic        rst_n,
    input  logic [63:0] din,
    output logic [63:0] cpu_data_bus,
    output logic [7:0]  debug_bus,
    output logic [7:0]  cpu_to_io
);
    logic [63:0] pipe_q;
    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            pipe_q <= 64'h0;
        end else begin
            pipe_q <= din ^ 64'h0000_0000_CAFE_BABE;
        end
    end
    assign cpu_data_bus = pipe_q;
    assign debug_bus    = pipe_q[7:0];
    assign cpu_to_io    = pipe_q[15:8];
endmodule

module sram_island (
    input  logic       clk,
    input  logic       rst_n,
    input  logic       we,
    input  logic [1:0] addr,
    input  logic [7:0] wdata,
    output logic [7:0] sram_rdata
);
    logic [7:0] mem [0:3];
    logic [7:0] ret_state;

    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            ret_state <= 8'h00;
            mem[0]    <= 8'h00;
            mem[1]    <= 8'h00;
            mem[2]    <= 8'h00;
            mem[3]    <= 8'h00;
        end else begin
            if (we) begin
                mem[addr] <= wdata;
            end
            ret_state <= mem[addr] ^ wdata;
        end
    end
    assign sram_rdata = ret_state;
endmodule

module io_ring (
    input  logic       clk,
    input  logic       rst_n,
    input  logic [7:0] from_cpu,
    input  logic [7:0] from_aon,
    output logic [7:0] pad_out,
    output logic [3:0] io_to_aon
);
    logic [7:0] pad_q;
    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            pad_q <= 8'h00;
        end else begin
            pad_q <= from_cpu ^ from_aon;
        end
    end
    assign pad_out   = pad_q;
    assign io_to_aon = pad_q[3:0];
endmodule

module pure_ft_wrap (
    input  logic [7:0] ft_in,
    output logic [7:0] ft_out
);
    assign ft_out = ft_in;
endmodule

module mobile_soc_top (
    input  logic        clk,
    input  logic        rst_n,
    input  logic        VDD_AON,
    input  logic        VDD_IO,
    input  logic        VSS,
    input  logic        sleep_req,
    input  logic        sram_we,
    input  logic [1:0]  sram_addr,
    input  logic [63:0] din,
    output logic [63:0] cpu_bus_o,
    output logic [7:0]  dbg_o,
    output logic [7:0]  sram_o,
    output logic [7:0]  pad_out,
    output logic [7:0]  ft_o,
    output logic [3:0]  io_aon_o,
    output logic        psw_cpu_ack,
    output logic        psw_sram_ack
);
    logic iso_cpu;
    logic iso_sram;
    logic psw_cpu;
    logic psw_sram;
    logic ret_save;
    logic ret_restore;
    logic [7:0]  aon_hb;
    logic [63:0] cpu_data_bus_w;
    logic [7:0]  debug_bus_w;
    logic [7:0]  cpu_to_io_w;
    logic [7:0]  sram_rdata_w;
    logic [7:0]  ft_out_w;

    assign psw_cpu_ack  = psw_cpu;
    assign psw_sram_ack = psw_sram;

    aon_pmu u_pmu (
        .clk         (clk),
        .rst_n       (rst_n),
        .sleep_req   (sleep_req),
        .iso_cpu     (iso_cpu),
        .iso_sram    (iso_sram),
        .psw_cpu     (psw_cpu),
        .psw_sram    (psw_sram),
        .ret_save    (ret_save),
        .ret_restore (ret_restore),
        .aon_hb      (aon_hb)
    );

    cpu_cluster u_cpu (
        .clk          (clk),
        .rst_n        (rst_n),
        .din          (din),
        .cpu_data_bus (cpu_data_bus_w),
        .debug_bus    (debug_bus_w),
        .cpu_to_io    (cpu_to_io_w)
    );

    sram_island u_sram (
        .clk        (clk),
        .rst_n      (rst_n),
        .we         (sram_we),
        .addr       (sram_addr),
        .wdata      (din[7:0]),
        .sram_rdata (sram_rdata_w)
    );

    io_ring u_io (
        .clk      (clk),
        .rst_n    (rst_n),
        .from_cpu (cpu_to_io_w),
        .from_aon (aon_hb),
        .pad_out  (pad_out),
        .io_to_aon(io_aon_o)
    );

    pure_ft_wrap u_ft (
        .ft_in  (aon_hb),
        .ft_out (ft_out_w)
    );

    assign cpu_bus_o = cpu_data_bus_w;
    assign dbg_o     = debug_bus_w;
    assign sram_o    = sram_rdata_w;
    assign ft_o      = ft_out_w;
endmodule
