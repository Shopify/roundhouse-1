//! `attribute :options, :json, default: []` over a `jsonb` column (Core's
//! MetafieldDefinition), against native Rails 8.2 on the same local
//! PostgreSQL database.
//!
//! The declaration makes the column retypable: its slot holds the declared
//! type's value (an Array), not stored text. The schema-less JSON accessors
//! (`attributes`, the dirty snapshot, `[]`) decoded that slot with
//! `RoundhouseJsonColumn.load` anyway, which raised "no implicit
//! conversion of Array into String" the first time a record with an
//! assigned Array was saved.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;
#[path = "support/pg_probe.rs"]
mod pg_probe;

const STRUCTURE: &str = r#"CREATE TABLE definitions (
    id bigserial NOT NULL,
    name character varying,
    options jsonb,
    CONSTRAINT definitions_pkey PRIMARY KEY (id)
);
"#;

const DATABASE_YML: &str = r#"default: &default
  adapter: postgresql
  encoding: unicode

test:
  <<: *default
  database: rh_e2e_attribute_json_default
"#;

const APPLICATION_RECORD: &str = r#"class ApplicationRecord < ActiveRecord::Base
  self.abstract_class = true
end
"#;

const DEFINITION: &str = r#"class Definition < ApplicationRecord
  attribute :options, :json, default: []

  after_validation :serialize_options!

  def serialize_options!
    return if options.empty? || options.first.is_a?(Hash)

    self.options = options.collect { |n| { "name" => n, "value" => n.to_s.upcase } }
  end
end
"#;

const PROBE: &str = r##"class AttributeJsonDefaultProbe
  def self.lines
    out = []
    d = Definition.new(name: "a")
    out << "new: #{d.options.inspect}"
    d.options = ["x", "y"]
    out << "assigned: #{d.options.inspect} #{d.attributes["options"].inspect} #{d[:options].inspect}"
    d.save!
    out << "saved: #{d.options.inspect} #{d.changed?}"
    found = Definition.find(d.id)
    out << "found: #{found.options.inspect} #{found.attributes["options"].inspect} #{found[:options].inspect}"
    out << "default: #{Definition.create!(name: "b").reload.options.inspect}"
    found.options = [{ "name" => "z", "value" => "Z" }]
    found.save!
    out << "updated: #{Definition.find(d.id).options.inspect}"
    out
  end
end
"##;

/// What native Rails 8.2 answers (checked on the task PostgreSQL), so two
/// identical wrong answers still fail.
const EXPECTED: [&str; 6] = [
    "new: []",
    r#"assigned: ["x", "y"] ["x", "y"] ["x", "y"]"#,
    r#"saved: [{"name" => "x", "value" => "X"}, {"name" => "y", "value" => "Y"}] false"#,
    r#"found: [{"name" => "x", "value" => "X"}, {"name" => "y", "value" => "Y"}] [{"name" => "x", "value" => "X"}, {"name" => "y", "value" => "Y"}] [{"name" => "x", "value" => "X"}, {"name" => "y", "value" => "Y"}]"#,
    "default: []",
    r#"updated: [{"name" => "z", "value" => "Z"}]"#,
];

/// What native Rails requires, in order.
const LOADS: [&str; 3] = [
    "app/models/application_record",
    "app/models/definition",
    "app/models/attribute_json_default_probe",
];

const FILES: [(&str, &str); 7] = [
    ("roundhouse.yml", "database_adapter: postgresql\n"),
    ("config/database.yml", DATABASE_YML),
    ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
    ("db/structure.sql", STRUCTURE),
    ("app/models/application_record.rb", APPLICATION_RECORD),
    ("app/models/definition.rb", DEFINITION),
    ("app/models/attribute_json_default_probe.rb", PROBE),
];

fn overlay() -> emit_and_run::Overlay {
    FILES.iter().fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text))
}

/// The models natively and emitted, same PostgreSQL.
#[test]
#[ignore = "requires pg gem, Core's pinned Rails and ROUNDHOUSE_PG_TEST_URL for a disposable local PostgreSQL server"]
fn attribute_json_default_matches_native_rails_on_postgresql() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    let (native, emitted_lines) = pg_probe::native_and_emitted(
        "attribute-json-default",
        &FILES,
        &emitted,
        "rh_e2e_attribute_json_default",
        "AttributeJsonDefaultProbe",
        &LOADS,
    );
    assert_eq!(native, EXPECTED, "native");
    let real: Vec<_> = errors.iter().filter(|e| !e.contains("database_adapter not supported")).collect();
    assert!(real.is_empty(), "{}", errors.join("\n"));
    assert_eq!(emitted_lines, native, "emitted");
}
