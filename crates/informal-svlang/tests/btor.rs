//! The Btor2 the svlang front end writes: well formed (it parses back), and
//! the modelling choices of `svlang::btor2` show up as the lines they promise.

use informal_slang::Request;
use std::path::PathBuf;

fn lower(name: &str, src: &str) -> informal_slang::Lowered {
    let dir = std::env::temp_dir().join(format!("informal-svlang-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let f: PathBuf = dir.join(format!("{name}.sv"));
    std::fs::write(&f, src).unwrap();
    informal_svlang::lower(&Request {
        files: vec![f],
        top: Some(name.into()),
        ..Default::default()
    })
}

fn ok(name: &str, src: &str) -> String {
    let l = lower(name, src);
    assert!(l.ok, "{:?}", l.errors);
    informal_ir::parse(&l.btor2).unwrap_or_else(|e| panic!("{e}\n{}", l.btor2));
    l.btor2
}

fn lines<'a>(btor: &'a str, op: &str) -> Vec<&'a str> {
    btor.lines()
        .filter(|l| l.split_whitespace().nth(1) == Some(op))
        .collect()
}

#[test]
fn counter_with_async_reset_and_labelled_assert() {
    let b = ok(
        "cnt",
        "module cnt(input logic clk, rst_n, output logic [3:0] q);\n\
           always_ff @(posedge clk or negedge rst_n) if (!rst_n) q <= 4'd0; else q <= q + 4'd1;\n\
           always @(posedge clk) p_le: assert (q <= 4'd15);\n\
         endmodule\n",
    );
    assert_eq!(lines(&b, "state").len(), 1);
    assert!(lines(&b, "bad").iter().any(|l| l.ends_with(" p_le")));
    // The reset acts synchronously: the next state picks the reset value.
    assert!(!lines(&b, "ite").is_empty());
}

#[test]
fn anyconst_is_held_and_x_is_free() {
    let b = ok(
        "fr",
        "module fr(input logic clk, input logic [1:0] s);\n\
           (* anyconst *) wire [3:0] k;\n\
           logic [3:0] y;\n\
           always_comb case (s) 2'd0: y = k; default: y = 'x; endcase\n\
           always @(posedge clk) assert (y == k);\n\
         endmodule\n",
    );
    // `k` is a state whose next value is itself.
    let st = lines(&b, "state")
        .into_iter()
        .find(|l| l.ends_with(" fr.k"))
        .expect("k state");
    let id = st.split_whitespace().next().unwrap();
    assert!(lines(&b, "next").iter().any(|l| {
        let w: Vec<_> = l.split_whitespace().collect();
        w[3] == id && w[4] == id
    }));
    // The X default is a free input, not 0.
    assert!(lines(&b, "input").iter().any(|l| l.contains("informal_x_")));
}

#[test]
fn two_clocks_and_latches_are_rejected() {
    let l = lower(
        "two",
        "module two(input logic a, b, d, output logic p, q);\n\
           always_ff @(posedge a) p <= d;\n\
           always_ff @(posedge b) q <= d;\n\
         endmodule\n",
    );
    assert!(
        !l.ok && l.errors[0].contains("more than one clock"),
        "{:?}",
        l.errors
    );
    let l = lower(
        "lat",
        "module lat(input logic en, d, output logic q);\n\
           always_latch if (en) q <= d;\n\
         endmodule\n",
    );
    assert!(!l.ok && l.errors[0].contains("latch"), "{:?}", l.errors);
    assert!(!l.fallback);
}

#[test]
fn trace_section_names_combinational_nets() {
    let dir = std::env::temp_dir().join(format!("informal-svlang-tr-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("tr.sv");
    std::fs::write(
        &f,
        "module tr(input logic clk, input logic [3:0] a, output logic [3:0] q);\n\
           logic [3:0] n;\n\
           assign n = a + 4'd1;\n\
           always_ff @(posedge clk) q <= n;\n\
         endmodule\n",
    )
    .unwrap();
    let l = informal_svlang::lower(&Request {
        files: vec![f],
        trace_outputs: true,
        ..Default::default()
    });
    assert!(l.ok, "{:?}", l.errors);
    let (model, section) = informal_slang::split_trace(&l.btor2);
    informal_ir::parse(model).unwrap();
    assert!(section.lines().any(|x| x.ends_with(" tr.n")), "{section}");
}
