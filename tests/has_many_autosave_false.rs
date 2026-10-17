//! `has_many ..., autosave: false`: the owner's save never saves the
//! collection's new records (`save_collection_association` inserts only when
//! `autosave != false`); a plain `has_many` still saves them. Core's
//! `Sales::Agreement` declares `sales`, `sale_taxes` and `sale_discounts` so,
//! and `AgreementBuilder` bulk-creates them after `agreement.save`: saved by
//! the agreement first, `BulkSqlOperations::Create` answered "All records
//! must be new records" and rolled the whole order edit back.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const SCHEMA: &str = r##"ActiveRecord::Schema.define(version: 1) do
  create_table "agreements", force: :cascade do |t|
    t.string "name"
  end

  create_table "sales", force: :cascade do |t|
    t.integer "agreement_id"
  end

  create_table "notes", force: :cascade do |t|
    t.integer "agreement_id"
  end
end
"##;

#[test]
fn autosave_false_collection_is_not_saved_with_its_owner() {
    emit_and_run::empty_app()
        .write("db/schema.rb", SCHEMA)
        .write("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n")
        .write(
            "app/models/agreement.rb",
            "class Agreement < ApplicationRecord\n  has_many :sales, inverse_of: :agreement, autosave: false\n  has_many :notes, inverse_of: :agreement\nend\n",
        )
        .write("app/models/sale.rb", "class Sale < ApplicationRecord\n  belongs_to :agreement, inverse_of: :sales, optional: true\nend\n")
        .write("app/models/note.rb", "class Note < ApplicationRecord\n  belongs_to :agreement, inverse_of: :notes, optional: true\nend\n")
        .run_ruby(
            r##"
agreement = Agreement.new(name: "a")
sale = agreement.sales.build
note = agreement.notes.build
raise "save failed" unless agreement.save
raise "autosave: false saved the sale" unless sale.new_record?
raise "the plain has_many did not save the note" unless note.persisted?
puts "passed"
"##,
        )
        .assert_passes();
}
