//! `Model.type_for_attribute` over an `enum` attribute
//! (ActiveRecord::Enum::EnumType, activerecord/lib/active_record/enum.rb),
//! against native Rails 8.2 on the same local PostgreSQL database: the
//! type's class, its subtype, `cast` / `serialize` / `deserialize` of a
//! label, a stored value and an unknown value, `assert_valid_value`, and
//! `attributes_for_database`.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;
#[path = "support/pg_probe.rs"]
mod pg_probe;

const STRUCTURE: &str = r#"CREATE TABLE tickets (
    id bigserial NOT NULL,
    stage integer,
    tone character varying,
    phase character varying,
    CONSTRAINT tickets_pkey PRIMARY KEY (id)
);
"#;

const DATABASE_YML: &str = r#"default: &default
  adapter: postgresql
  encoding: unicode

test:
  <<: *default
  database: rh_e2e_enum_type
"#;

const APPLICATION_RECORD: &str = r#"class ApplicationRecord < ActiveRecord::Base
  self.abstract_class = true
end
"#;

const TICKET: &str = r#"class Ticket < ApplicationRecord
  enum :stage, { early: 1, late: 2 }
  enum :tone, { soft: "s", loud: "l" }
  enum :phase, { pending: :pending, active: :active }, allow_nil: true
end
"#;

const PROBE: &str = r##"class EnumTypeProbe
  def self.try(&block)
    block.call.inspect
  rescue => e
    "#{e.class}: #{e.message}"
  end

  def self.lines
    out = []
    stage = Ticket.type_for_attribute("stage")
    tone = Ticket.type_for_attribute(:tone)
    out << "types: #{stage.class} #{stage.subtype.class} #{stage.type.inspect} #{tone.subtype.class} #{tone.type.inspect}"
    out << "same: #{Ticket.type_for_attribute("stage").equal?(stage)}"
    phase = Ticket.type_for_attribute("phase")
    out << "phase: #{phase.class} #{phase.subtype.class} #{phase.type.inspect} #{[phase.cast("pending"), phase.cast(:active), phase.serialize("pending"), phase.serialize(:active), phase.deserialize("active"), phase.deserialize(nil)].inspect}"
    [["stage", stage, ["late", :late, 2, "2", "other", nil, ""]], ["tone", tone, ["soft", :soft, "l", "x", nil, " "]]].each do |name, type, values|
      values.each do |value|
        out << "#{name} #{value.inspect}: cast=#{try { type.cast(value) }} serialize=#{try { type.serialize(value) }} serializable=#{try { type.serializable?(value) }} valid=#{try { type.assert_valid_value(value) }}"
      end
    end
    out << "deserialize: #{try { stage.deserialize(1) }} #{try { stage.deserialize(9) }} #{try { stage.deserialize(nil) }} #{try { tone.deserialize("s") }}"
    out << "query_predicates: #{stage.query_predicates?}"
    ticket = Ticket.create!(stage: :late, tone: :loud)
    found = Ticket.find(ticket.id)
    out << "for_database: #{found.attributes_for_database.except("id").inspect}"
    out << "attribute_types: #{Ticket.attribute_types["stage"].equal?(stage)} #{Ticket.attribute_types.keys.sort.inspect}"
    out
  end
end
"##;

const FILES: [(&str, &str); 7] = [
    ("roundhouse.yml", "database_adapter: postgresql\n"),
    ("config/database.yml", DATABASE_YML),
    ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
    ("db/structure.sql", STRUCTURE),
    ("app/models/application_record.rb", APPLICATION_RECORD),
    ("app/models/ticket.rb", TICKET),
    ("app/models/enum_type_probe.rb", PROBE),
];

/// What native Rails requires, in order.
const LOADS: [&str; 3] = ["app/models/application_record", "app/models/ticket", "app/models/enum_type_probe"];

fn overlay() -> emit_and_run::Overlay {
    FILES.iter().fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text))
}

/// `check`'s errors other than the PostgreSQL adapter notice every
/// PostgreSQL probe app carries.
fn real_errors(errors: &[String]) -> Vec<&String> {
    errors.iter().filter(|e| !e.contains("database_adapter not supported")).collect()
}

/// The compiler side: an enum column's schema entry names the mapping
/// over the column's own type, not an unported class.
#[test]
fn an_enum_column_s_schema_entry_carries_its_mapping_and_subtype() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    let ticket = std::fs::read_to_string(emitted.join("app/models/ticket.rb")).unwrap();
    assert!(ticket.contains(r#""stage" => [:enum, [["early", 1], ["late", 2]], [:integer, 4]]"#), "{ticket}");
    assert!(ticket.contains(r#""tone" => [:enum, [["soft", "s"], ["loud", "l"]], [:string, nil]]"#), "{ticket}");
    assert!(ticket.contains(r#""phase" => [:enum, [["pending", "pending"], ["active", "active"]], [:string, nil]]"#), "{ticket}");
    assert!(!ticket.contains("EnumType"), "{ticket}");
}

/// The model files natively and emitted, same PostgreSQL.
#[test]
#[ignore = "requires pg gem, Core's pinned Rails and ROUNDHOUSE_PG_TEST_URL for a disposable local PostgreSQL server"]
fn enum_type_matches_native_rails_on_postgresql() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    let (native, emitted_lines) = pg_probe::native_and_emitted(
        "enum-type",
        &FILES,
        &emitted,
        "rh_e2e_enum_type",
        "EnumTypeProbe",
        &LOADS,
    );
    assert!(native.iter().any(|l| l.starts_with("types: ActiveRecord::Enum::EnumType ")), "{native:#?}");
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    assert_eq!(emitted_lines, native, "emitted");
}
