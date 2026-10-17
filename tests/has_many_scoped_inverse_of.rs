//! `has_many :x, -> { order(:id) }, inverse_of: :owner`: a record built
//! through the scoped association holds its owner through the inverse
//! `belongs_to`. Only Rails' automatic inverse is off under a scope lambda; a
//! written `inverse_of:` still applies. Core's `Refund#refund_line_items`
//! reads so, and `RefundLineItem#line_item_belongs_to_parent_order` validates
//! through `refund&.order_id`: without the inverse the refund built by
//! `OrderEditRefundCreate` was invalid ("Refund line items refund can't be
//! blank") and the order edit commit rolled back.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const SCHEMA: &str = r##"ActiveRecord::Schema.define(version: 1) do
  create_table "refunds", force: :cascade do |t|
    t.integer "order_id"
  end

  create_table "refund_line_items", force: :cascade do |t|
    t.integer "refund_id"
  end
end
"##;

#[test]
fn scoped_has_many_with_written_inverse_sets_the_owner_on_build() {
    emit_and_run::empty_app()
        .write("db/schema.rb", SCHEMA)
        .write("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n")
        .write(
            "app/models/refund.rb",
            "class Refund < ApplicationRecord\n  has_many :refund_line_items, -> { order(:id) }, inverse_of: :refund\nend\n",
        )
        .write(
            "app/models/refund_line_item.rb",
            "class RefundLineItem < ApplicationRecord\n  belongs_to :refund, inverse_of: :refund_line_items\n  validate :belongs_to_order\n\n  def belongs_to_order\n    errors.add(:refund, \"order missing\") unless refund && refund.order_id == 7\n  end\nend\n",
        )
        .run_ruby(
            r##"
refund = Refund.new(order_id: 7)
item = refund.refund_line_items.build
raise "build did not set the inverse" unless item.refund.equal?(refund)
raise "refund invalid: #{refund.errors.full_messages}" unless refund.save
puts "passed"
"##,
        )
        .assert_passes();
}
