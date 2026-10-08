//! A model on a table with a composite primary key that declares no
//! `self.primary_key` (Core's Yugabyte tables, `PRIMARY KEY((shop_id)
//! HASH, id ASC, island ASC)`), against native Rails 8.2 on the same
//! local PostgreSQL database.
//!
//! Rails derives the key from the schema: `primary_key` is the column
//! list, `id` the tuple of their values, `find` takes tuples, and update,
//! destroy, reload and `first` key on every column
//! (`query_constraints_list` is the primary key). `orders` holds two rows
//! with the same `id` under different shops, so keying on `id` alone
//! would read, update or delete the wrong one.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;
#[path = "support/pg_probe.rs"]
mod pg_probe;

const STRUCTURE: &str = r#"CREATE TABLE orders (
    shop_id bigint NOT NULL,
    id bigint NOT NULL,
    island integer DEFAULT 0 NOT NULL,
    name character varying,
    CONSTRAINT orders_pkey PRIMARY KEY (shop_id, id, island)
);
"#;

const DATABASE_YML: &str = r#"default: &default
  adapter: postgresql
  encoding: unicode

test:
  <<: *default
  database: rh_e2e_composite_primary_key
"#;

const APPLICATION_RECORD: &str = r#"class ApplicationRecord < ActiveRecord::Base
  self.abstract_class = true
end
"#;

const ORDER: &str = r#"class Order < ApplicationRecord
end
"#;

const PROBE: &str = r##"class CompositePrimaryKeyProbe
  def self.lines
    out = []
    out << "key: #{Order.primary_key.inspect} #{Order.composite_primary_key?} #{Order.query_constraints_list.inspect}"
    a = Order.create!(id: [1, 10, 0], name: "a")
    b = Order.create!(shop_id: 2, id_value: 10, name: "b")
    out << "id: #{a.id.inspect} #{a.id_value.inspect} #{a[:id].inspect} #{a.to_param.inspect} #{a.attributes["id"].inspect} #{a.cache_key.inspect}"
    out << "find: #{Order.find([2, 10, 0]).name} #{Order.find([[1, 10, 0], [2, 10, 0]]).map(&:name).inspect}"
    out << "column: #{Order.where(id: 10).count} #{Order.find_by(id: 10, shop_id: 2).name} #{Order.ids.inspect} #{Order.order(:shop_id).pluck(:id).inspect}"
    a.name = "a2"
    a.save!
    out << "update: #{Order.where(shop_id: 1).pluck(:name).inspect} #{Order.where(shop_id: 2).pluck(:name).inspect}"
    Order.where(shop_id: 2).update_all(name: "b2")
    out << "reload: #{b.reload.name} #{a.reload.name}"
    out << "first: #{Order.first.name} #{Order.last.name}"
    c = Order.new(name: "c")
    c.id = [3, 11, 1]
    c.save!
    out << "assign: #{c.shop_id} #{c.id_value} #{c.island} #{Order.find([3, 11, 1]).name}"
    b.destroy
    out << "destroy: #{Order.count} #{Order.order(:shop_id).pluck(:name).inspect}"
    out
  end
end
"##;

/// What native Rails 8.2 answers (checked on the task PostgreSQL), so two
/// identical wrong answers still fail.
const EXPECTED: [&str; 9] = [
    r#"key: ["shop_id", "id", "island"] true ["shop_id", "id", "island"]"#,
    r#"id: [1, 10, 0] 10 10 "1_10_0" 10 "orders/[1, 10, 0]""#,
    r#"find: b ["a", "b"]"#,
    "column: 2 b [[1, 10, 0], [2, 10, 0]] [10, 10]",
    r#"update: ["a2"] ["b"]"#,
    "reload: b2 a2",
    "first: a2 b2",
    "assign: 3 11 1 c",
    r#"destroy: 2 ["a2", "c"]"#,
];

/// What native Rails requires, in order.
const LOADS: [&str; 3] = ["app/models/application_record", "app/models/order", "app/models/composite_primary_key_probe"];

const FILES: [(&str, &str); 7] = [
    ("roundhouse.yml", "database_adapter: postgresql\n"),
    ("config/database.yml", DATABASE_YML),
    ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
    ("db/structure.sql", STRUCTURE),
    ("app/models/application_record.rb", APPLICATION_RECORD),
    ("app/models/order.rb", ORDER),
    ("app/models/composite_primary_key_probe.rb", PROBE),
];

fn overlay() -> emit_and_run::Overlay {
    FILES.iter().fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text))
}

/// `check`'s errors other than the PostgreSQL adapter notice every
/// PostgreSQL probe app carries.
fn real_errors(errors: &[String]) -> Vec<&String> {
    errors.iter().filter(|e| !e.contains("database_adapter not supported")).collect()
}

/// The compiler side: `id` is the tuple over the key columns, the `id`
/// column `id_value`, and the class answers the column list.
#[test]
fn composite_primary_key_compiles_to_the_tuple() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    let order = std::fs::read_to_string(emitted.join("app/models/order.rb")).unwrap();
    for expected in [
        // `id` in the model's GeneratedAttributeMethods module.
        "    def id\n      [self.shop_id, self.id_value, self.island]\n    end\n",
        "  def id_value\n    @id\n  end\n",
        "  def self.primary_key\n    [\"shop_id\", \"id\", \"island\"]\n  end\n",
        r#"WHERE \"shop_id\" = " + Db.escape_int(@shop_id) + " AND \"id\" = " + Db.escape_int(@id) + " AND \"island\" = "#,
    ] {
        assert!(order.contains(expected), "missing {expected:?} in:\n{order}");
    }
}

/// The model files natively and emitted, same PostgreSQL.
#[test]
#[ignore = "requires pg gem, Core's pinned Rails and ROUNDHOUSE_PG_TEST_URL for a disposable local PostgreSQL server"]
fn composite_primary_key_matches_native_rails_on_postgresql() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    let (native, emitted_lines) = pg_probe::native_and_emitted(
        "composite-primary-key",
        &FILES,
        &emitted,
        "rh_e2e_composite_primary_key",
        "CompositePrimaryKeyProbe",
        &LOADS,
    );
    assert_eq!(native, EXPECTED, "native");
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    assert_eq!(emitted_lines, native, "emitted");
}
