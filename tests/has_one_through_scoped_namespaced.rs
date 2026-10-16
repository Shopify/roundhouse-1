//! Core's `ProductPublication`: a top-level model whose
//! `has_one :catalog, -> { where(is_not_deleted: true) }, through:
//! :publication` names no class of its own (`Catalog`, from the name,
//! is no model): the class is the source association's, `Merchandising::
//! Publication#catalog`'s `class_name: "Merchandising::Catalog"`, and the
//! scope filters it. Emitted, the reader read `Catalog.where(
//! product_publication_id: …)`: `uninitialized constant ProductPublication::
//! Catalog`. The probe runs natively on Core's pinned activerecord over
//! in-memory SQLite (tests/support/rails_oracle.rb) and emitted on CRuby,
//! and must print the same.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::process::Command;

const SCHEMA: &str = r#"ActiveRecord::Schema[8.0].define(version: 1) do
  create_table "publications", force: :cascade do |t|
    t.string "name"
  end
  create_table "catalogs", force: :cascade do |t|
    t.integer "publication_id"
    t.string "title"
    t.boolean "is_not_deleted", default: true
  end
  create_table "product_publications", force: :cascade do |t|
    t.integer "publication_id"
  end
end
"#;

const MODELS: &[(&str, &str)] = &[
    ("application_record", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n"),
    (
        "product_publication",
        "class ProductPublication < ApplicationRecord\n  belongs_to :publication, class_name: \"Merchandising::Publication\", optional: true\n  has_one :catalog, -> { where(is_not_deleted: true) }, through: :publication\nend\n",
    ),
    (
        "merchandising/publication",
        "module Merchandising\n  class Publication < ApplicationRecord\n    self.table_name = \"publications\"\n    has_one :catalog, class_name: \"Merchandising::Catalog\", inverse_of: :publication\n    has_many :product_publications, class_name: \"ProductPublication\"\n  end\nend\n",
    ),
    (
        "merchandising/catalog",
        "module Merchandising\n  class Catalog < ApplicationRecord\n    self.table_name = \"catalogs\"\n    belongs_to :publication, class_name: \"Merchandising::Publication\", optional: true\n  end\nend\n",
    ),
];

const PROBE: &str = r##"module ScopedThroughProbe
  def self.show(label)
    value = begin
      yield
    rescue StandardError => e
      [e.class.name, e.message[0, 120]]
    end
    puts "#{label}: #{value.inspect}"
  end

  def self.run
    publication = Merchandising::Publication.create!(name: "p")
    gone = Merchandising::Publication.create!(name: "g")
    Merchandising::Catalog.create!(publication_id: publication.id, title: "live")
    Merchandising::Catalog.create!(publication_id: gone.id, title: "deleted", is_not_deleted: false)
    linked = ProductPublication.create!(publication_id: publication.id)
    dropped = ProductPublication.create!(publication_id: gone.id)
    bare = ProductPublication.create!
    show("reader") { ProductPublication.find(linked.id).catalog&.title }
    show("reader class") { ProductPublication.find(linked.id).catalog.class.name }
    show("scope hides a deleted catalog") { ProductPublication.find(dropped.id).catalog }
    show("no publication") { ProductPublication.find(bare.id).catalog }
    show("unsaved owner") { ProductPublication.new.catalog }
  end
end
"##;

fn app() -> emit_and_run::Overlay {
    let mut app = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write("db/schema.rb", SCHEMA)
        .write("lib/scoped_through_probe.rb", PROBE);
    for (name, source) in MODELS {
        app = app.write(&format!("app/models/{name}.rb"), source);
    }
    app
}

fn native() -> String {
    let oracle = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/rails_oracle.rb");
    let dir = std::env::temp_dir().join(format!("rh_scoped_through_native_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("schema.rb"), SCHEMA).unwrap();
    std::fs::write(dir.join("scoped_through_probe.rb"), PROBE).unwrap();
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
         {requires}require_relative \"scoped_through_probe\"\n\
         ScopedThroughProbe.run\n"
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
fn a_scoped_has_one_through_reads_the_source_associations_namespaced_class() {
    let run = app().run_ruby("ScopedThroughProbe.run");
    run.assert_passes();
    assert_eq!(run.stdout, native(), "emitted scoped has_one through vs native activerecord");
}
