//! A superclass-less `include ActiveModel::Model` class is tableless even
//! when its inferred table exists: Core's `Taxes::TaxLine` (accessors and
//! `usable_title`) shares `tax_lines` with the `TaxLine` record. It took
//! that table's columns, hydrated rows through `self.class._cast_column`
//! (which only a record class has), and every `Taxes::TaxLine.new(...)`
//! raised `undefined method '_cast_column'` while an order edit
//! negotiated.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const SCHEMA: &str = r##"ActiveRecord::Schema.define(version: 1) do
  create_table "tax_lines", force: :cascade do |t|
    t.string "title"
    t.decimal "price"
  end
end
"##;

const RECORD: &str = r##"class TaxLine < ApplicationRecord
  def label
    title.to_s
  end
end
"##;

const VALUE: &str = r##"module Taxes
  class TaxLine
    include ActiveModel::Model

    attr_accessor :title, :price, :rate

    def usable_title
      title.present? ? title : "Tax"
    end
  end
end
"##;

#[test]
fn a_tableless_model_does_not_take_the_same_named_table() {
    emit_and_run::empty_app()
        .write("db/schema.rb", SCHEMA)
        .write("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n")
        .write("app/models/tax_line.rb", RECORD)
        .write("app/models/taxes/tax_line.rb", VALUE)
        .run_ruby(
            r#"
line = Taxes::TaxLine.new(title: "VAT", price: 3, rate: 0.2)
raise line.inspect unless line.title == "VAT" && line.rate == 0.2 && line.usable_title == "VAT"
raise "no usable_title fallback" unless Taxes::TaxLine.new(rate: 1).usable_title == "Tax"
raise "tableless model answers a table" if Taxes::TaxLine.respond_to?(:table_name)
puts "passed"
"#,
        )
        .assert_passes();
}
