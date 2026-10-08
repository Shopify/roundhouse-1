//! STI subclasses that retype a column of their table with a direct
//! `attribute :value, :boolean` (Core's Setting subclasses, 29 of them),
//! against native Rails 8.2 on the same local PostgreSQL database.
//!
//! Each declaration applies to its subclass alone: `BoolSetting#value`
//! casts through the boolean type and `IntSetting#value` through the
//! integer one, while `Setting#value`, on the same varchar column, stays a
//! String. Ingest keeps the subclasses as library classes over the
//! model's compiled accessors, so the model compiles `value` as retypable
//! and each subclass's call runs as written at class load. A call with
//! `default:` or other options stays refused (the runtime ports neither).
#[path = "support/emit_and_run.rs"]
mod emit_and_run;
#[path = "support/pg_probe.rs"]
mod pg_probe;

const STRUCTURE: &str = r#"CREATE TABLE settings (
    id bigserial NOT NULL,
    type character varying,
    name character varying,
    value character varying,
    CONSTRAINT settings_pkey PRIMARY KEY (id)
);
"#;

const DATABASE_YML: &str = r#"default: &default
  adapter: postgresql
  encoding: unicode

test:
  <<: *default
  database: rh_e2e_sti_attribute
"#;

const APPLICATION_RECORD: &str = r#"class ApplicationRecord < ActiveRecord::Base
  self.abstract_class = true
end
"#;

const SETTING: &str = r#"class Setting < ApplicationRecord
end
"#;

const BOOL_SETTING: &str = r#"class BoolSetting < Setting
  attribute :value, :boolean
end
"#;

const INT_SETTING: &str = r#"class IntSetting < Setting
  attribute :value, :integer
end
"#;

const PROBE: &str = r##"class StiAttributeProbe
  def self.lines
    out = []
    plain = Setting.create!(name: "p", value: "1")
    flag = BoolSetting.create!(name: "b", value: "1")
    count = IntSetting.create!(name: "i", value: "42")
    out << "read: #{plain.value.inspect} #{flag.value.inspect} #{count.value.inspect}"
    out << "raw: #{Setting.order(:id).pluck(:value).inspect}"
    out << "sti find: #{Setting.find(flag.id).class} #{Setting.find(flag.id).value.inspect} #{Setting.find(count.id).value.inspect}"
    out << "where: #{BoolSetting.where(value: true).count} #{IntSetting.where(value: 42).count} #{Setting.where(value: "1").count}"
    flag.value = false
    flag.save!
    out << "update: #{BoolSetting.find(flag.id).value.inspect} #{Setting.where(id: flag.id).pluck(:value).inspect}"
    out << "types: #{BoolSetting.type_for_attribute(:value).type.inspect} #{Setting.type_for_attribute(:value).type.inspect}"
    out
  end
end
"##;

/// What native Rails 8.2 answers (checked on the task PostgreSQL), so two
/// identical wrong answers still fail.
const EXPECTED: [&str; 6] = [
    r#"read: "1" true 42"#,
    r#"raw: ["1", "t", "42"]"#,
    "sti find: BoolSetting true 42",
    "where: 1 1 1",
    r#"update: false ["f"]"#,
    "types: :boolean :string",
];

/// What native Rails requires, in order.
const LOADS: [&str; 5] = [
    "app/models/application_record",
    "app/models/setting",
    "app/models/bool_setting",
    "app/models/int_setting",
    "app/models/sti_attribute_probe",
];

const FILES: [(&str, &str); 9] = [
    ("roundhouse.yml", "database_adapter: postgresql\n"),
    ("config/database.yml", DATABASE_YML),
    ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
    ("db/structure.sql", STRUCTURE),
    ("app/models/application_record.rb", APPLICATION_RECORD),
    ("app/models/setting.rb", SETTING),
    ("app/models/bool_setting.rb", BOOL_SETTING),
    ("app/models/int_setting.rb", INT_SETTING),
    ("app/models/sti_attribute_probe.rb", PROBE),
];

fn overlay() -> emit_and_run::Overlay {
    FILES.iter().fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text))
}

/// `check`'s errors other than the PostgreSQL adapter notice every
/// PostgreSQL probe app carries. The subclasses' `attribute` calls are
/// claimed: they run as written at class load.
fn real_errors(errors: &[String]) -> Vec<&String> {
    errors.iter().filter(|e| !e.contains("database_adapter not supported")).collect()
}

/// The compiler side: the model's `value` consults the declared type, and
/// each subclass carries its call.
#[test]
fn sti_subclass_attribute_runs_on_the_subclass() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    let setting = std::fs::read_to_string(emitted.join("app/models/setting.rb")).unwrap();
    assert!(setting.contains(r#"self.class._attribute_deserialize("value", value)"#), "setting:\n{setting}");
    let flag = std::fs::read_to_string(emitted.join("app/models/bool_setting.rb")).unwrap();
    assert!(flag.contains("  attribute :value, :boolean\n"), "bool_setting:\n{flag}");
}

/// A subclass `attribute` call with `default:` stays refused.
#[test]
fn sti_subclass_attribute_with_a_default_is_refused() {
    let app = overlay().write(
        "app/models/bool_setting.rb",
        "class BoolSetting < Setting\n  attribute :value, :boolean, default: true\nend\n",
    );
    let (_emitted, errors) = app.emit(roundhouse::project::BuildTarget::Ruby);
    assert!(
        real_errors(&errors).iter().any(|e| e.contains("attribute") && e.contains("BoolSetting")),
        "{}",
        errors.join("\n")
    );
}

/// The model files natively and emitted, same PostgreSQL.
#[test]
#[ignore = "requires pg gem, Core's pinned Rails and ROUNDHOUSE_PG_TEST_URL for a disposable local PostgreSQL server"]
fn sti_subclass_attribute_matches_native_rails_on_postgresql() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    let (native, emitted_lines) = pg_probe::native_and_emitted(
        "sti-attribute",
        &FILES,
        &emitted,
        "rh_e2e_sti_attribute",
        "StiAttributeProbe",
        &LOADS,
    );
    assert_eq!(native, EXPECTED, "native");
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    assert_eq!(emitted_lines, native, "emitted");
}
