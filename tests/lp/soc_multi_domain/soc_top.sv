module pmu(
  input  logic clk,
  input  logic rst_n,
  input  logic sleep_req,
  output logic iso_en,
  output logic save_en,
  output logic rest_en,
  output logic psw_en
);
  always_ff @(posedge clk or negedge rst_n) begin
    if (!rst_n) begin
      iso_en  <= 1'b0;
      save_en <= 1'b0;
      rest_en <= 1'b0;
      psw_en  <= 1'b1;
    end else begin
      iso_en  <= sleep_req;
      save_en <= sleep_req;
      rest_en <= !sleep_req;
      psw_en  <= !sleep_req;
    end
  end
endmodule

module lane(input logic [7:0] din, output logic [7:0] dout);
  assign dout = din;
endmodule

module core_blk(
  input  logic        clk,
  input  logic        rst_n,
  input  logic        io_in,
  input  logic [63:0] d_in,
  output logic [63:0] bus_data,
  output logic        bus_valid,
  output logic        core_to_io,
  output logic [7:0]  \bus_esc$0
);
  logic [7:0] lane_out;
  lane u_lane_0(.din(d_in[7:0]), .dout(lane_out));
  always_ff @(posedge clk or negedge rst_n) begin
    if (!rst_n) begin
      bus_data   <= 64'd0;
      bus_valid  <= 1'b0;
      core_to_io <= 1'b0;
      \bus_esc$0 <= 8'd0;
    end else begin
      bus_data   <= d_in ^ {56'd0, lane_out};
      bus_valid  <= io_in;
      core_to_io <= io_in;
      \bus_esc$0 <= lane_out;
    end
  end
endmodule

module sram_ret_blk(
  input  logic        clk,
  input  logic        rst_n,
  input  logic        valid_in,
  input  logic [63:0] data_in,
  input  logic [7:0]  esc_in,
  output logic [63:0] data_q
);
  always_ff @(posedge clk or negedge rst_n) begin
    if (!rst_n) begin
      data_q <= 64'd0;
    end else if (valid_in) begin
      data_q <= data_in ^ {56'd0, esc_in};
    end
  end
endmodule

module io_blk(
  input  logic pad_in,
  input  logic core_to_io,
  output logic io_to_core,
  output logic pad_out
);
  assign io_to_core = pad_in;
  assign pad_out    = core_to_io;
endmodule

module soc_top(
  input  logic        clk,
  input  logic        rst_n,
  input  logic        sleep_req,
  input  logic        pad_in,
  input  logic [63:0] d_in,
  output logic [63:0] sram_q,
  output logic        pad_out
);
  logic iso_en, save_en, rest_en, psw_en;
  logic io_to_core, core_to_io;
  logic [63:0] bus_data, bus_data_iso;
  logic        bus_valid, bus_valid_iso;
  logic [7:0]  esc_bus, esc_bus_iso;

  pmu u_pmu(
    .clk(clk), .rst_n(rst_n), .sleep_req(sleep_req),
    .iso_en(iso_en), .save_en(save_en), .rest_en(rest_en), .psw_en(psw_en)
  );

  io_blk u_io(
    .pad_in(pad_in), .core_to_io(core_to_io),
    .io_to_core(io_to_core), .pad_out(pad_out)
  );

  core_blk u_core(
    .clk(clk), .rst_n(rst_n), .io_in(io_to_core), .d_in(d_in),
    .bus_data(bus_data), .bus_valid(bus_valid), .core_to_io(core_to_io),
    .\bus_esc$0 (esc_bus)
  );

  assign bus_data_iso  = (!iso_en) ? bus_data  : 64'd0;
  assign bus_valid_iso = (!iso_en) & bus_valid;
  assign esc_bus_iso   = (!iso_en) ? esc_bus   : 8'd0;

  sram_ret_blk u_sram(
    .clk(clk), .rst_n(rst_n),
    .valid_in(bus_valid_iso),
    .data_in({
      bus_data_iso[63:56], bus_data_iso[55:48], bus_data_iso[47:40], bus_data_iso[39:32],
      bus_data_iso[31:24], bus_data_iso[23:16], bus_data_iso[15:8],  bus_data_iso[7:0]
    }),
    .esc_in(esc_bus_iso),
    .data_q(sram_q)
  );
endmodule
