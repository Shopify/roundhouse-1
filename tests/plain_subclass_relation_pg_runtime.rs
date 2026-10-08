//! Class-level relation calls on a plain (non-STI) model subclass
//! constant, against native Rails 8.2 on the same local PostgreSQL
//! database. `class BigAsset < Asset` on a table whose inheritance
//! column is off: Rails' `Querying` delegates `BigAsset.select(:id)` /
//! `.order(:id)` / `.limit(n)` / `.pluck` / `.first(n)` to `all`, a
//! relation on the subclass, so the rows load as BigAsset and a
//! subclass method answers on them. The emitted tree reached the runtime
//! Base, which has no class-side `select` / `order`, and raised
//! NoMethodError.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;
#[path = "support/pg_probe.rs"]
mod pg_probe;

const STRUCTURE: &str = r#"CREATE TABLE assets (
    id bigserial NOT NULL,
    type character varying,
    name character varying,
    CONSTRAINT assets_pkey PRIMARY KEY (id)
);

CREATE TABLE tiles (
    id bigserial NOT NULL,
    type character varying,
    name character varying,
    CONSTRAINT tiles_pkey PRIMARY KEY (id)
);
"#;

const DATABASE_YML: &str = r#"default: &default
  adapter: postgresql
  encoding: unicode

test:
  <<: *default
  database: rh_e2e_plain_subclass_relation
"#;

const APPLICATION_RECORD: &str = r#"class ApplicationRecord < ActiveRecord::Base
  self.abstract_class = true
end
"#;

const ASSET: &str = r#"class Asset < ApplicationRecord
  self.inheritance_column = nil

  scope :named, ->(n) { where(name: n) }
end
"#;

const BIG_ASSET: &str = r#"class BigAsset < Asset
  def label
    "big #{name}"
  end
end
"#;

/// Two levels below the model.
const HUGE_ASSET: &str = r#"class HugeAsset < BigAsset
end
"#;

const TILE: &str = r#"class Tile < ApplicationRecord
  self.inheritance_column = :_type_disabled
end
"#;

const WIDE_TILE: &str = r#"class WideTile < Tile
end
"#;

const PROBE: &str = r##"class PlainSubclassProbe
  def self.lines
    out = []
    Asset.create!(name: "a", type: "png")
    BigAsset.create!(name: "b", type: "jpg")
    BigAsset.create!(name: "c", type: "gif")
    out << "select: #{BigAsset.select(:id, :name).order(:id).map { |a| [a.class.name, a.name] }.inspect}"
    out << "order: #{BigAsset.order(:id).map(&:label).join(",")} #{BigAsset.order(id: :desc).first.label} #{BigAsset.order(:id).last.class.name}"
    out << "chain: #{BigAsset.order(:name).where(type: "gif").map(&:label).join(",")} #{BigAsset.select(:id).order(:id).count} #{BigAsset.order(:id).named("b").map(&:label).join(",")}"
    out << "scope: #{BigAsset.named("b").map(&:label).join(",")} #{BigAsset.named("c").order(:id).first.class.name}"
    out << "limit: #{BigAsset.limit(2).order(:id).map(&:label).join(",")} #{BigAsset.offset(1).order(:id).limit(1).first.label}"
    out << "terminals: #{BigAsset.pluck(:name).sort.join(",")} #{BigAsset.ids.size} #{BigAsset.first(2).map(&:label).join(",")} #{BigAsset.last(1).map(&:class).map(&:name).join(",")}"
    out << "deeper: #{HugeAsset.order(:id).map { |a| a.class.name }.join(",")} #{HugeAsset.select(:name).order(:name).first.label}"
    out << "parent: #{Asset.order(:id).map { |a| a.class.name }.join(",")} #{Asset.select(:id).order(:id).first.class.name}"
    Tile.create!(name: "t", type: "x")
    WideTile.create!(name: "w")
    out << "tile: #{WideTile.order(:id).map { |t| t.class.name }.join(",")} #{WideTile.select(:id, :name).order(name: :desc).first.name} #{WideTile.where(name: "w").order(:id).first.class.name}"
    out
  end
end
"##;

/// What native Rails 8.2 answers (checked on the task PostgreSQL), so two
/// identical wrong answers still fail.
const EXPECTED: [&str; 9] = [
    r#"select: [["BigAsset", "a"], ["BigAsset", "b"], ["BigAsset", "c"]]"#,
    r#"order: big a,big b,big c big c BigAsset"#,
    r#"chain: big c 3 big b"#,
    r#"scope: big b BigAsset"#,
    r#"limit: big a,big b big b"#,
    r#"terminals: a,b,c 3 big a,big b BigAsset"#,
    r#"deeper: HugeAsset,HugeAsset,HugeAsset big a"#,
    r#"parent: Asset,Asset,Asset Asset"#,
    r#"tile: WideTile,WideTile w WideTile"#,
];

/// What native Rails requires, in order.
const LOADS: [&str; 7] = [
    "app/models/application_record",
    "app/models/asset",
    "app/models/big_asset",
    "app/models/huge_asset",
    "app/models/tile",
    "app/models/wide_tile",
    "app/models/plain_subclass_probe",
];

const FILES: [(&str, &str); 11] = [
    ("roundhouse.yml", "database_adapter: postgresql\n"),
    ("config/database.yml", DATABASE_YML),
    (
        "config/routes.rb",
        "Rails.application.routes.draw do\nend\n",
    ),
    ("db/structure.sql", STRUCTURE),
    ("app/models/application_record.rb", APPLICATION_RECORD),
    ("app/models/asset.rb", ASSET),
    ("app/models/big_asset.rb", BIG_ASSET),
    ("app/models/huge_asset.rb", HUGE_ASSET),
    ("app/models/tile.rb", TILE),
    ("app/models/wide_tile.rb", WIDE_TILE),
    ("app/models/plain_subclass_probe.rb", PROBE),
];

fn overlay() -> emit_and_run::Overlay {
    FILES
        .iter()
        .fold(emit_and_run::empty_app(), |app, (path, text)| {
            app.write(path, text)
        })
}

/// `check`'s errors other than the PostgreSQL adapter notice every
/// PostgreSQL probe app carries.
fn real_errors(errors: &[String]) -> Vec<&String> {
    errors
        .iter()
        .filter(|e| !e.contains("database_adapter not supported"))
        .collect()
}

/// The emitted relation is seeded on the subclass, not left on Base.
#[test]
fn a_plain_subclass_chain_seeds_a_relation_on_the_subclass() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    let probe =
        std::fs::read_to_string(emitted.join("app/models/plain_subclass_probe.rb")).unwrap();
    for seeded in [
        "ActiveRecord::Relation.new(BigAsset).select(:id, :name)",
        "ActiveRecord::Relation.new(BigAsset).order(:id)",
        "ActiveRecord::Relation.new(HugeAsset).order(:id)",
        "ActiveRecord::Relation.new(WideTile).order(:id)",
        "ActiveRecord::Relation.new(Asset).order(:id)",
    ] {
        assert!(probe.contains(seeded), "missing {seeded}:\n{probe}");
    }
    // The inherited scope stays a class-method call on the subclass.
    assert!(probe.contains("BigAsset.named(\"b\")"), "{probe}");
}

/// The model files natively and emitted, same PostgreSQL.
#[test]
#[ignore = "requires pg gem, Core's pinned Rails and ROUNDHOUSE_PG_TEST_URL for a disposable local PostgreSQL server"]
fn plain_subclass_relations_match_native_rails_on_postgresql() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    let (native, emitted_lines) = pg_probe::native_and_emitted(
        "plain-subclass-relation",
        &FILES,
        &emitted,
        "rh_e2e_plain_subclass_relation",
        "PlainSubclassProbe",
        &LOADS,
    );
    assert_eq!(native, EXPECTED, "native");
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    assert_eq!(emitted_lines, native, "emitted");
}
