//! `include?` is Module's ancestry query and the membership test of
//! Array, String, Hash, Set and Range. It is refused as Module protocol
//! only where the receiver is known not to be a class object. A value
//! typed `Class` answers the query (`klass.include?(Mod)`). A receiver of
//! unknown or gradual type gets the diagnostics any other send on it
//! gets, not a claim that a Module was required.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

fn app(lib: &str) -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("test/test_helper.rb", "require \"active_support/test_case\"\n")
        .write("lib/tags.rb", lib)
}

fn errors(lib: &str) -> Vec<String> {
    app(lib).emit(BuildTarget::Ruby).1
}

const TAGS: &str = r#"class Tags
  def tagged?(record)
    record.class.include?(Tagged)
  end
end
"#;

#[test]
fn a_class_value_answers_include() {
    app(TAGS)
        .write("lib/tagged.rb", "module Tagged\n  def tag\n    \"tagged\"\n  end\nend\n")
        .write("lib/note.rb", "class Note\n  include Tagged\nend\n")
        .write("lib/plain.rb", "class Plain\nend\n")
        .write("test/tags_test.rb", r#"require "test_helper"
require "tags"
class TagsTest < ActiveSupport::TestCase
  test "the ancestry query on a record's class" do
    assert_equal true, Tags.new.tagged?(Note.new)
    assert_equal false, Tags.new.tagged?(Plain.new)
  end
end
"#)
        .run_test("test/models/tags_test.rb")
        .assert_passes();
}

#[test]
fn an_unknown_or_gradual_receiver_is_not_a_missing_module() {
    for body in ["value.include?(1)", "T.unsafe(value).include?(1)"] {
        let errors = errors(&format!("class Tags\n  def has?(value)\n    {body}\n  end\nend\n"));
        assert!(!errors.iter().any(|e| e.contains("Module protocol")), "{body}: {errors:#?}");
    }
}

#[test]
fn a_known_instance_is_still_refused() {
    let errors = errors("class Tags\n  def has?\n    Tags.new.include?(1)\n  end\nend\n");
    assert!(errors.iter().any(|e| e.contains("Module protocol")), "{errors:#?}");
}
