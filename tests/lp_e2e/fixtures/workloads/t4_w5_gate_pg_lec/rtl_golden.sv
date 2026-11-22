module rtl_ret_dff (
    input  wire CK,
    input  wire D,
    input  wire SAVE,
    input  wire RESTORE,
    output reg  Q
);
    reg q_ret;
    initial begin
        Q     = 1'b0;
        q_ret = 1'b0;
    end
    always @(posedge CK) begin
        if (SAVE) begin
            q_ret <= Q;
        end
        Q <= RESTORE ? q_ret : D;
    end
endmodule

module rtl_sram_macro (
    input  wire       CK,
    input  wire       CE,
    input  wire [3:0] DIN,
    output reg  [3:0] DOUT
);
    initial begin
        DOUT = 4'b0000;
    end
    always @(posedge CK) begin
        if (CE) begin
            DOUT <= DIN;
        end
    end
endmodule

module rtl_core_pg (
    input  wire       clk,
    input  wire       rst_n,
    input  wire       din,
    input  wire       ret_save,
    input  wire       ret_restore,
    output wire       core_o,
    output wire [3:0] macro_dout
);
    wire d_int;
    assign d_int = din & rst_n;

    rtl_ret_dff u_ret (
        .CK      (clk),
        .D       (d_int),
        .SAVE    (ret_save),
        .RESTORE (ret_restore),
        .Q       (core_o)
    );

    rtl_sram_macro u_sram (
        .CK   (clk),
        .CE   (ret_save),
        .DIN  ({4{din}}),
        .DOUT (macro_dout)
    );
endmodule

module rtl_high_pg (
    input  wire a,
    output wire y
);
    assign y = a;
endmodule

module soc_pg_top (
    input  wire       clk,
    input  wire       rst_n,
    input  wire       VDD_AON,
    input  wire       VDD_HIGH,
    input  wire       VDD_BK,
    input  wire       VSS,
    input  wire       sleep_core,
    input  wire       iso_en,
    input  wire       ret_save,
    input  wire       ret_restore,
    input  wire       din,
    output wire       dout,
    output wire [3:0] macro_dout
);
    wire core_sig;
    wire iso_sig;

    rtl_core_pg u_core (
        .clk         (clk),
        .rst_n       (rst_n),
        .din         (din),
        .ret_save    (ret_save),
        .ret_restore (ret_restore),
        .core_o      (core_sig),
        .macro_dout  (macro_dout)
    );

    assign iso_sig = iso_en ? 1'b0 : core_sig;

    rtl_high_pg u_high (
        .a (iso_sig),
        .y (dout)
    );
endmodule
