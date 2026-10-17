//! `Model.first!`, `last!` and `take!` on the class, as ActiveRecord's
//! querying delegates them to `all`. Core's tests read the record a mutation
//! just wrote with `Shipping::Intent.last!`; the class had only `first`,
//! `last` and `take`.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const SCHEMA: &str = r##"ActiveRecord::Schema.define(version: 1) do
  create_table "widgets", force: :cascade do |t|
    t.string "name"
  end
end
"##;

#[test]
fn class_side_bang_finders_return_the_record_or_raise() {
    emit_and_run::empty_app()
        .write("db/schema.rb", SCHEMA)
        .write("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n")
        .write("app/models/widget.rb", "class Widget < ApplicationRecord\nend\n")
        .run_ruby(
            r##"
[:first!, :last!, :take!].each do |finder|
  begin
    Widget.public_send(finder)
    raise "#{finder} on an empty table returned"
  rescue ActiveRecord::RecordNotFound
  end
end
Widget.create!(name: "a")
Widget.create!(name: "b")
raise "first!" unless Widget.first!.name == "a"
raise "last!" unless Widget.last!.name == "b"
raise "take!" unless %w[a b].include?(Widget.take!.name)
puts "passed"
"##,
        )
        .assert_passes();
}
