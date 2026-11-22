use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;

use informal_svlang::{compile_cached, DriverOrigin, NetSegKind, Request};

struct TempDir(PathBuf);
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn write_temp_sv(tag: &str, src: &str) -> (TempDir, PathBuf) {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "informal_sg_build_{}_{n}_{tag}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("mkdir");
    let path = dir.join(format!("{tag}.sv"));
    std::fs::write(&path, src).expect("write sv");
    (TempDir(dir), path)
}

#[test]
fn struct_graph_once_lock_thread_safety_and_cache_invalidation() {
    let sv = r#"
module leaf(input logic [7:0] a, output logic [7:0] y);
  assign y = a;
endmodule
module top(input logic [7:0] in_bus, output logic [7:0] out_bus);
  leaf u_leaf(.a(in_bus), .y(out_bus));
endmodule
"#;
    let (_dir, path) = write_temp_sv("oncelock", sv);
    let req = Request {
        files: vec![path.clone()],
        top: Some("top".to_string()),
        ..Default::default()
    };
    let compiled = compile_cached(None, &req).expect("compile");
    let mut handles = Vec::new();
    for _ in 0..4 {
        let c = Arc::clone(&compiled);
        handles.push(thread::spawn(move || {
            let g = c.struct_graph();
            (g.cells.len(), g.nets.len(), g.pins.len())
        }));
    }
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert!(results.windows(2).all(|w| w[0] == w[1]));
    assert_eq!(results[0].0, 2);

    // Edit file on disk -> Tier-1 invalidation builds a fresh StructuralGraph.
    let sv2 = r#"
module leaf(input logic [7:0] a, output logic [7:0] y);
  assign y = a;
endmodule
module top(input logic [7:0] in_bus, output logic [7:0] out_bus);
  logic [7:0] mid;
  leaf u_l1(.a(in_bus), .y(mid));
  leaf u_l2(.a(mid), .y(out_bus));
endmodule
"#;
    std::fs::write(&path, sv2).expect("overwrite sv");
    let compiled2 = compile_cached(Some(&compiled), &req).expect("recompile");
    assert!(!Arc::ptr_eq(&compiled, &compiled2));
    assert_eq!(compiled2.struct_graph().cells.len(), 3);
}

#[test]
fn preserves_hierarchy_and_all_port_conn_forms() {
    let sv = r#"
module leaf(input logic clk, input logic a, input logic b, output logic y);
  assign y = a & b;
endmodule
module mid(input logic clk, input logic a, input logic b, output logic y);
  leaf u_named(.clk(clk), .a(a), .b(b), .y(y));
  leaf u_pos(clk, a, b, );
  leaf u_impl(.clk, .a, .b, .y());
  leaf u_wild(.*);
endmodule
module top(input logic clk, input logic a, input logic b, output logic y);
  mid u_mid(.*);
endmodule
"#;
    let (_dir, path) = write_temp_sv("conns", sv);
    let req = Request {
        files: vec![path],
        top: Some("top".to_string()),
        ..Default::default()
    };
    let compiled = compile_cached(None, &req).expect("compile");
    let g = compiled.struct_graph();
    // top + u_mid + 4 leaf instances = 6 cells.
    assert_eq!(g.cells.len(), 6);
    assert!(g.lookup_cell_by_upf("u_mid").is_some());
    assert!(g.lookup_cell_by_upf("u_mid/u_named").is_some());
    assert!(g.lookup_cell_by_upf("u_mid/u_pos").is_some());
    assert!(g.lookup_cell_by_upf("u_mid/u_impl").is_some());
    assert!(g.lookup_cell_by_upf("u_mid/u_wild").is_some());

    let y_pin = g.lookup_pin_by_upf("u_mid/u_impl/y").expect("u_impl/y pin");
    assert!(g.pin(y_pin).unconnected);
    let a_pin = g.lookup_pin_by_upf("u_mid/u_wild/a").expect("u_wild/a pin");
    assert!(!g.pin(a_pin).external_conns.is_empty());
}

#[test]
fn bit_slices_concats_ascending_ranges_and_structs() {
    let sv = r#"
typedef struct packed {
  logic [3:0] tag;
  logic       valid;
} pkt_t;
module sink(input logic [7:0] d, input logic [0:3] asc, output logic o);
  assign o = d[7] ^ asc[0];
endmodule
module top(input logic [3:0] hi, input logic [3:0] lo, input pkt_t pkt, output logic o);
  logic [0:7] asc_bus;
  assign asc_bus = {hi, lo};
  sink u_sink(.d({hi, pkt.tag}), .asc(asc_bus[0:3]), .o(o));
endmodule
"#;
    let (_dir, path) = write_temp_sv("slices", sv);
    let req = Request {
        files: vec![path],
        top: Some("top".to_string()),
        ..Default::default()
    };
    let compiled = compile_cached(None, &req).expect("compile");
    let g = compiled.struct_graph();
    let d_pin = g.lookup_pin_by_upf("u_sink/d").expect("d pin");
    let conns = &g.pin(d_pin).external_conns;
    assert_eq!(conns.len(), 2);
    // MSB half `hi` -> pin_range [4..=7]; LSB half `pkt.tag` (packed_slice lo=1, w=4) -> [0..=3].
    assert_eq!((conns[0].pin_range.lo, conns[0].pin_range.hi), (4, 7));
    assert_eq!((conns[1].pin_range.lo, conns[1].pin_range.hi), (0, 3));
    assert_eq!((conns[1].parent_range.lo, conns[1].parent_range.hi), (1, 4));

    let asc_pin = g.lookup_pin_by_upf("u_sink/asc").expect("asc pin");
    // In ascending [0:7], [0:3] is the MSB half -> physical bit offsets [4..=7]!
    assert_eq!(
        (
            g.pin(asc_pin).external_conns[0].parent_range.lo,
            g.pin(asc_pin).external_conns[0].parent_range.hi
        ),
        (4, 7)
    );
}

#[test]
fn escaped_identifiers_and_blackbox_stubs() {
    let sv = r#"
module top(input logic \bus[0] , output logic y);
  logic \wire$esc ;
  assign \wire$esc  = \bus[0] ;
  prim_bb \u_blk$1 (.in(\wire$esc ), .out(y));
endmodule
"#;
    let (_dir, path) = write_temp_sv("escaped_bb", sv);
    let req = Request {
        files: vec![path],
        top: Some("top".to_string()),
        black_box_modules: vec!["prim_bb".to_string()],
        ..Default::default()
    };
    let compiled = compile_cached(None, &req).expect("compile");
    let g = compiled.struct_graph();
    let bb_cell = g
        .lookup_cell_by_upf("u_blk$1")
        .expect("escaped blackbox cell");
    assert!(g.cell(bb_cell).is_blackbox);
    assert!(g.lookup_pin_by_upf("u_blk$1/in").is_some());
    assert!(g.lookup_pin_by_upf("u_blk$1/out").is_some());
    assert!(g.lookup_net_by_upf("\\bus [0]").is_some());
    assert!(g.lookup_net_by_upf("wire$esc").is_some());
}

#[test]
fn procedural_flops_latches_and_nonsynthesizable_resilience() {
    let sv = r#"
module top(
  input  logic clk,
  input  logic rst_n,
  input  logic en,
  input  logic d,
  inout  wire  io_pad,
  output logic q_ff,
  output logic q_lat,
  output logic g_out
);
  supply0 gnd_net;
  supply1 vdd_net;
  real r_debug;
  always_ff @(posedge clk or negedge rst_n) begin
    if (!rst_n) q_ff <= 1'b0;
    else q_ff <= d;
  end
  always_latch begin
    if (en) q_lat = d;
  end
  and u_and(g_out, d, vdd_net);
  initial begin
    r_debug = 1.5;
    $display("hi");
  end
endmodule
"#;
    let (_dir, path) = write_temp_sv("seq_nonsynth", sv);
    let req = Request {
        files: vec![path],
        top: Some("top".to_string()),
        ..Default::default()
    };
    let compiled = compile_cached(None, &req).expect("compile");
    let g = compiled.struct_graph();
    let top_cell = g.cell(g.tops[0]);
    assert_eq!(top_cell.flops.len(), 1);
    assert_eq!(top_cell.latches.len(), 1);
    assert!(top_cell.flops[0].async_reset.is_some());
    assert_eq!(
        top_cell.flops[0].async_reset_range,
        Some(informal_svlang::struct_graph::BitRange { lo: 0, hi: 0 })
    );
    assert_eq!(top_cell.flops[0].async_reset_active_high, Some(false));

    let gnd = g.lookup_net_by_upf("gnd_net").expect("supply0");
    assert_eq!(g.net(gnd).kind, NetSegKind::Supply0);
    let gout = g.lookup_net_by_upf("g_out").expect("g_out");
    assert!(g
        .net(gout)
        .drivers
        .iter()
        .any(|d| matches!(d.origin, DriverOrigin::Gate(ref k) if k == "and")));
}

#[test]
fn blackbox_stub_infers_width_and_outgoing_crossings() {
    let sv = r#"
module dst(input logic [7:0] d, output logic y);
  assign y = ^d;
endmodule
module top(input logic [7:0] a, output logic y);
  logic [7:0] w_bb;
  bb_ip u_bb(.in_bus(a), .out_bus(w_bb));
  dst u_dst(.d(w_bb), .y(y));
endmodule
"#;
    let (_dir, path) = write_temp_sv("bb_out", sv);
    let req = Request {
        files: vec![path],
        top: Some("top".to_string()),
        black_box_modules: vec!["bb_ip".to_string()],
        ..Default::default()
    };
    let compiled = compile_cached(None, &req).expect("compile");
    let g = compiled.struct_graph();
    let u_top = g.tops[0];
    let u_bb = g.lookup_cell_by_upf("u_bb").expect("u_bb");
    let u_dst = g.lookup_cell_by_upf("u_dst").expect("u_dst");

    let out_pin = g.lookup_pin_by_upf("u_bb/out_bus").expect("out_bus");
    assert_eq!(g.pin(out_pin).width, 8);

    let mut dom = std::collections::HashMap::new();
    dom.insert(u_top, 0u32);
    dom.insert(u_bb, 1u32);
    dom.insert(u_dst, 2u32);
    let edges = g.extract_crossings(&|c| dom.get(&c).copied());
    let bb_to_dst: Vec<_> = edges
        .iter()
        .filter(|e| e.src_cell == u_bb && e.dst_cell == u_dst)
        .collect();
    assert_eq!(bb_to_dst.len(), 1, "{edges:#?}");
    assert_eq!(bb_to_dst[0].src_range.width(), 8);
    // Input-only `in_bus` driven by `a` must NOT produce a false `u_bb -> u_dst` crossing.
    assert!(edges
        .iter()
        .all(|e| !(e.src_cell == u_bb && g.net(e.src_net).name == "in_bus")));
}

#[test]
fn multibit_async_reset_slice_records_exact_bit_range() {
    let sv = r#"
module top(input logic clk, input logic [3:0] rst_bus, input logic d, output logic q1, output logic q2);
  always_ff @(posedge clk or negedge rst_bus[2]) begin
    if (!rst_bus[2]) q1 <= 1'b0;
    else q1 <= d;
  end
  always_ff @(posedge clk or posedge rst_bus[0]) begin
    if (rst_bus[0]) q2 <= 1'b0;
    else q2 <= d;
  end
endmodule
"#;
    let (_dir, path) = write_temp_sv("multibit_rst", sv);
    let req = Request {
        files: vec![path],
        top: Some("top".to_string()),
        ..Default::default()
    };
    let compiled = compile_cached(None, &req).expect("compile");
    let g = compiled.struct_graph();
    let top_cell = g.cell(g.tops[0]);
    assert_eq!(top_cell.flops.len(), 2);
    let f1 = top_cell
        .flops
        .iter()
        .find(|f| f.upf_path == "q1")
        .expect("q1");
    let f2 = top_cell
        .flops
        .iter()
        .find(|f| f.upf_path == "q2")
        .expect("q2");
    assert_eq!(
        f1.async_reset_range,
        Some(informal_svlang::struct_graph::BitRange { lo: 2, hi: 2 })
    );
    assert_eq!(f1.async_reset_active_high, Some(false));
    assert_eq!(
        f2.async_reset_range,
        Some(informal_svlang::struct_graph::BitRange { lo: 0, hi: 0 })
    );
    assert_eq!(f2.async_reset_active_high, Some(true));
}
