//! `has_many :provinces, through: :zone_regions, source: :region,
//! source_type: "Province"` over a polymorphic `belongs_to :region`: the
//! reader joins the join table on the id AND the type column, and
//! `preload` / `includes` answer the same rows. The reader was left as a
//! plain has_many (`provinces.zone_id`, which does not exist), so Core's
//! `Delivery::Context.preload(zone: [:countries, { provinces: :country }])`
//! raised `PG::UndefinedColumn`. Rows agree with native Rails 8.2 on the
//! same local PostgreSQL database.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;
#[path = "support/pg_probe.rs"]
mod pg_probe;

const STRUCTURE: &str = r#"CREATE TABLE zones (
    id bigserial NOT NULL,
    name character varying,
    CONSTRAINT zones_pkey PRIMARY KEY (id)
);
CREATE TABLE provinces (
    id bigserial NOT NULL,
    code character varying,
    CONSTRAINT provinces_pkey PRIMARY KEY (id)
);
CREATE TABLE countries (
    id bigserial NOT NULL,
    code character varying,
    CONSTRAINT countries_pkey PRIMARY KEY (id)
);
CREATE TABLE zone_regions (
    id bigserial NOT NULL,
    zone_id bigint,
    region_id bigint,
    region_type character varying,
    CONSTRAINT zone_regions_pkey PRIMARY KEY (id)
);
"#;

const DATABASE_YML: &str = r#"default: &default
  adapter: postgresql
  encoding: unicode

test:
  <<: *default
  database: rh_e2e_polymorphic_source_through
"#;

const APPLICATION_RECORD: &str = r#"class ApplicationRecord < ActiveRecord::Base
  self.abstract_class = true
end
"#;

const ZONE: &str = "class Zone < ApplicationRecord\n  has_many :zone_regions\n  has_many :provinces, through: :zone_regions, source: :region, source_type: \"Province\"\n  has_many :countries, through: :zone_regions, source: :region, source_type: \"Country\"\nend\n";
const ZONE_REGION: &str = "class ZoneRegion < ApplicationRecord\n  belongs_to :zone\n  belongs_to :region, polymorphic: true\nend\n";
const PROVINCE: &str = "class Province < ApplicationRecord\nend\n";
const COUNTRY: &str = "class Country < ApplicationRecord\nend\n";

const PROBE: &str = r##"class PolymorphicSourceThroughProbe
  def self.lines
    out = []
    z1 = Zone.create!(name: "one")
    z2 = Zone.create!(name: "two")
    on = Province.create!(code: "ON")
    qc = Province.create!(code: "QC")
    ca = Country.create!(code: "CA")
    ZoneRegion.create!(zone: z1, region: on)
    ZoneRegion.create!(zone: z1, region: ca)
    ZoneRegion.create!(zone: z2, region: qc)
    out << "provinces: #{z1.provinces.order(:id).map(&:code).inspect}"
    out << "countries: #{z1.countries.order(:id).map(&:code).inspect}"
    out << "other zone: #{z2.provinces.map(&:code).inspect} #{z2.countries.count}"
    out << "preload: #{Zone.preload(:provinces, :countries).order(:id).map { |z| [z.provinces.map(&:code), z.countries.map(&:code)] }.inspect}"
    out << "includes: #{Zone.includes(:provinces).order(:id).map { |z| z.provinces.map(&:code) }.inspect}"
    out
  end
end
"##;

const LOADS: [&str; 6] = [
    "app/models/application_record",
    "app/models/province",
    "app/models/country",
    "app/models/zone",
    "app/models/zone_region",
    "app/models/polymorphic_source_through_probe",
];

const FILES: [(&str, &str); 11] = [
    ("roundhouse.yml", "database_adapter: postgresql\n"),
    ("config/database.yml", DATABASE_YML),
    ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
    ("db/structure.sql", STRUCTURE),
    ("app/models/application_record.rb", APPLICATION_RECORD),
    ("app/models/zone.rb", ZONE),
    ("app/models/zone_region.rb", ZONE_REGION),
    ("app/models/province.rb", PROVINCE),
    ("app/models/country.rb", COUNTRY),
    ("app/models/polymorphic_source_through_probe.rb", PROBE),
    ("test/.keep", ""),
];

fn overlay() -> emit_and_run::Overlay {
    FILES.iter().fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text))
}

#[test]
#[ignore = "requires pg gem, Core's pinned Rails and ROUNDHOUSE_PG_TEST_URL for a disposable local PostgreSQL server"]
fn a_polymorphic_source_through_reads_the_typed_rows_like_native_rails() {
    let (emitted, _errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    let (native, emitted_lines) = pg_probe::native_and_emitted(
        "polymorphic-source-through",
        &FILES,
        &emitted,
        "rh_e2e_polymorphic_source_through",
        "PolymorphicSourceThroughProbe",
        &LOADS,
    );
    // Not a vacuous agreement: Rails' own answers.
    assert_eq!(native[0], "provinces: [\"ON\"]", "{native:#?}");
    assert_eq!(native[1], "countries: [\"CA\"]", "{native:#?}");
    assert_eq!(native[2], "other zone: [\"QC\"] 0", "{native:#?}");
    assert_eq!(native[3], "preload: [[[\"ON\"], [\"CA\"]], [[\"QC\"], []]]", "{native:#?}");
    assert_eq!(emitted_lines, native, "emitted");
}
