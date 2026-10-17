//! A nil inside a `where` array is `IS NULL` beside the IN list
//! (`col IN (1, 2) OR col IS NULL`), as Rails' ArrayHandler builds it, so
//! `where.not(col: [nil, 1])` keeps the rows `col NOT IN (1) AND col IS NOT
//! NULL` keeps. A NULL left inside the list made the negation NULL for every
//! row: Core's `Delivery::Profile.app_owned` matched nothing and the
//! app-owned shipping contexts never loaded. Rows agree with native Rails
//! 8.2 on the same local PostgreSQL database.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;
#[path = "support/pg_probe.rs"]
mod pg_probe;

const STRUCTURE: &str = r#"CREATE TABLE profiles (
    id bigserial NOT NULL,
    api_client_id bigint,
    CONSTRAINT profiles_pkey PRIMARY KEY (id)
);
"#;

const DATABASE_YML: &str = r#"default: &default
  adapter: postgresql
  encoding: unicode

test:
  <<: *default
  database: rh_e2e_where_array_nil
"#;

const APPLICATION_RECORD: &str = r#"class ApplicationRecord < ActiveRecord::Base
  self.abstract_class = true
end
"#;

const PROFILE: &str = "class Profile < ApplicationRecord\n  scope :app_owned, -> { where.not(api_client_id: [nil, *SHOPIFY_CLIENT_IDS]) }\n  scope :merchant_managed, -> { where(api_client_id: [nil, SHOPIFY_CLIENT_IDS.first]) }\n  SHOPIFY_CLIENT_IDS = [10, 20].freeze\nend\n";

const PROBE: &str = r##"class WhereArrayNilProbe
  def self.lines
    out = []
    [nil, 10, 20, 30, 40].each { |client| Profile.create!(api_client_id: client) }
    out << "app_owned: #{Profile.app_owned.order(:id).pluck(:api_client_id).inspect}"
    out << "merchant_managed: #{Profile.merchant_managed.order(:id).pluck(:api_client_id).inspect}"
    out << "only nil: #{Profile.where(api_client_id: [nil]).count}"
    out << "not only nil: #{Profile.where.not(api_client_id: [nil]).count}"
    out << "list: #{Profile.where(api_client_id: [10, 30]).order(:id).pluck(:api_client_id).inspect}"
    out << "not list: #{Profile.where.not(api_client_id: [10, 30]).order(:id).pluck(:api_client_id).inspect}"
    out << "empty: #{Profile.where(api_client_id: []).count}"
    out
  end
end
"##;

const LOADS: [&str; 3] = [
    "app/models/application_record",
    "app/models/profile",
    "app/models/where_array_nil_probe",
];

const FILES: [(&str, &str); 7] = [
    ("roundhouse.yml", "database_adapter: postgresql\n"),
    ("config/database.yml", DATABASE_YML),
    ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
    ("db/structure.sql", STRUCTURE),
    ("app/models/application_record.rb", APPLICATION_RECORD),
    ("app/models/profile.rb", PROFILE),
    ("app/models/where_array_nil_probe.rb", PROBE),
];

fn overlay() -> emit_and_run::Overlay {
    FILES.iter().fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text))
}

#[test]
#[ignore = "requires pg gem, Core's pinned Rails and ROUNDHOUSE_PG_TEST_URL for a disposable local PostgreSQL server"]
fn a_nil_in_a_where_array_is_is_null_like_native_rails() {
    let (emitted, _errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    let (native, emitted_lines) = pg_probe::native_and_emitted(
        "where-array-nil",
        &FILES,
        &emitted,
        "rh_e2e_where_array_nil",
        "WhereArrayNilProbe",
        &LOADS,
    );
    // Not a vacuous agreement: Rails' own answers.
    assert_eq!(native[0], "app_owned: [30, 40]", "{native:#?}");
    assert_eq!(native[1], "merchant_managed: [nil, 10]", "{native:#?}");
    assert_eq!(native[2], "only nil: 1", "{native:#?}");
    assert_eq!(native[3], "not only nil: 4", "{native:#?}");
    assert_eq!(native[5], "not list: [20, 40]", "{native:#?}");
    assert_eq!(native[6], "empty: 0", "{native:#?}");
    assert_eq!(emitted_lines, native, "emitted");
}
