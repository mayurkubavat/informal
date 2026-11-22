module equiv_core (
    input  logic       clk,
    input  logic       rst_n,
    input  logic [7:0] din,
    output logic [7:0] core_to_top,
    output logic [7:0] core_to_io
);
    logic [7:0] state_q;
    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            state_q <= 8'h00;
        end else begin
            state_q <= din ^ 8'h5A;
        end
    end
    assign core_to_top = state_q;
    assign core_to_io  = state_q + 8'h01;
endmodule

module equiv_io (
    input  logic       clk,
    input  logic       rst_n,
    input  logic [7:0] in_from_core,
    input  logic [7:0] in_from_top,
    output logic [7:0] pad_out,
    output logic [3:0] io_to_top
);
    logic [7:0] io_q;
    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            io_q <= 8'h00;
        end else begin
            io_q <= in_from_core ^ in_from_top;
        end
    end
    assign pad_out   = io_q;
    assign io_to_top = io_q[3:0];
endmodule

module soc_equiv_top (
    input  logic       clk,
    input  logic       rst_n,
    input  logic       VDD_AON,
    input  logic       VDD_IO,
    input  logic       VSS,
    input  logic       pwr_en,
    output logic       pwr_ack,
    input  logic       iso_en,
    input  logic       ret_save,
    input  logic       ret_restore,
    input  logic [7:0] din,
    output logic [7:0] dout_top,
    output logic [7:0] pad_out
);
    logic [7:0] core_to_top_w;
    logic [7:0] core_to_io_w;
    logic [3:0] io_to_top_w;

    assign pwr_ack = pwr_en;

    equiv_core u_core (
        .clk         (clk),
        .rst_n       (rst_n),
        .din         (din),
        .core_to_top (core_to_top_w),
        .core_to_io  (core_to_io_w)
    );

    equiv_io u_io (
        .clk          (clk),
        .rst_n        (rst_n),
        .in_from_core (core_to_io_w),
        .in_from_top  (core_to_top_w),
        .pad_out      (pad_out),
        .io_to_top    (io_to_top_w)
    );

    assign dout_top = core_to_top_w ^ {4'h0, io_to_top_w};
endmodule
