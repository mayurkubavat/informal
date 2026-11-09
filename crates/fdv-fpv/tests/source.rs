//! Properties remember the script line that created them (`Property.source`),
//! so the app can open the script at that line. No engines are loaded: the
//! prove at the end of the script may fail, the properties are made before it.

use fdv_fpv::{Flavor, Session, Shell, SourceLoc};
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    std::fs::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")).unwrap()
}

fn sourced(flavor: Flavor, script: &str, tag: &str) -> Vec<(String, Option<SourceLoc>)> {
    let work = std::env::temp_dir().join(format!("fdv-fpv-source-{tag}-{}", std::process::id()));
    let sh = Shell::new(Session::new(Vec::new(), work), flavor);
    let _ = sh.source(&root().join(script));
    let s = sh.session.borrow();
    s.props
        .iter()
        .map(|p| (p.name.clone(), p.source.clone()))
        .collect()
}

#[test]
fn jaspergold_script_lines() {
    let file = root()
        .join("tests/fpv/counter/counter.jg.tcl")
        .to_string_lossy()
        .into_owned();
    let got = sourced(Flavor::Jg, "tests/fpv/counter/counter.jg.tcl", "jg");
    let want: Vec<_> = [
        ("a_max", 9),
        ("a_step", 10),
        ("a_hold", 11),
        ("a_never9", 12),
        ("a_no_wrap", 13),
        ("c_done", 14),
        ("c_10", 15),
        ("m_en_burst", 16),
    ]
    .iter()
    .map(|(n, l)| {
        (
            n.to_string(),
            Some(SourceLoc {
                file: file.clone(),
                line: *l,
            }),
        )
    })
    .collect();
    assert_eq!(got, want);
}

#[test]
fn console_properties_have_no_source() {
    let sh = Shell::new(
        Session::new(Vec::new(), std::env::temp_dir().join("fdv-src-console")),
        Flavor::Jg,
    );
    sh.eval("assert -name a {x |=> y}").unwrap();
    assert_eq!(sh.session.borrow().props[0].source, None);
}

#[test]
fn eval_inside_a_script_falls_back_to_a_name_search() {
    let dir = std::env::temp_dir().join(format!("fdv-src-eval-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("s.tcl");
    // `eval` of a built string has no line info; the name search finds it.
    std::fs::write(
        &f,
        "set n a_x\n\n# a_x2 is not a_x\neval \"assert -name $n {p}\"\n# a_x\n",
    )
    .unwrap();
    let sh = Shell::new(Session::new(Vec::new(), dir.join("work")), Flavor::Jg);
    sh.source(&f).unwrap();
    let src = sh.session.borrow().props[0].source.clone().unwrap();
    assert_eq!(src.line, 1);
    let _ = std::fs::remove_dir_all(&dir);
}
