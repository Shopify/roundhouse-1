//! Core's `BulkSqlOperations::Create` marks the rows it inserted with
//! `record.instance_variable_set(:@new_record, false)`, Rails' own flag. The
//! runtime keeps `@persisted`, so those records stayed new and a later
//! `save` (a belongs_to autosave from a sale tax) inserted the `Sales::Sale`
//! again: "duplicate key value violates unique constraint sales_pkey".
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const SCHEMA: &str = r##"ActiveRecord::Schema.define(version: 1) do
  create_table "things", force: :cascade do |t|
    t.string "name"
  end
end
"##;

#[test]
fn new_record_ivar_set_false_marks_the_record_persisted() {
    emit_and_run::empty_app()
        .write("db/schema.rb", SCHEMA)
        .write("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n")
        .write("app/models/thing.rb", "class Thing < ApplicationRecord\nend\n")
        .run_ruby(
            r##"
thing = Thing.new(name: "a")
raise "a new record is not new" unless thing.new_record? && !thing.persisted?
thing.instance_variable_set(:@new_record, false)
raise "the flag was ignored" if thing.new_record? || !thing.persisted?
fresh = Thing.create!(name: "b")
raise "a saved record is new" if fresh.new_record? || !fresh.persisted?
fresh.destroy
raise "a destroyed record is persisted" if fresh.persisted? || fresh.new_record?
puts "passed"
"##,
        )
        .assert_passes();
}
