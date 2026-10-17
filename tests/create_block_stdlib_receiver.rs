//! `Tempfile.create(...) { |f| }` is Ruby's own block factory, not an
//! ActiveRecord record factory: block-form `create` inlining must leave
//! it alone (it used to emit `file = Tempfile.new(...)` + `file.save`).

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const SRC: &str = r#"
require "tempfile"
class Workspace
  #: (String, String) -> void
  def write_json(dir, text)
    Tempfile.create([".lithic-", ".json"], dir) do |file|
      file.write(text)
      file.flush
    end
  end
end
"#;

#[test]
fn tempfile_create_block_is_not_inlined_as_a_record_factory() {
    let (emitted, _errors) = emit_and_run::real_blog()
        .write("app/models/workspace.rb", SRC)
        .emit(BuildTarget::Ruby);
    let text = std::fs::read_to_string(emitted.join("app/models/workspace.rb")).unwrap();
    assert!(text.contains("Tempfile.create("), "{text}");
    assert!(!text.contains("Tempfile.new"), "{text}");
    assert!(!text.contains(".save"), "{text}");
}
