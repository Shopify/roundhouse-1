//! A class macro that `define_method`s a reader, called above the model's own
//! `def` of the same name, leaves the `def` standing (Ruby runs the macro
//! first). The Ruby-family emitter writes replayed macros below the methods,
//! so the `def` was replaced by the macro's reader. Core's
//! `money_column(:variant_price)` above `def variant_price` in LineItem
//! returned nil, and a gift card bought from it had no initial value.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const SCHEMA: &str = r##"ActiveRecord::Schema.define(version: 1) do
  create_table "line_items", force: :cascade do |t|
    t.string "title"
  end
end
"##;

const MACROS: &str = r##"module Macros
  def reader_macro(name)
    define_method(name) { :from_macro }
  end
end
"##;

const LINE_ITEM: &str = r##"class LineItem < ApplicationRecord
  extend Macros

  reader_macro(:variant_price)
  reader_macro(:only_macro)

  def variant_price
    :from_def
  end
end
"##;

#[test]
fn a_def_below_a_reader_macro_stands() {
    emit_and_run::empty_app()
        .write("db/schema.rb", SCHEMA)
        .write("app/models/macros.rb", MACROS)
        .write("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n")
        .write("app/models/line_item.rb", LINE_ITEM)
        .run_ruby(
            r#"
item = LineItem.new
raise item.variant_price.inspect unless item.variant_price == :from_def
raise item.only_macro.inspect unless item.only_macro == :from_macro
puts "passed"
"#,
        )
        .assert_passes();
}
