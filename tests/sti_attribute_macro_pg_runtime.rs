//! An STI subclass whose own body calls an app class macro that reaches
//! `attribute` (Core's `model_uuid_attributes` in a subclass, which Core
//! keys on the subclass's name), against native Rails 8.2 on the same
//! local PostgreSQL database.
//!
//! The declaration applies to the subclass alone: `SpecialItem#ref` reads
//! and writes through the registered type while `Item#ref`, on the same
//! table, stays a uuid String. Ingest keeps the subclass as a library
//! class over the model's compiled accessors, so the model compiles `ref`
//! as retypable and the subclass's call runs as written at class load,
//! declaring the type on the subclass (the runtime's per-class `attribute`).
#[path = "support/emit_and_run.rs"]
mod emit_and_run;
#[path = "support/pg_probe.rs"]
mod pg_probe;

const STRUCTURE: &str = r#"CREATE TABLE items (
    id bigserial NOT NULL,
    type character varying,
    ref uuid,
    name character varying,
    CONSTRAINT items_pkey PRIMARY KEY (id)
);
"#;

const DATABASE_YML: &str = r#"default: &default
  adapter: postgresql
  encoding: unicode

test:
  <<: *default
  database: rh_e2e_sti_attribute_macro
"#;

const APPLICATION_RECORD: &str = r#"class ApplicationRecord < ActiveRecord::Base
  self.abstract_class = true

  def self.uuid_attributes(*names)
    names.each do |name|
      attribute name.to_sym, :uuid_as_integer
    end
  end
end
"#;

const ITEM: &str = r#"class Item < ApplicationRecord
end
"#;

const SPECIAL_ITEM: &str = r#"class SpecialItem < Item
  uuid_attributes :ref
end
"#;

/// Shaped like Core's Shopify::ModelUuidAsInteger.
const UUID_AS_INTEGER: &str = r##"module Probe
  class UuidAsInteger < ActiveModel::Type::Value
    ACCEPTABLE_UUID = /\A\h{8}-?\h{4}-?\h{4}-?\h{4}-?\h{12}\z/

    def type
      :uuid_as_integer
    end

    def deserialize(value)
      return if value.nil?
      cast_value(value)
    end

    def cast_value(value)
      if value.is_a?(Integer)
        value
      elsif value.is_a?(String) && ACCEPTABLE_UUID.match?(value)
        value.delete("-").to_i(16)
      elsif value.respond_to?(:to_i)
        value.to_i
      end
    end

    def serialize(value)
      return if value.nil?
      int = cast_value(value)
      return if int.nil?
      hex = int.to_s(16).rjust(32, "0")
      "#{hex[0, 8]}-#{hex[8, 4]}-#{hex[12, 4]}-#{hex[16, 4]}-#{hex[20, 12]}"
    end
  end
end
"##;

const INITIALIZER: &str =
    "ActiveRecord::Type.register(:uuid_as_integer, Probe::UuidAsInteger, adapter: :postgresql)\n";

const PROBE: &str = r##"class StiAttributeMacroProbe
  def self.lines
    out = []
    plain = Item.create!(name: "p", ref: "00000000-0000-0000-0000-000000000042")
    special = SpecialItem.create!(name: "s", ref: 0x42)
    out << "plain: #{plain.ref.class} #{plain.ref}"
    out << "special: #{special.ref.class} #{special.ref}"
    found = Item.find(special.id)
    out << "sti find: #{found.class} #{found.ref.class} #{found.ref}"
    out << "where: #{SpecialItem.where(ref: 0x42).count} #{Item.where(ref: "00000000-0000-0000-0000-000000000042").count}"
    special.ref = 0x43
    special.save!
    out << "update: #{SpecialItem.find(special.id).ref} #{Item.where(id: special.id).pluck(:ref).inspect}"
    out << "plain reload: #{plain.reload.ref.class} #{plain.ref}"
    out
  end
end
"##;

/// What native Rails 8.2 answers (checked on the task PostgreSQL), so two
/// identical wrong answers still fail.
const EXPECTED: [&str; 6] = [
    "plain: String 00000000-0000-0000-0000-000000000042",
    "special: Integer 66",
    "sti find: SpecialItem Integer 66",
    "where: 1 2",
    "update: 67 [\"00000000-0000-0000-0000-000000000043\"]",
    "plain reload: String 00000000-0000-0000-0000-000000000042",
];

/// What native Rails requires, in order.
const LOADS: [&str; 6] = [
    "lib/probe/uuid_as_integer",
    "config/initializers/uuid_types",
    "app/models/application_record",
    "app/models/item",
    "app/models/special_item",
    "app/models/sti_attribute_macro_probe",
];

const FILES: [(&str, &str); 10] = [
    ("roundhouse.yml", "database_adapter: postgresql\n"),
    ("config/database.yml", DATABASE_YML),
    ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
    ("db/structure.sql", STRUCTURE),
    ("app/models/application_record.rb", APPLICATION_RECORD),
    ("app/models/item.rb", ITEM),
    ("app/models/special_item.rb", SPECIAL_ITEM),
    ("app/models/sti_attribute_macro_probe.rb", PROBE),
    ("lib/probe/uuid_as_integer.rb", UUID_AS_INTEGER),
    ("config/initializers/uuid_types.rb", INITIALIZER),
];

fn overlay() -> emit_and_run::Overlay {
    FILES.iter().fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text))
}

/// `check`'s errors other than the PostgreSQL adapter notice every
/// PostgreSQL probe app carries. The subclass's `uuid_attributes` call is
/// claimed: it runs as written at class load.
fn real_errors(errors: &[String]) -> Vec<&String> {
    errors.iter().filter(|e| !e.contains("database_adapter not supported")).collect()
}

/// The compiler side: the model's `ref` consults the declared type, and
/// the subclass carries its call.
#[test]
fn sti_subclass_attribute_macro_runs_on_the_subclass() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    let item = std::fs::read_to_string(emitted.join("app/models/item.rb")).unwrap();
    assert!(item.contains(r#"self.class._attribute_deserialize("ref", value)"#), "item:\n{item}");
    let special = std::fs::read_to_string(emitted.join("app/models/special_item.rb")).unwrap();
    assert!(special.contains("  uuid_attributes :ref\n"), "special_item:\n{special}");
}

/// The model files natively and emitted, same PostgreSQL.
#[test]
#[ignore = "requires pg gem, Core's pinned Rails and ROUNDHOUSE_PG_TEST_URL for a disposable local PostgreSQL server"]
fn sti_subclass_attribute_macro_matches_native_rails_on_postgresql() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    let (native, emitted_lines) = pg_probe::native_and_emitted(
        "sti-attribute-macro",
        &FILES,
        &emitted,
        "rh_e2e_sti_attribute_macro",
        "StiAttributeMacroProbe",
        &LOADS,
    );
    assert_eq!(native, EXPECTED, "native");
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    assert_eq!(emitted_lines, native, "emitted");
}
