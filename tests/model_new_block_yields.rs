//! `Model.new { |m| ... }` yields the new record to the block, after the
//! attributes are assigned (ActiveRecord::Core#initialize). Core's
//! `Country.build_from_local_data` builds every country that way; a block
//! that never ran left it with no code and an empty name, so address
//! validation had no CountryDb entry for it.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const SCHEMA: &str = r##"ActiveRecord::Schema.define(version: 1) do
  create_table "countries", force: :cascade do |t|
    t.string "name", default: "", null: false
    t.string "code"
  end
end
"##;

const COUNTRY: &str = r##"class Country < ApplicationRecord
  def self.build_from_local_data(params)
    Country.new do |country|
      country.name = params["name"]
      country.code = params["code"]
    end
  end
end
"##;

#[test]
fn a_model_new_block_receives_the_record() {
    emit_and_run::empty_app()
        .write("db/schema.rb", SCHEMA)
        .write("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n")
        .write("app/models/country.rb", COUNTRY)
        .run_ruby(
            r#"
country = Country.build_from_local_data("name" => "Canada", "code" => "CA")
raise country.inspect unless country.name == "Canada" && country.code == "CA"
later = Country.new(name: "Peru") { |c| c.code = "PE" }
raise later.inspect unless later.name == "Peru" && later.code == "PE"
puts "passed"
"#,
        )
        .assert_passes();
}
