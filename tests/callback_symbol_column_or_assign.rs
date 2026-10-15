//! `before_validation :generate_uuid` over `def generate_uuid; self.uuid
//! ||= SecureRandom.uuid; end` (Core's Shipping::LabelPurchaseSession, whose
//! `validates :uuid, presence: true` raised "Uuid can't be blank"). Rails'
//! new-record string attribute is nil, so `||=` assigns it. The runtime's
//! storage defaults a string slot to "", which is truthy; the block form of
//! the callback already read `||=` as "assign when blank"
//! (`markers::rewrite_column_or_assign`), and a callback that names a method
//! gets the same reading of that method's body.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const SCHEMA: &str = "ActiveRecord::Schema[8.1].define(version: 2026_01_01_000000) do\n  create_table \"sessions\", force: :cascade do |t|\n    t.string \"uuid\", null: false\n    t.string \"label\"\n  end\nend\n";

const SESSION: &str = r#"class Session < ApplicationRecord
  before_validation :generate_uuid
  validates :uuid, presence: true

  private

  def generate_uuid
    self.uuid ||= SecureRandom.uuid
  end
end
"#;

const SESSION_TEST: &str = r#"require "test_helper"

class SessionTest < ActiveSupport::TestCase
  test "a callback method's ||= assigns the attribute a new record leaves unset" do
    session = Session.new(label: "a")
    session.save!
    assert_equal 36, session.uuid.length
    assert_equal session.uuid, Session.find(session.id).uuid
  end

  test "a value the record was given is kept" do
    session = Session.create!(uuid: "given", label: "b")
    assert_equal "given", session.uuid
    session.update!(label: "c")
    assert_equal "given", session.reload.uuid
  end
end
"#;

#[test]
fn a_callback_method_s_or_assign_on_a_string_column_assigns_the_unset_attribute() {
    emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write("db/schema.rb", SCHEMA)
        .write("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n")
        .write("app/models/session.rb", SESSION)
        .write("test/test_helper.rb", "require \"active_support/test_case\"\n")
        .write("test/models/session_test.rb", SESSION_TEST)
        .run_test("test/models/session_test.rb")
        .assert_passes();
}
