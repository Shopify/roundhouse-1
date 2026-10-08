//! `alias_attribute :new, :old` over a column (Core's
//! `alias_attribute :subdomain, :name`, 77 calls), against native Rails
//! 8.2 on the same local PostgreSQL database.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;
#[path = "support/pg_probe.rs"]
mod pg_probe;

const STRUCTURE: &str = r#"CREATE TABLE shops (
    id bigserial NOT NULL,
    name character varying,
    number integer,
    CONSTRAINT shops_pkey PRIMARY KEY (id)
);
"#;

const DATABASE_YML: &str = r#"default: &default
  adapter: postgresql
  encoding: unicode

test:
  <<: *default
  database: rh_e2e_alias_attribute
"#;

const APPLICATION_RECORD: &str = r#"class ApplicationRecord < ActiveRecord::Base
  self.abstract_class = true
end
"#;

const SHOP: &str = r#"class Shop < ApplicationRecord
  alias_attribute :subdomain, :name
  alias_attribute(:tracking_number, :number)

  def renamed?
    subdomain_changed?
  end

  def old_subdomain
    subdomain_was
  end
end
"#;

const PROBE: &str = r##"class AliasAttributeProbe
  def self.lines
    out = []
    s = Shop.new(subdomain: "a", tracking_number: 7)
    out << "new: #{s.name.inspect} #{s.subdomain.inspect} #{s.subdomain?} #{s.tracking_number.inspect} #{s.number.inspect}"
    s.subdomain = "b"
    out << "write: #{s.name.inspect} #{s.renamed?} #{s.old_subdomain.inspect}"
    s.save!
    out << "saved: #{s.saved_change_to_subdomain?} #{s.subdomain_previously_changed?} #{s.subdomain_previously_was.inspect} #{Shop.pluck(:name).inspect}"
    Shop.create!(subdomain: "c", tracking_number: 3)
    out << "where: #{Shop.where(subdomain: "b").count} #{Shop.find_by(subdomain: "c").number} #{Shop.where(tracking_number: 3).pluck(:subdomain).inspect} #{Shop.exists?(subdomain: "c")}"
    out << "order: #{Shop.order(subdomain: :desc).pluck(:name).inspect} #{Shop.order(:tracking_number).pluck(:tracking_number).inspect}"
    s.update!(subdomain: "d")
    out << "update: #{s.reload.name.inspect} #{Shop.where(name: "d").count}"
    out << "read: #{s.read_attribute(:subdomain).inspect} #{s[:subdomain].inspect}"
    out << "class: #{Shop.attribute_aliases.inspect} #{Shop.attribute_alias?(:subdomain)} #{Shop.attribute_alias(:subdomain).inspect}"
    out
  end
end
"##;

/// What native Rails 8.2 answers (checked on the task PostgreSQL), so two
/// identical wrong answers still fail. Not alias behavior, so not asked
/// here: a String given for the integer column (`tracking_number: "7"`,
/// as `number: "7"`) stays a String in `new`, and `attributes` omits
/// `id`.
const EXPECTED: [&str; 8] = [
    r#"new: "a" "a" true 7 7"#,
    r#"write: "b" true nil"#,
    r#"saved: true true nil ["b"]"#,
    r#"where: 1 3 ["c"] true"#,
    r#"order: ["c", "b"] [3, 7]"#,
    r#"update: "d" 1"#,
    r#"read: "d" "d""#,
    r#"class: {"subdomain" => "name", "tracking_number" => "number", "id_value" => "id"} true "name""#,
];

/// What native Rails requires, in order.
const LOADS: [&str; 3] = [
    "app/models/application_record",
    "app/models/shop",
    "app/models/alias_attribute_probe",
];

const FILES: [(&str, &str); 7] = [
    ("roundhouse.yml", "database_adapter: postgresql\n"),
    ("config/database.yml", DATABASE_YML),
    ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
    ("db/structure.sql", STRUCTURE),
    ("app/models/application_record.rb", APPLICATION_RECORD),
    ("app/models/shop.rb", SHOP),
    ("app/models/alias_attribute_probe.rb", PROBE),
];

fn overlay() -> emit_and_run::Overlay {
    FILES.iter().fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text))
}

/// `check`'s errors other than the PostgreSQL adapter notice every
/// PostgreSQL probe app carries.
fn real_errors(errors: &[String]) -> Vec<&String> {
    errors.iter().filter(|e| !e.contains("database_adapter not supported")).collect()
}

/// The model files natively and emitted, same PostgreSQL.
#[test]
#[ignore = "requires pg gem, Core's pinned Rails and ROUNDHOUSE_PG_TEST_URL for a disposable local PostgreSQL server"]
fn alias_attribute_matches_native_rails_on_postgresql() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    let (native, emitted_lines) = pg_probe::native_and_emitted(
        "alias-attribute",
        &FILES,
        &emitted,
        "rh_e2e_alias_attribute",
        "AliasAttributeProbe",
        &LOADS,
    );
    assert_eq!(native, EXPECTED, "native");
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    assert_eq!(emitted_lines, native, "emitted");
}
