//! A scope body reading the relation's `klass` (Core's `Responder.
//! matching_driver_ids` asks `Registry.from_model(klass)`) sees the model
//! class, as native Rails 8.2 does on the same local PostgreSQL database.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;
#[path = "support/pg_probe.rs"]
mod pg_probe;

const STRUCTURE: &str = r#"CREATE TABLE catalogs (
    id bigserial NOT NULL,
    shop_id bigint NOT NULL,
    created_at timestamp(6) without time zone NOT NULL,
    updated_at timestamp(6) without time zone NOT NULL,
    CONSTRAINT catalogs_pkey PRIMARY KEY (id)
);
"#;

const DATABASE_YML: &str = r#"default: &default
  adapter: postgresql
  encoding: unicode

test:
  <<: *default
  database: rh_e2e_scope_klass
"#;

const APPLICATION_RECORD: &str = r#"class ApplicationRecord < ActiveRecord::Base
  self.abstract_class = true
end
"#;

const CATALOG: &str = r#"class Catalog < ApplicationRecord
  scope :for_shop_of, ->(shop_id) {
    kind = klass.name.downcase
    where(shop_id: shop_id).where.not(id: kind.length)
  }
end
"#;

const PROBE: &str = r##"class ScopeKlassProbe
  def self.lines
    out = []
    now = Time.now
    Catalog.create!(shop_id: 1, created_at: now, updated_at: now)
    Catalog.create!(shop_id: 1, created_at: now, updated_at: now)
    Catalog.create!(shop_id: 2, created_at: now, updated_at: now)
    out << "scope: #{Catalog.for_shop_of(1).count}"
    out << "chained: #{Catalog.where(shop_id: 1).for_shop_of(1).count}"
    out
  end
end
"##;

const EXPECTED: [&str; 2] = ["scope: 2", "chained: 2"];

const LOADS: [&str; 3] = ["app/models/application_record", "app/models/catalog", "app/models/scope_klass_probe"];

const FILES: [(&str, &str); 6] = [
    ("roundhouse.yml", "database_adapter: postgresql\n"),
    ("config/database.yml", DATABASE_YML),
    ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
    ("db/structure.sql", STRUCTURE),
    ("app/models/application_record.rb", APPLICATION_RECORD),
    ("app/models/catalog.rb", CATALOG),
];

fn files() -> Vec<(&'static str, &'static str)> {
    let mut all = FILES.to_vec();
    all.push(("app/models/scope_klass_probe.rb", PROBE));
    all
}

fn real_errors(errors: &[String]) -> Vec<&String> {
    errors.iter().filter(|e| !e.contains("database_adapter not supported")).collect()
}

#[test]
#[ignore = "requires pg gem, Core's pinned Rails and ROUNDHOUSE_PG_TEST_URL for a disposable local PostgreSQL server"]
fn scope_body_klass_reads_the_model_class_like_native_rails_on_postgresql() {
    let files = files();
    let overlay = files.iter().fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text));
    let (emitted, errors) = overlay.emit(roundhouse::project::BuildTarget::Ruby);
    let (native, emitted_lines) =
        pg_probe::native_and_emitted("scope-klass", &files, &emitted, "rh_e2e_scope_klass", "ScopeKlassProbe", &LOADS);
    assert_eq!(native, EXPECTED, "native");
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    assert_eq!(emitted_lines, native, "emitted");
}
