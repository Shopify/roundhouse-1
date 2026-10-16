//! Relation write/group argument forms Rails accepts and the runtime wrote
//! as Ruby inspect text into SQL: `update_all(["col = COALESCE(col, 0) + ?",
//! 1])` (Core's `CollectActiveRecordCallbacks.update_collection_products_count`,
//! `UPDATE "collections" SET ["products_count = …", 1, <Time>]`) and
//! `group([:a, "b"])` (`GROUP BY ["context_driver_sets"…`).
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::process::Command;

const SCHEMA: &str = r#"ActiveRecord::Schema[8.0].define(version: 1) do
  create_table "tallies", force: :cascade do |t|
    t.string "name"
    t.string "kind"
    t.integer "count"
  end
end
"#;

const MODELS: &[(&str, &str)] = &[
    ("application_record", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n"),
    ("tally", "class Tally < ApplicationRecord\nend\n"),
];

const PROBE: &str = r##"module RelationSqlFormsProbe
  def self.show(label)
    value = begin
      yield
    rescue StandardError => e
      [e.class.name, e.message[0, 120]]
    end
    puts "#{label}: #{value.inspect}"
  end

  def self.run
    a = Tally.create!(name: "a", kind: "x", count: 1)
    b = Tally.create!(name: "b", kind: "x")
    c = Tally.create!(name: "c", kind: "y", count: 5)
    show("update_all array binds") { Tally.where(id: [a.id, b.id]).update_all(["count = COALESCE(count, 0) + ?, name = ?", 3, "it's"]) }
    show("after") { Tally.order(:id).map { |t| [t.name, t.count] } }
    show("update_all array no binds") { Tally.where(id: c.id).update_all(["count = 0"]) }
    show("update_all named binds") { Tally.where(id: c.id).update_all(["count = :n", { n: 9 }]) }
    show("named after") { Tally.find(c.id).count }
    show("update_all string") { Tally.where(id: c.id).update_all("count = 4") }
    show("group array") { Tally.group([:kind, "name"]).order(:kind, :name).pluck(:kind, :name) }
    show("group splat") { Tally.group(:kind, "name").order(:kind, :name).pluck(:kind, :name) }
    show("group one") { Tally.group(:kind).count.sort_by { |k, _| k.to_s } }
  end
end
"##;

fn app() -> emit_and_run::Overlay {
    let mut app = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write("db/schema.rb", SCHEMA)
        .write("lib/relation_sql_forms_probe.rb", PROBE);
    for (name, source) in MODELS {
        app = app.write(&format!("app/models/{name}.rb"), source);
    }
    app
}

fn native() -> String {
    let oracle = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/rails_oracle.rb");
    let dir = std::env::temp_dir().join(format!("rh_relation_sql_forms_native_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("schema.rb"), SCHEMA).unwrap();
    std::fs::write(dir.join("relation_sql_forms_probe.rb"), PROBE).unwrap();
    let mut requires = String::new();
    for (name, source) in MODELS {
        let path = dir.join(format!("{name}.rb"));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, source).unwrap();
        requires.push_str(&format!("require_relative {name:?}\n"));
    }
    let program = format!(
        "require {oracle:?}\nRailsOracle.activate(\"activerecord\", \"activemodel\", \"activesupport\")\n\
         require \"active_record\"\n\
         ActiveRecord::Base.establish_connection(adapter: \"sqlite3\", database: \":memory:\")\n\
         ActiveRecord::Migration.verbose = false\n\
         ActiveRecord::Base.belongs_to_required_by_default = true\n\
         load File.expand_path(\"schema.rb\", __dir__)\n\
         {requires}require_relative \"relation_sql_forms_probe\"\n\
         RelationSqlFormsProbe.run\n"
    );
    std::fs::write(dir.join("native.rb"), program).unwrap();
    let out = Command::new("ruby")
        .args(["-W0", "native.rb"])
        .current_dir(&dir)
        .env("GLIBC_TUNABLES", "glibc.rtld.optional_static_tls=262144")
        .output()
        .expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "native run failed:\n{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    std::fs::remove_dir_all(&dir).unwrap();
    stdout
}

#[test]
fn update_all_takes_a_sanitized_array_and_group_flattens_its_arguments() {
    let run = app().run_ruby("RelationSqlFormsProbe.run");
    run.assert_passes();
    assert_eq!(run.stdout, native(), "emitted update_all/group argument forms vs native activerecord");
}
