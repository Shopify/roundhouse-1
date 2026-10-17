//! `has_one …, autosave: true` saves its child from Rails' `after_create` and
//! `after_update`, not `after_save`: `run_callbacks(:save)` alone must leave the
//! child unsaved. Core's `Sales::AgreementBuilder#insertable_sale_associations`
//! runs `sale_tax.run_callbacks(:save)` on each new `SaleTax` before a bulk
//! insert; the `after_save` form saved the new `PresentmentSaleTax`, whose
//! `belongs_to :sale_tax` then saved the still-new `SaleTax`, and the bulk
//! create answered "All records must be new records".
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const SCHEMA: &str = r##"ActiveRecord::Schema.define(version: 1) do
  create_table "taxes", force: :cascade do |t|
    t.string "name"
  end

  create_table "presentment_taxes", force: :cascade do |t|
    t.integer "tax_id"
    t.string "currency"
  end
end
"##;

#[test]
fn has_one_autosave_child_is_saved_by_create_and_update_not_by_the_save_chain() {
    emit_and_run::empty_app()
        .write("db/schema.rb", SCHEMA)
        .write("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n")
        .write(
            "app/models/tax.rb",
            "class Tax < ApplicationRecord\n  has_one :presentment_tax, autosave: true, inverse_of: :tax\nend\n",
        )
        .write(
            "app/models/presentment_tax.rb",
            "class PresentmentTax < ApplicationRecord\n  belongs_to :tax, inverse_of: :presentment_tax, optional: true\nend\n",
        )
        .run_ruby(
            r##"
tax = Tax.new(name: "vat")
presentment = PresentmentTax.new(currency: "USD")
tax.presentment_tax = presentment
tax.send(:after_save) if tax.respond_to?(:after_save, true)
raise "the save chain saved the child" unless presentment.new_record?
raise "the save chain saved the owner" unless tax.new_record?
raise "save failed" unless tax.save
raise "create did not save the child" unless presentment.persisted? && presentment.tax_id == tax.id
tax.presentment_tax = PresentmentTax.new(currency: "EUR")
tax.save
raise "update did not save the new child" unless PresentmentTax.where(tax_id: tax.id, currency: "EUR").count == 1
puts "passed"
"##,
        )
        .assert_passes();
}
