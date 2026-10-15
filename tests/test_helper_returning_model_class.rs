//! A shared-tests method returns the model class under test (Core's
//! `SavedSearchWithViewSharedTests#saved_search_class`, a
//! `self.class.const_get(:MODEL_CLASS)`), and the included tests call
//! `saved_search_class.create!(..)`. The test-module lowering took the
//! receiver for an undeclared has_many of the test class and wrote
//! `::SavedSearchClass.create!(.., <test>_id: self.id)`: every such test
//! died `NameError: uninitialized constant SavedSearchClass`.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const SCHEMA: &str = "ActiveRecord::Schema.define do\n  create_table :widgets do |t|\n    t.string :name\n  end\nend\n";

const APPLICATION_RECORD: &str = "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n";

const WIDGET: &str = "class Widget < ApplicationRecord\nend\n";

const SHARED: &str = r#"module SharedTests
  def saved_search_class
    self.class.const_get(:MODEL_CLASS)
  end

  def test_creates_through_the_helper
    record = saved_search_class.create!(name: "x")
    assert_equal "x", record.name
    assert_equal 1, saved_search_class.count
  end
end
"#;

const TEST: &str = r#"require "test_helper"
require "support/shared_tests"

class WidgetTest < ActiveSupport::TestCase
  MODEL_CLASS = Widget
  include SharedTests
end
"#;

fn app() -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test/support\n")
        .write("db/schema.rb", SCHEMA)
        .write("app/models/application_record.rb", APPLICATION_RECORD)
        .write("app/models/widget.rb", WIDGET)
        .write("test/support/shared_tests.rb", SHARED)
        .write("test/models/widget_test.rb", TEST)
}

#[test]
fn a_helper_returning_a_model_class_is_not_read_as_an_association() {
    let (emitted, _) = app().emit(BuildTarget::Ruby);
    let test = std::fs::read_to_string(emitted.join("test/models/widget_test.rb")).expect("emitted test");
    assert!(!test.contains("SavedSearchClass"), "the helper is read as an association:\n{test}");
    assert!(!test.contains("Association.created"), "{test}");
    let run = app().run_test("test/models/widget_test.rb");
    run.assert_passes();
}
