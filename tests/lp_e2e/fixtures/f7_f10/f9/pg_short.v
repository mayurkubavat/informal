module pg_short (
    input  wire VDD_AON,
    input  wire VDD_HIGH,
    input  wire VSS,
    input  wire din,
    output wire dout_a,
    output wire dout_b
);
    // Incompatible power/ground rail short: VDD and VSS tied to the same net VDD_AON
    BUF_X1 u_short_pwr_gnd (
        .A   (din),
        .Y   (dout_a),
        .VDD (VDD_AON),
        .VSS (VDD_AON)
    );

    // Incompatible supply domains shorted via wire alias
    wire short_rail;
    assign short_rail = VDD_AON & VDD_HIGH;
    BUF_X1 u_short_rails (
        .A   (din),
        .Y   (dout_b),
        .VDD (short_rail),
        .VSS (VSS)
    );
endmodule
