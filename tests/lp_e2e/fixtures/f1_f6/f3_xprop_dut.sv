module f3_xprop_dut (
    input  logic       clk,
    input  logic       rst_n,
    input  logic       load_en,
    input  logic       we_en,
    input  logic [7:0] din,
    input  logic [12:0] raddr,
    output logic [7:0] clean_out,
    output logic [7:0] reconv_out,
    output logic [7:0] leaky_out,
    output logic [7:0] ctrl_leaky_out,
    output logic [7:0] mem_rdata
);
    logic [7:0]  uninit_q;
    logic        uninit_sel;
    logic [12:0] uninit_waddr;
    logic [7:0]  ram [0:8191];

    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) clean_out <= 8'h00;
        else        clean_out <= din;
    end

    // Reset-less registers: hold power-on X until enabled
    always_ff @(posedge clk) begin
        if (load_en) begin
            uninit_q     <= din;
            uninit_sel   <= din[0];
            uninit_waddr <= {5'd0, din};
        end
    end

    // Memory with uninitialized write address when we_en is high
    always_ff @(posedge clk) begin
        if (we_en) begin
            ram[uninit_waddr] <= din;
        end
    end

    // Reconvergent mux: both arms constant 8'hA5 -> exact 2-copy miter proves X-clean
    assign reconv_out     = uninit_q[0] ? 8'hA5 : 8'hA5;
    assign leaky_out      = uninit_q;
    assign ctrl_leaky_out = uninit_sel ? 8'h11 : 8'h22;
    assign mem_rdata      = ram[raddr];
endmodule
