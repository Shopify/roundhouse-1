//! `upsert(attrs, update_only:)` on the PostgreSQL CRuby tree, against
//! native Rails 8.2 on the same local PostgreSQL database (activerecord
//! persistence.rb and insert_all.rb).
//!
//! On a conflict only the listed columns are assigned from the new row;
//! every other column keeps its stored value. Without `update_only:` all
//! non-conflict columns are assigned. The emitted runtime raised
//! `unknown keyword: :update_only`.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;
#[path = "support/pg_probe.rs"]
mod pg_probe;

const STRUCTURE: &str = r#"CREATE TABLE rates (
    id bigserial NOT NULL,
    code character varying NOT NULL,
    value integer,
    batch integer,
    note character varying,
    CONSTRAINT rates_pkey PRIMARY KEY (id)
);
CREATE UNIQUE INDEX index_rates_on_code ON rates USING btree (code);
"#;

const DATABASE_YML: &str = r#"default: &default
  adapter: postgresql
  encoding: unicode

test:
  <<: *default
  database: rh_e2e_upsert_update_only
"#;

const APPLICATION_RECORD: &str = "class ApplicationRecord < ActiveRecord::Base\n  primary_abstract_class\nend\n";

const RATE: &str = "class Rate < ApplicationRecord\nend\n";

const PROBE: &str = r##"class UpsertUpdateOnlyProbe
  def self.row
    r = Rate.find_by(code: "usd")
    [r.value, r.batch, r.note].inspect
  end

  def self.lines
    out = []
    Rate.upsert({ code: "usd", value: 1, batch: 1, note: "first" }, unique_by: :code)
    out << "insert: #{row}"
    Rate.upsert({ code: "usd", value: 2, batch: 2, note: "second" }, unique_by: :code, update_only: [:value, :batch])
    out << "update_only list: #{row}"
    Rate.upsert({ code: "usd", value: 3, batch: 3, note: "third" }, unique_by: :code, update_only: :value)
    out << "update_only symbol: #{row}"
    Rate.upsert({ code: "usd", value: 4, batch: 4, note: "fourth" }, unique_by: :code)
    out << "default: #{row}"
    Rate.upsert_all([{ code: "usd", value: 5, batch: 5, note: "fifth" }, { code: "eur", value: 6, batch: 6, note: "x" }], unique_by: :code, update_only: [:note])
    out << "upsert_all: #{row} #{Rate.count}"
    out
  end
end
"##;

const EXPECTED: [&str; 5] = [
    r#"insert: [1, 1, "first"]"#,
    r#"update_only list: [2, 2, "first"]"#,
    r#"update_only symbol: [3, 2, "first"]"#,
    r#"default: [4, 4, "fourth"]"#,
    r#"upsert_all: [4, 4, "fifth"] 2"#,
];

const FILES: [(&str, &str); 7] = [
    ("roundhouse.yml", "database_adapter: postgresql\n"),
    ("config/database.yml", DATABASE_YML),
    ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
    ("db/structure.sql", STRUCTURE),
    ("app/models/application_record.rb", APPLICATION_RECORD),
    ("app/models/rate.rb", RATE),
    ("app/models/upsert_update_only_probe.rb", PROBE),
];

#[test]
#[ignore = "requires pg gem, Core's pinned Rails and ROUNDHOUSE_PG_TEST_URL for a disposable local PostgreSQL server"]
fn upsert_update_only_assigns_only_the_listed_columns_as_natively_on_postgresql() {
    let overlay = FILES.iter().fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text));
    let (emitted, errors) = overlay.emit(roundhouse::project::BuildTarget::Ruby);
    let (native, emitted_lines) = pg_probe::native_and_emitted(
        "upsert-update-only",
        &FILES,
        &emitted,
        "rh_e2e_upsert_update_only",
        "UpsertUpdateOnlyProbe",
        &["app/models/application_record", "app/models/rate", "app/models/upsert_update_only_probe"],
    );
    assert_eq!(native, EXPECTED, "native");
    let real: Vec<_> = errors.iter().filter(|e| !e.contains("database_adapter not supported")).collect();
    assert!(real.is_empty(), "{}", errors.join("\n"));
    assert_eq!(emitted_lines, native, "emitted");
}
