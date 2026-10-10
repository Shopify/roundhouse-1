//! A concern's own `<assoc>_loaded?(owner)` beside the flat predicate
//! Roundhouse synthesizes for that has_many, against Core's pinned
//! activerecord on SQLite.
//!
//! Core's `HasSyncedActiveRecordMetafields` declares `has_many
//! :metafields` on each includer from its `included` hook and asks, in
//! a private `metafields_loaded?(owner)`, `owner.metafields.loaded? ||
//! owner.metafields.target.any?`. The model's synthesized
//! `metafields_loaded?` shadowed the concern's (every callback raised
//! "given 1, expected 0"), and the concern's `.loaded?` / `.target`
//! reached the reader's Array (`lower::assoc_loaded`).
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;
use std::process::Command;

const APPLICATION_RECORD: &str = "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n";
const PRODUCT: &str = "class Product < ApplicationRecord\n  include HasMetafields\nend\n";
const LOCATION: &str = r#"class Location < ApplicationRecord
  include HasMetafields

  def ready?
    metafields.loaded?
  end
end
"#;
const METAFIELD: &str = "class Metafield < ApplicationRecord\n  belongs_to :owner, polymorphic: true\nend\n";
const CONCERN: &str = r#"module HasMetafields
  class << self
    def included(base)
      base.has_many(:metafields, as: "owner", foreign_key: "owner_id")
    end
  end

  def metafields_ready?
    owner = self
    metafields_loaded?(owner)
  end

  def loaded_values
    return [] unless metafields_loaded?(self)

    metafields.map(&:value)
  end

  private

  def metafields_loaded?(owner)
    owner.metafields.loaded? || owner.metafields.target.any?
  end
end
"#;

const PROBE: &str = r##"class LoadedProbe
  def self.lines
    out = []
    p = Product.create!(title: "p")
    Metafield.create!(owner: p, value: "a")
    l = Location.create!(name: "l")
    Metafield.create!(owner: l, value: "b")
    product = Product.find(p.id)
    out << "product before: #{product.metafields_ready?} #{product.loaded_values.inspect}"
    product.metafields.to_a
    out << "product after: #{product.metafields_ready?} #{product.loaded_values.inspect}"
    location = Location.find(l.id)
    out << "location before: #{location.ready?} #{location.metafields_ready?}"
    location.metafields.each { |m| m.value }
    out << "location after: #{location.ready?} #{location.metafields_ready?} #{location.loaded_values.inspect}"
    out
  end
end
"##;

/// What native Rails answers, so two identical wrong answers still fail.
const EXPECTED: &[&str] = &[
    "product before: false []",
    "product after: true [\"a\"]",
    "location before: false false",
    "location after: true true [\"b\"]",
];

const SCHEMA: &str = r#"ActiveRecord::Schema.define(version: 1) do
  create_table "products", force: :cascade do |t|
    t.string "title"
  end
  create_table "locations", force: :cascade do |t|
    t.string "name"
  end
  create_table "metafields", force: :cascade do |t|
    t.integer "owner_id"
    t.string "owner_type"
    t.string "value"
  end
end
"#;

fn files() -> Vec<(&'static str, &'static str)> {
    vec![
        ("app/models/application_record.rb", APPLICATION_RECORD),
        ("app/models/concerns/has_metafields.rb", CONCERN),
        ("app/models/product.rb", PRODUCT),
        ("app/models/location.rb", LOCATION),
        ("app/models/metafield.rb", METAFIELD),
        ("app/models/loaded_probe.rb", PROBE),
    ]
}

fn native_sqlite(files: &[(&str, &str)]) -> Vec<String> {
    let oracle = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/rails_oracle.rb");
    let dir = std::env::temp_dir().join(format!("rh_assoc_loaded_concern_native_{}", std::process::id()));
    for (path, text) in files.iter().chain([&("db/schema.rb", SCHEMA)]) {
        let file = dir.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    let loads: String = files.iter().map(|(path, _)| format!("load {path:?}\n")).collect();
    let program = format!(
        "require {oracle:?}\nRailsOracle.activate(\"activerecord\", sqlite3: \"2.9.2\")\nrequire \"active_support/all\"\nrequire \"active_record\"\n\
         abort \"native: activerecord is not Core's\" unless Gem.loaded_specs[\"activerecord\"].full_gem_path.start_with?(RailsOracle::CORE_GEM_HOME)\n\
         ActiveRecord::Base.establish_connection(adapter: \"sqlite3\", database: \":memory:\")\n\
         ActiveRecord::Migration.verbose = false\nload \"db/schema.rb\"\n{loads}puts LoadedProbe.lines\n"
    );
    std::fs::write(dir.join("native.rb"), program).unwrap();
    let out = Command::new("ruby")
        .arg("native.rb")
        .current_dir(&dir)
        .env("GLIBC_TUNABLES", "glibc.rtld.optional_static_tls=262144")
        .output()
        .expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "native run failed:\n{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    std::fs::remove_dir_all(&dir).unwrap();
    stdout.lines().map(str::to_string).collect()
}

fn emitted_sqlite(files: &[(&str, &str)]) -> Vec<String> {
    let app = files
        .iter()
        .fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text))
        .write("db/schema.rb", SCHEMA)
        .write("config/database.yml", "test:\n  adapter: sqlite3\n  database: \":memory:\"\n");
    let (dir, errors) = app.emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let script = "require File.expand_path('main', Dir.pwd)\nMain.configure_default_adapter!\nputs LoadedProbe.lines\n";
    let out = emit_and_run::ruby()
        .current_dir(&*dir)
        .args(["-W0", "-e", script])
        .env("BLOG_DB", ":memory:")
        .output()
        .expect("ruby");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "{stdout}\n{}\n{}",
        String::from_utf8_lossy(&out.stderr),
        std::fs::read_to_string(dir.join("app/models/has_metafields.rb")).unwrap_or_default()
    );
    stdout.lines().map(str::to_string).collect()
}

#[test]
fn concern_loaded_predicate_runs_beside_flat_predicate_on_sqlite() {
    let files = files();
    let native = native_sqlite(&files);
    assert_eq!(native, EXPECTED, "native");
    assert_eq!(emitted_sqlite(&files), native, "emitted");
}
