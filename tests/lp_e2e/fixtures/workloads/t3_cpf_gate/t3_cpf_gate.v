module cpf_core_blk (
    input  wire       clk,
    input  wire       VDD_CORE_SW,
    input  wire       VDD_BK,
    input  wire       VSS,
    input  wire       din,
    input  wire       ce,
    input  wire       ret_save,
    input  wire       ret_restore,
    output wire       core_sig,
    output wire [3:0] macro_dout
);
    wire n0;
    RET_DFF_X1 u_ret (
        .CK(clk), .D(din), .SAVE(ret_save), .RESTORE(ret_restore),
        .Q(n0), .VDD(VDD_CORE_SW), .VDD_BK(VDD_BK), .VSS(VSS)
    );
    BUF_X1 u_buf (.A(n0), .Y(core_sig), .VDD(VDD_CORE_SW), .VSS(VSS));
    SRAM_MACRO_2R u_sram (
        .CK(clk), .CE(ce), .DIN({4{din}}), .DOUT(macro_dout),
        .VDD_PERI(VDD_CORE_SW), .VDD_ARRAY(VDD_BK), .VSS(VSS)
    );
endmodule

module cpf_high_blk (
    input  wire VDD_HIGH,
    input  wire VSS,
    input  wire a,
    output wire y
);
    BUF_X1 u_hbuf (.A(a), .Y(y), .VDD(VDD_HIGH), .VSS(VSS));
endmodule

module soc_cpf_top (
    input  wire       clk,
    input  wire       VDD_AON,
    input  wire       VDD_HIGH,
    input  wire       VDD_BK,
    input  wire       VSS,
    input  wire       sleep_core,
    output wire       sleep_ack,
    input  wire       iso_en,
    input  wire       ret_save,
    input  wire       ret_restore,
    input  wire       ce,
    input  wire       din,
    output wire       dout,
    output wire [3:0] macro_dout
);
    wire VDD_CORE_SW;
    wire aon_iso;
    wire core_sig;
    wire iso_sig;
    wire ls_sig;
    wire [3:0] macro_raw;

    PSW_HDR_X1 u_psw_core (
        .SLEEP(sleep_core), .SLEEPOUT(sleep_ack),
        .VDD_IN(VDD_AON), .VDD_OUT(VDD_CORE_SW), .VSS(VSS)
    );
    AON_BUF_X1 u_aon_iso (.A(iso_en), .Y(aon_iso), .VDD_BK(VDD_BK), .VSS(VSS));

    cpf_core_blk u_core (
        .clk(clk), .VDD_CORE_SW(VDD_CORE_SW), .VDD_BK(VDD_BK), .VSS(VSS),
        .din(din), .ce(ce), .ret_save(ret_save), .ret_restore(ret_restore),
        .core_sig(core_sig), .macro_dout(macro_raw)
    );

    ISO_LO_X1 u_iso (.A(core_sig), .ISO(aon_iso), .Y(iso_sig), .VDD_BK(VDD_BK), .VSS(VSS));
    LS_LH_X1  u_ls  (.A(iso_sig), .Y(ls_sig), .VDDL(VDD_AON), .VDDH(VDD_HIGH), .VSS(VSS));
    cpf_high_blk u_high (.VDD_HIGH(VDD_HIGH), .VSS(VSS), .a(ls_sig), .y(dout));

    assign macro_dout = macro_raw;
endmodule
