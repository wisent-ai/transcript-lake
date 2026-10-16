//! Real CLI journey: `label add --source` accepts only a declared origin,
//! with an optional `:detail`, and refuses anything else by naming every
//! origin it accepts and the value it was given. The refusal comes before
//! any session lookup, so an empty Lake is enough; the journey, its inputs
//! and outputs are retained under the package's build directory by
//! `evidence`.
#[path = "../harnesses/evidence.rs"]
mod evidence;

#[test]
fn an_undeclared_source_is_refused_with_every_accepted_origin() {
    let work = evidence::directory();
    let lake = work.join("lake");
    let output = evidence::run(
        &work,
        "label-add-undeclared-source",
        &[
            "--data-dir",
            lake.to_str().expect("UTF-8 Lake path"),
            "label",
            "add",
            "any-session",
            "--aspect",
            "topic",
            "--value",
            "test",
            "--source",
            "bogus:route",
        ],
        &[],
    );
    let said = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "an undeclared source was accepted: {said}"
    );
    assert!(
        said.contains("manual, human, model, brama"),
        "the refusal does not name every accepted origin: {said}"
    );
    assert!(
        said.contains("\"bogus:route\""),
        "the refusal does not name the value given: {said}"
    );
}
