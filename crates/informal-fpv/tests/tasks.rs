//! JasperGold tasks (decision D20): each task holds its own properties,
//! assumptions and cut points, and is proved on its own model. The command
//! tests need no engine; the prove tests use the in-house engines and are
//! skipped without them (CaDiCaL).

use informal_fpv::{Flavor, Session, Shell};
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn counter(engines: Vec<std::sync::Arc<dyn informal_engine::Engine>>, tag: &str) -> Shell {
    let work = std::env::temp_dir().join(format!("informal-tasks-{tag}-{}", std::process::id()));
    let sh = Shell::new(Session::new(engines, work), Flavor::Jg);
    let path = root().join("tests/fpv/counter/counter.sv");
    sh.eval(&format!("analyze -sv12 {{{}}}", path.display()))
        .unwrap();
    sh.eval(
        "elaborate -top counter; clock clk; reset -expression {!rst_n}; set_prove_time_limit 60s",
    )
    .unwrap();
    sh
}

fn engines() -> Option<Vec<std::sync::Arc<dyn informal_engine::Engine>>> {
    let e = informal_bmc::engines();
    if e.is_empty() {
        informal_engine::tool_missing("needs the in-house engines (CaDiCaL)");
        return None;
    }
    Some(e)
}

fn status(sh: &Shell, name: &str) -> String {
    sh.eval(&format!("get_status {{{name}}}")).unwrap()
}

#[test]
fn create_list_set_and_delete() {
    let sh = counter(Vec::new(), "cmds");
    assert_eq!(sh.eval("task").unwrap(), "<embedded>");
    assert_eq!(sh.eval("task -list").unwrap(), "<embedded>");
    sh.eval("assert -name a_max {q <= 4'd9}").unwrap();
    sh.eval("assume -name m_en {en}").unwrap();
    assert_eq!(sh.eval("task -create t1").unwrap(), "t1");
    assert_eq!(sh.eval("get_task_list").unwrap(), "<embedded> t1");
    // Still in the embedded task until -set.
    assert_eq!(sh.eval("task").unwrap(), "<embedded>");
    sh.eval("task -set t1").unwrap();
    assert_eq!(sh.eval("task").unwrap(), "t1");
    // A property added now is in t1 and named with it.
    assert_eq!(sh.eval("assert -name a_x {q != 4'd3}").unwrap(), "t1::a_x");
    assert_eq!(sh.eval("assert {q != 4'd4}").unwrap(), "t1::assert_1");
    // -task picks the task whatever the current one is.
    assert_eq!(
        sh.eval("assert -task <embedded> -name a_y {q != 4'd5}")
            .unwrap(),
        "a_y"
    );
    assert_eq!(
        sh.eval("get_property_list -task t1").unwrap(),
        "t1::a_x t1::assert_1"
    );
    assert_eq!(
        sh.eval("get_property_list -task <embedded>").unwrap(),
        "a_max m_en a_y"
    );
    // A short name is looked up in the current task.
    assert_eq!(status(&sh, "a_x"), "unprocessed");
    // An embedded property also answers to its full name.
    assert_eq!(status(&sh, "<embedded>::a_max"), "unprocessed");
    sh.eval("assert -disable a_x").unwrap();
    assert_eq!(
        sh.eval("get_property_list -task t1 -include {disabled 0}")
            .unwrap(),
        "t1::assert_1"
    );
    // Errors: an unknown task, a duplicate, deleting the embedded task.
    assert!(sh.eval("task -set nope").is_err());
    assert!(sh.eval("assert -task nope {q != 0}").is_err());
    assert!(sh.eval("task -create t1").is_err());
    assert!(sh.eval("task -delete <embedded>").is_err());
    assert!(sh.eval("task -create {a b}").is_err());
    // Deleting the current task takes its properties and returns to the
    // embedded task.
    sh.eval("task -delete t1").unwrap();
    assert_eq!(sh.eval("task").unwrap(), "<embedded>");
    assert_eq!(sh.eval("task -list").unwrap(), "<embedded>");
    assert_eq!(sh.eval("get_property_list").unwrap(), "a_max m_en a_y");
}

#[test]
fn copies_and_cut_points() {
    let sh = counter(Vec::new(), "copy");
    sh.eval("assert -name a_max {q <= 4'd9}; assert -name a_step {en |=> 1}; cover -name c_done {done}; assume -name m_en {en}")
        .unwrap();
    sh.eval("stopat q").unwrap();
    // Copy by glob and by kind, from the embedded task (the current one).
    sh.eval("task -create t -copy a_m* -copy_assumes -copy_stopats")
        .unwrap();
    assert_eq!(
        sh.eval("get_property_list -task t").unwrap(),
        "t::a_max t::m_en"
    );
    assert_eq!(sh.eval("stopat -list -task t").unwrap(), "q");
    // Cut points are the task's own.
    sh.eval("stopat -task t -clear").unwrap();
    assert_eq!(sh.eval("stopat -list -task t").unwrap(), "");
    assert_eq!(sh.eval("stopat -list").unwrap(), "q");
    // -edit adds to an existing task; a copy already there stays one.
    sh.eval("task -edit t -copy_covers -copy a_max").unwrap();
    assert_eq!(
        sh.eval("get_property_list -task t").unwrap(),
        "t::a_max t::m_en t::c_done"
    );
    // -regexp, and -copy_asserts from another source task.
    sh.eval("task -create u -source_task t -copy_asserts")
        .unwrap();
    assert_eq!(sh.eval("get_property_list -task u").unwrap(), "u::a_max");
    sh.eval("task -edit u -copy {^a_s} -regexp").unwrap();
    assert_eq!(
        sh.eval("get_property_list -task u").unwrap(),
        "u::a_max u::a_step"
    );
    // The session JSON says which task each property is in.
    let json: serde_json::Value = serde_json::from_str(&sh.eval("session").unwrap()).unwrap();
    assert_eq!(json["current_task"], "<embedded>");
    assert_eq!(json["tasks"][1]["name"], "t");
    let p = json["props"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "u::a_step")
        .unwrap();
    assert_eq!(p["task"], "u");
}

/// Each task is proved on its own model: an assumption of one task does
/// not constrain another, and neither does a cut point.
#[test]
fn each_task_has_its_own_assumptions_and_cut_points() {
    let Some(engines) = engines() else {
        return;
    };
    let sh = counter(engines, "prove");
    // With en held low the counter stays at 0.
    sh.eval("assert -name a_never9 {q != 4'd9}; assert -name a_max {q <= 4'd9}; assume -name m_off {!en}")
        .unwrap();
    sh.eval("task -create free -copy a_*").unwrap();
    sh.eval("task -create cut -copy a_max -copy_assumes; stopat -task cut q")
        .unwrap();
    // The embedded task only.
    sh.eval("prove -all").unwrap();
    assert_eq!(status(&sh, "a_never9"), "proven");
    assert_eq!(status(&sh, "a_max"), "proven");
    assert_eq!(status(&sh, "free::a_never9"), "unprocessed");
    // Without the assumption the counter reaches 9.
    sh.eval("prove -task free").unwrap();
    assert_eq!(status(&sh, "free::a_never9"), "cex");
    assert_eq!(status(&sh, "free::a_max"), "proven");
    // With q cut, any value is possible, the assumption notwithstanding.
    sh.eval("prove -task cut").unwrap();
    assert_eq!(status(&sh, "cut::a_max"), "cex");
    // The embedded results stand.
    assert_eq!(status(&sh, "a_never9"), "proven");
    let report = sh.eval("report -task free").unwrap();
    assert!(report.contains("free::a_never9"), "{report}");
    assert!(!report.contains("cut::a_max"), "{report}");
}

/// The pattern of the vendor scripts in docs/vendor-script-tally.md
/// (OpenTitan's FPV flow and its after-load files): a task made from the
/// embedded one by name, with its assumptions, set current and proved; a
/// proc that moves an assertion to a task with its own cut point and
/// disables it in the embedded task by its full name.
#[test]
fn vendor_script_pattern() {
    let Some(engines) = engines() else {
        return;
    };
    let sh = counter(engines, "vendor");
    sh.eval(
        r#"
assert -name a_never9_A {q != 4'd9}
assert -name a_max_A    {q <= 4'd9}
assume -name m_off      {!en}

proc move_to_task {task_name assert_list} {
  foreach assert_name $assert_list {
    task -edit ${task_name} -copy "${assert_name}.*" -regexp
    assert -disable -regexp "\<embedded\>\::${assert_name}"
  }
}
task -create FreeQ
move_to_task FreeQ {a_max_A}
stopat -task FreeQ q

set TASK never9
task -create $TASK -source_task <embedded> -copy *$TASK* -copy_assumes -set
prove -task $TASK
prove -task FreeQ
"#,
    )
    .unwrap();
    assert_eq!(sh.eval("task").unwrap(), "never9");
    assert_eq!(status(&sh, "never9::a_never9_A"), "proven");
    assert_eq!(status(&sh, "FreeQ::a_max_A"), "cex");
    // Disabled in the embedded task, and not proved there.
    assert_eq!(
        sh.eval("get_property_list -task <embedded> -include {type assert disabled 0}")
            .unwrap(),
        "a_never9_A"
    );
    assert_eq!(status(&sh, "<embedded>::a_max_A"), "unprocessed");
}
