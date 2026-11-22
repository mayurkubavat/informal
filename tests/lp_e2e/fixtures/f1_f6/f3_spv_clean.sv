module f3_spv_clean (
    input  logic       clk,
    input  logic       rst_n,
    input  logic [7:0] secret_key,
    input  logic [7:0] pub_data,
    output logic [7:0] pub_out,
    output logic [7:0] key_shadow
);
    logic [7:0] key_reg;

    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            pub_out <= 8'h00;
            key_reg <= 8'h00;
        end else begin
            pub_out <= pub_data ^ 8'h3C;
            key_reg <= secret_key;
        end
    end

    assign key_shadow = key_reg;
endmodule
