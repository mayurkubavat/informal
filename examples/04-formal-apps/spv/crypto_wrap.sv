// Formal Security Path Verification Lab (`check_spv`)
module crypto_wrap (
    input  logic       clk,
    input  logic       rst_n,
    input  logic [7:0] secret_key,
    input  logic [7:0] pub_data,
    input  logic       debug_unlock,
    output logic [7:0] pub_status_o,
    output logic [7:0] debug_port_o,
    output logic [7:0] covert_port_o
);
    logic s1, s2;

    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            pub_status_o <= 8'd0;
            s1           <= 1'b0;
            s2           <= 1'b0;
        end else begin
            pub_status_o <= pub_data + 8'd1;
            s1           <= secret_key[0];
            s2           <= s1; // 2-cycle covert storage channel!
        end
    end

    assign debug_port_o  = debug_unlock ? secret_key : pub_data;
    assign covert_port_o = pub_data ^ {7'd0, s2};
endmodule
