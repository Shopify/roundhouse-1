//! A Range inside a `where` array is its own bound predicate ORed beside
//! the IN list and the `IS NULL` of a nil, as Rails' ArrayHandler builds it
//! (`where(ended_at: [Time.now.utc.., nil])` → `ended_at >= t OR ended_at IS
//! NULL`). The Range used to be quoted as a string inside the IN list, so
//! PostgreSQL read `'2026-10-19 ...'..` as a time zone and Core's
//! `Plus::Trial::Commitment.active` failed with `time zone "utc.." not
//! recognized`. Rows agree with native Rails 8.2 on the same local
//! PostgreSQL database.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;
#[path = "support/pg_probe.rs"]
mod pg_probe;

const STRUCTURE: &str = r#"CREATE TABLE commitments (
    id bigserial NOT NULL,
    label character varying NOT NULL,
    started_at timestamp(6) without time zone,
    ended_at timestamp(6) without time zone,
    CONSTRAINT commitments_pkey PRIMARY KEY (id)
);
"#;

const DATABASE_YML: &str = r#"default: &default
  adapter: postgresql
  encoding: unicode

test:
  <<: *default
  database: rh_e2e_where_array_range
"#;

const APPLICATION_RECORD: &str = r#"class ApplicationRecord < ActiveRecord::Base
  self.abstract_class = true
end
"#;

const COMMITMENT: &str = "class Commitment < ApplicationRecord\n  scope :active, -> { where(started_at: ..Time.utc(2026, 6, 1), ended_at: [Time.utc(2026, 6, 1).., nil]) }\nend\n";

const PROBE: &str = r##"class WhereArrayRangeProbe
  def self.lines
    out = []
    Commitment.create!(label: "open", started_at: Time.utc(2026, 1, 1), ended_at: nil)
    Commitment.create!(label: "future", started_at: Time.utc(2026, 1, 1), ended_at: Time.utc(2026, 12, 1))
    Commitment.create!(label: "ended", started_at: Time.utc(2026, 1, 1), ended_at: Time.utc(2026, 3, 1))
    Commitment.create!(label: "mid", started_at: Time.utc(2026, 1, 1), ended_at: Time.utc(2026, 4, 1))
    out << "active: #{Commitment.active.order(:id).pluck(:label).inspect}"
    out << "range and nil: #{Commitment.where(ended_at: [Time.utc(2026, 4, 1).., nil]).order(:id).pluck(:label).inspect}"
    out << "two ranges: #{Commitment.where(ended_at: [..Time.utc(2026, 3, 1), Time.utc(2026, 12, 1)..]).order(:id).pluck(:label).inspect}"
    out << "range and value: #{Commitment.where(ended_at: [Time.utc(2026, 3, 1), Time.utc(2026, 11, 1)..]).order(:id).pluck(:label).inspect}"
    out << "not range and nil: #{Commitment.where.not(ended_at: [Time.utc(2026, 4, 1).., nil]).order(:id).pluck(:label).inspect}"
    out << "ids: #{Commitment.where(id: [1, 3..4]).order(:id).pluck(:label).inspect}"
    out
  end
end
"##;

const LOADS: [&str; 3] = [
    "app/models/application_record",
    "app/models/commitment",
    "app/models/where_array_range_probe",
];

const FILES: [(&str, &str); 7] = [
    ("roundhouse.yml", "database_adapter: postgresql\n"),
    ("config/database.yml", DATABASE_YML),
    ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
    ("db/structure.sql", STRUCTURE),
    ("app/models/application_record.rb", APPLICATION_RECORD),
    ("app/models/commitment.rb", COMMITMENT),
    ("app/models/where_array_range_probe.rb", PROBE),
];

fn overlay() -> emit_and_run::Overlay {
    FILES.iter().fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text))
}

#[test]
#[ignore = "requires pg gem, Core's pinned Rails and ROUNDHOUSE_PG_TEST_URL for a disposable local PostgreSQL server"]
fn a_range_in_a_where_array_is_a_bound_predicate_like_native_rails() {
    let (emitted, _errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    let (native, emitted_lines) = pg_probe::native_and_emitted(
        "where-array-range",
        &FILES,
        &emitted,
        "rh_e2e_where_array_range",
        "WhereArrayRangeProbe",
        &LOADS,
    );
    // Not a vacuous agreement: Rails' own answers.
    assert_eq!(native[0], r#"active: ["open", "future"]"#, "{native:#?}");
    assert_eq!(native[1], r#"range and nil: ["open", "future", "mid"]"#, "{native:#?}");
    assert_eq!(native[2], r#"two ranges: ["future", "ended"]"#, "{native:#?}");
    assert_eq!(emitted_lines, native, "emitted");
}
