//! A bare `valid?` does not save the new record a `belongs_to` points at:
//! Rails saves it in the owner's before_save, so `box.valid?` leaves the
//! `intent` without an id. Core's `Shipping::Processes::PersistIntents` keys
//! a Hash by its new intents, validates each box, then looks the intent up
//! again; the validation saved the intent, its hash changed with its new id,
//! and the lookup returned nil ("Something went wrong." from the mutation).
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const SCHEMA: &str = r##"ActiveRecord::Schema.define(version: 1) do
  create_table "intents", force: :cascade do |t|
    t.string "label"
  end

  create_table "boxes", force: :cascade do |t|
    t.integer "intent_id"
  end
end
"##;

#[test]
fn valid_leaves_a_new_belongs_to_target_unsaved_and_save_saves_it() {
    emit_and_run::empty_app()
        .write("db/schema.rb", SCHEMA)
        .write("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n")
        .write("app/models/intent.rb", "class Intent < ApplicationRecord\nend\n")
        .write("app/models/box.rb", "class Box < ApplicationRecord\n  belongs_to :intent\nend\n")
        .run_ruby(
            r#"
intent = Intent.new(label: "a")
box = Box.new(intent: intent)
by_intent = { intent => box }
raise "valid? failed: #{box.errors.full_messages}" unless box.valid?
raise "valid? saved the intent: #{intent.id.inspect}" unless intent.new_record? && intent.id.nil?
raise "hash lookup lost the intent" unless by_intent[intent].equal?(box)
raise "save failed" unless box.save
raise "save did not persist the intent" unless intent.persisted? && box.intent_id == intent.id
puts "passed"
"#,
        )
        .assert_passes();
}
