//! A JSON column whose stored text is not JSON still hydrates: Rails decodes a
//! `serialize … coder: JSON` attribute lazily, so a read raises
//! (`PlaybookAction#arguments` rescues the `JSON::ParserError`) while a find
//! and an unrelated update leave the column as stored. The compiled model's
//! `_attribute_snapshot` (the dirty-tracking baseline `_note_hydrated` takes
//! at hydration) decoded every JSON column eagerly and raised
//! `JSON::ParserError: unexpected end of input` from the find.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const MODEL: &str = "class Action < ApplicationRecord\n  serialize :arguments, coder: JSON\nend\n";

fn overlay() -> emit_and_run::Overlay {
    emit_and_run::real_blog()
        .write("app/models/action.rb", MODEL)
        .write(
            "db/schema.rb",
            "ActiveRecord::Schema[8.0].define(version: 1) do\n  create_table \"actions\", force: :cascade do |t|\n    t.string \"status\"\n    t.text \"arguments\"\n  end\nend\n",
        )
}

#[test]
fn the_dirty_baseline_decodes_json_leniently_and_the_reader_does_not() {
    let (emitted, _errors) = overlay().emit(BuildTarget::Ruby);
    let model = std::fs::read_to_string(emitted.join("app/models/action.rb")).expect("action.rb");
    assert!(model.contains("\"arguments\" => RoundhouseJsonColumn.snapshot(@arguments)"), "{model}");
    assert!(model.contains("def arguments\n      RoundhouseJsonColumn.load(@arguments)"), "{model}");
}

#[test]
fn a_stored_text_that_is_not_json_is_its_own_snapshot() {
    emit_and_run::real_blog()
        .run_ruby(
            r#"
require "./runtime/json_column"
raise "valid" unless RoundhouseJsonColumn.snapshot('{"a":1}') == { "a" => 1 }
raise "nil" unless RoundhouseJsonColumn.snapshot(nil).nil?
raise "malformed" unless RoundhouseJsonColumn.snapshot('{"payload":') == '{"payload":'
begin
  RoundhouseJsonColumn.load('{"payload":')
  raise "load must raise"
rescue JSON::ParserError
end
puts "ok"
"#,
        )
        .assert_passes();
}
