use fdv_ir::{parse, print, Features, Op};
use std::path::PathBuf;

fn models_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/models")
}

#[test]
fn every_sample_model_round_trips() {
    let mut seen = 0;
    for entry in std::fs::read_dir(models_dir()).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("btor2") {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        let m = parse(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let printed = print(&m);
        let again = parse(&printed).unwrap();
        assert_eq!(m, again, "{} did not round-trip", path.display());
        assert_eq!(
            printed,
            print(&again),
            "printing is not stable for {}",
            path.display()
        );
        seen += 1;
    }
    assert!(seen >= 3, "expected sample models, found {seen}");
}

#[test]
fn prints_canonical_text_without_comments() {
    let m = parse("; c\n1 sort bitvec 8   ; w\n2 input 1 x\n3 constd 1 -5\n4 slice 1 2 7 0\n5 add 1 -2 3 sum\n").unwrap();
    assert_eq!(
        print(&m),
        "1 sort bitvec 8\n2 input 1 x\n3 constd 1 -5\n4 slice 1 2 7 0\n5 add 1 -2 3 sum\n"
    );
}

#[test]
fn reads_properties_and_symbols() {
    let m = parse(&std::fs::read_to_string(models_dir().join("liveness.btor2")).unwrap()).unwrap();
    let j: Vec<_> = m.with_op(Op::Justice).collect();
    assert_eq!(j.len(), 1);
    assert_eq!(j[0].args, vec![3]);
    assert_eq!(j[0].symbol.as_deref(), Some("eventually_grant"));
}

#[test]
fn rejects_bad_input() {
    let cases = [
        ("1 sort bitvec 0\n", "width"),
        ("1 sort bitvec 8\n1 input 1\n", "not greater"),
        ("1 sort bitvec 8\n2 input 7\n", "not a declared sort"),
        ("1 sort bitvec 8\n2 add 1 3 4\n", "earlier node"),
        ("1 sort bitvec 8\n2 frob 1\n", "unknown operator"),
        ("1 sort bitvec 8\n2 const 1 102\n", "bad literal"),
        ("1 sort bitvec 8\n2 input 1 x y\n", "trailing"),
        ("1 sort bitvec 8\n2 input 1\n3 not 1 -1\n", "earlier node"),
    ];
    for (text, needle) in cases {
        let e = parse(text).expect_err(text);
        assert!(
            e.msg.contains(needle),
            "{text:?}: got '{}', wanted '{needle}'",
            e.msg
        );
    }
}

#[test]
fn features_describe_the_model() {
    let m = parse(&std::fs::read_to_string(models_dir().join("memory.btor2")).unwrap()).unwrap();
    let f = Features::of(&m);
    assert!(f.has_arrays);
    assert_eq!((f.inputs, f.states, f.array_states, f.bad), (2, 1, 1, 1));
    assert_eq!(f.max_width, 8);

    let c = Features::of(
        &parse(&std::fs::read_to_string(models_dir().join("counter.btor2")).unwrap()).unwrap(),
    );
    assert_eq!((c.state_bits, c.arith_ops, c.has_arrays), (3, 1, false));
}
