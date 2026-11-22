module LS_HL_X1 (
    input  wire A,
    output wire Y,
    input  wire VDDH,
    input  wire VDDL,
    input  wire VSS
);
    assign Y = A;
endmodule

module DFF_X1 (
    input  wire CK,
    input  wire D,
    output reg  Q,
    input  wire VDD,
    input  wire VSS
);
    initial begin
        Q = 1'b0;
    end
    always @(posedge CK) begin
        Q <= D;
    end
endmodule

module ISO_HI_X1 (
    input  wire A,
    input  wire ISO,
    output wire Y,
    input  wire VDD_BK,
    input  wire VSS
);
    assign Y = ISO ? 1'b1 : A;
endmodule

module SRAM_MACRO_BAD_PIN (
    input  wire       CK,
    input  wire       CE,
    input  wire [3:0] DIN,
    output reg  [3:0] DOUT,
    input  wire       VDD_PERI,
    input  wire       VDD_ARRAY,
    input  wire       VSS
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
