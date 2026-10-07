//! A constant written inside another constant's value.
//!
//! Core's `Home::Onboarding::Modules::ActionTypes::ButtonMethod` declares
//! its enum members inside the list of them:
//! `ALL = [VISIT = T.let(ButtonMethod.new(…), ButtonMethod), …].freeze`.
//! Ruby defines VISIT in ButtonMethod when ALL's value runs. Native Ruby
//! on the same source is the oracle for the values, their order and
//! their identity with ALL's elements.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const BUTTON_METHOD: &str = r#"# frozen_string_literal: true

class ButtonMethod
  attr_reader :value

  def initialize(value:)
    @value = value
  end

  ALL = [
    VISIT = ButtonMethod.new(value: "VISIT"),
    ButtonMethod.new(value: "ANON"),
    MODAL = ButtonMethod.new(value: "MODAL"),
  ].freeze
  BY_VALUE = { (KEY = "visit") => VISIT }.freeze

  def self.report
    [VISIT.value, MODAL.value, ALL.map(&:value), ALL.first.equal?(VISIT), BY_VALUE[KEY].equal?(VISIT)]
  end
end
"#;

const READER: &str = r#"# frozen_string_literal: true

module Reader
  def self.visit
    ButtonMethod::VISIT.value
  end
end
"#;

const REPORT: &str = "p ButtonMethod.report, Reader.visit\n";

#[test]
fn a_constant_written_in_another_constant_s_value_is_declared_as_in_native_ruby() {
    let dir = std::env::temp_dir().join(format!("roundhouse-nested-const-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("button_method.rb"), BUTTON_METHOD).unwrap();
    std::fs::write(dir.join("reader.rb"), READER).unwrap();
    let native = std::process::Command::new("ruby")
        .arg("-e")
        .arg(format!("require {:?}\nrequire {:?}\n{REPORT}", dir.join("button_method.rb"), dir.join("reader.rb")))
        .output()
        .expect("native Ruby control");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    let expected = String::from_utf8_lossy(&native.stdout).into_owned();
    assert!(expected.contains("[\"VISIT\", \"ANON\", \"MODAL\"], true, true]"), "native control: {expected}");

    let (emitted, errors) = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("lib/button_method.rb", BUTTON_METHOD)
        .write("lib/reader.rb", READER)
        .emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{}", errors.join("\n"));
    let output = emit_and_run::ruby()
        .arg("-e")
        .arg(format!("require \"./app/models/button_method\"\nrequire \"./app/models/reader\"\n{REPORT}"))
        .current_dir(&emitted)
        .output()
        .expect("spawn ruby");
    assert!(output.status.success(), "emitted program failed in {}:\n{}", emitted.display(), String::from_utf8_lossy(&output.stderr));
    assert_eq!(String::from_utf8_lossy(&output.stdout), expected);
}

/// A write the value may not evaluate (a branch, a block) does not
/// declare the constant: its reads stay refused.
#[test]
fn a_constant_written_in_a_branch_of_another_s_value_stays_unknown() {
    let source = r#"# frozen_string_literal: true

class Flags
  ON = ENV["FLAG"] ? (LATE = 1) : 2
  LIST = [1].map { |x| x }

  def self.late
    LATE
  end
end
"#;
    let (_, errors) = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("lib/flags.rb", source)
        .emit(BuildTarget::Ruby);
    assert!(
        errors.iter().any(|e| e.contains("constant not supported (all targets): LATE") || e.contains("Flags::LATE")),
        "{}",
        errors.join("\n")
    );
}
