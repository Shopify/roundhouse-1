//! `ActionController::TestCase#process` asks the controller whether
//! `process_action` is emitted. It must ask `Object#method` by name: Core's
//! `ApplicationController` has a `method` reader (an ivar the views read),
//! and `controller.method(:process_action)` called it with an argument:
//! "wrong number of arguments (given 1, expected 0)" before any action ran.

const TEST_CASE: &str = include_str!("../runtime/spinel/test/action_controller_test_case.rb");

#[test]
fn process_asks_object_method_by_name() {
    assert!(!TEST_CASE.contains("controller.method(:process_action)"), "a controller may define its own `method`");
    assert!(
        TEST_CASE.contains("Object.instance_method(:method).bind_call(controller, :process_action)"),
        "the owner of process_action is read through Object#method"
    );
    // The same call, run: a class whose own `method` takes no argument.
    let script = "class C\n  def method = :reader\n  def process_action = 1\nend\n\
                  p Object.instance_method(:method).bind_call(C.new, :process_action).owner\n";
    let output = std::process::Command::new("ruby").args(["-e", script]).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "C");
}
