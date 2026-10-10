//! A module body's statement written below its nested classes runs
//! after them, as Ruby runs the body in source order, though each class
//! keeps a file of its own. Core's GraphApiTestSchemaHelper calls
//! `api_versioning_test_setup`, declares its test schema classes (whose
//! `visibility:` names fixture-only ApiChange handles), then
//! `reset_api_versioning_test_setup`. The module's file ran the reset
//! before the classes' files loaded, so every class body saw the real
//! handles and raised InvalidApiChange.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const GATE: &str = r##"module Gate
  def self.open=(value)
    @open = value
  end

  def self.open?
    @open
  end

  def self.seen
    @seen ||= []
  end

  def open_gate
    Gate.open = true
  end

  def close_gate
    Gate.open = false
  end
end
"##;

const DECLARED: &str = r##"class Declared
  def self.declare(name)
    raise "#{name} declared while the gate is closed" unless Gate.open?
    Gate.seen << name
  end
end
"##;

const HELPER: &str = r##"module SchemaHelper
  extend ::Gate

  open_gate

  class First < Declared
    declare :first
  end

  class Second < Declared
    declare :second
  end

  close_gate
end
"##;

#[test]
fn nested_classes_load_before_the_module_statement_below_them() {
    let script = "p SchemaHelper::Second.name\np Gate.seen\n";
    let native = emit_and_run::ruby()
        .args(["-W:no-deprecated", "-e", &format!("{GATE}\n{DECLARED}\n{HELPER}\n{script}")])
        .output()
        .expect("CRuby control");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));

    let (tree, errors) = emit_and_run::empty_app()
        .write("app/models/gate.rb", GATE)
        .write("app/models/declared.rb", DECLARED)
        .write("app/models/schema_helper.rb", HELPER)
        .emit(BuildTarget::Ruby);
    let emitted = tree.join("app/models/schema_helper.rb");
    let text = std::fs::read_to_string(&emitted).unwrap_or_else(|_| panic!("no {}: {errors:#?}", emitted.display()));
    assert!(tree.join("app/models/schema_helper/first.rb").exists(), "each class keeps its own file:\n{text}");

    let run = std::process::Command::new("ruby")
        .current_dir(&tree)
        .env("BLOG_DB", ":memory:")
        .arg("-e")
        .arg(format!("require File.expand_path(\"main\", Dir.pwd)\nMain.configure_default_adapter!\n{script}"))
        .output()
        .expect("spawn ruby");
    assert!(run.status.success(), "{}\n{text}", String::from_utf8_lossy(&run.stderr));
    assert_eq!(String::from_utf8_lossy(&run.stdout), String::from_utf8_lossy(&native.stdout), "{text}");
}
