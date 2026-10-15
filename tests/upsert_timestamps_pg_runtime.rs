//! `upsert` / `upsert_all` stamp created_at and updated_at on the
//! PostgreSQL CRuby tree, against native Rails 8.2 on the same local
//! PostgreSQL database (insert_all.rb, `record_timestamps`).
//!
//! A table whose timestamps are NOT NULL rejected the emitted upsert
//! (`null value in column "created_at"`): the row carried neither. On a
//! conflict updated_at moves only when a written column changed, and
//! created_at never does.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;
#[path = "support/pg_probe.rs"]
mod pg_probe;

const STRUCTURE: &str = r#"CREATE TABLE rates (
    id bigserial NOT NULL,
    code character varying NOT NULL,
    value integer,
    created_at timestamp(6) without time zone NOT NULL,
    updated_at timestamp(6) without time zone NOT NULL,
    CONSTRAINT rates_pkey PRIMARY KEY (id)
);
CREATE UNIQUE INDEX index_rates_on_code ON rates USING btree (code);
"#;

const DATABASE_YML: &str = r#"default: &default
  adapter: postgresql
  encoding: unicode

test:
  <<: *default
  database: rh_e2e_upsert_timestamps
"#;

const APPLICATION_RECORD: &str = "class ApplicationRecord < ActiveRecord::Base\n  primary_abstract_class\nend\n";

const RATE: &str = "class Rate < ApplicationRecord\nend\n";

const PROBE: &str = r##"class UpsertTimestampsProbe
  def self.lines
    out = []
    Rate.upsert({ code: "usd", value: 1 }, unique_by: :code)
    first = Rate.find_by(code: "usd")
    out << "insert: #{first.created_at.nil?} #{first.updated_at.nil?}"
    sleep 0.01
    Rate.upsert({ code: "usd", value: 1 }, unique_by: :code)
    same = Rate.find_by(code: "usd")
    out << "unchanged: #{same.updated_at == first.updated_at}"
    sleep 0.01
    Rate.upsert({ code: "usd", value: 2 }, unique_by: :code)
    moved = Rate.find_by(code: "usd")
    out << "changed: #{moved.value} created #{moved.created_at == first.created_at} touched #{moved.updated_at > first.updated_at}"
    Rate.upsert_all([{ code: "usd", value: 3 }, { code: "eur", value: 4 }], unique_by: :code)
    out << "upsert_all: #{Rate.order(:code).map { |r| [r.code, r.value, r.created_at.nil?] }.inspect}"
    stamp = Time.utc(2020, 1, 2, 3, 4, 5)
    Rate.upsert({ code: "gbp", value: 5, created_at: stamp, updated_at: stamp }, unique_by: :code)
    given = Rate.find_by(code: "gbp")
    out << "given: #{given.created_at.year} #{given.updated_at.year}"
    out
  end
end
"##;

const EXPECTED: [&str; 5] = [
    "insert: false false",
    "unchanged: true",
    "changed: 2 created true touched true",
    r#"upsert_all: [["eur", 4, false], ["usd", 3, false]]"#,
    "given: 2020 2020",
];

const FILES: [(&str, &str); 7] = [
    ("roundhouse.yml", "database_adapter: postgresql\n"),
    ("config/database.yml", DATABASE_YML),
    ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
    ("db/structure.sql", STRUCTURE),
    ("app/models/application_record.rb", APPLICATION_RECORD),
    ("app/models/rate.rb", RATE),
    ("app/models/upsert_timestamps_probe.rb", PROBE),
];

#[test]
#[ignore = "requires pg gem, Core's pinned Rails and ROUNDHOUSE_PG_TEST_URL for a disposable local PostgreSQL server"]
fn upsert_stamps_timestamps_as_natively_on_postgresql() {
    let overlay = FILES.iter().fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text));
    let (emitted, errors) = overlay.emit(roundhouse::project::BuildTarget::Ruby);
    let (native, emitted_lines) = pg_probe::native_and_emitted(
        "upsert-timestamps",
        &FILES,
        &emitted,
        "rh_e2e_upsert_timestamps",
        "UpsertTimestampsProbe",
        &["app/models/application_record", "app/models/rate", "app/models/upsert_timestamps_probe"],
    );
    assert_eq!(native, EXPECTED, "native");
    let real: Vec<_> = errors.iter().filter(|e| !e.contains("database_adapter not supported")).collect();
    assert!(real.is_empty(), "{}", errors.join("\n"));
    assert_eq!(emitted_lines, native, "emitted");
}
