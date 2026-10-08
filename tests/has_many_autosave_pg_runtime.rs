//! A has_many validates and saves the records it holds with its owner,
//! as Rails' AutosaveAssociation does, against Core's pinned
//! activerecord on SQLite and on the same local PostgreSQL database.
//!
//! Rails validates the target's new records (every loaded one in a
//! custom context, each in that context) and adds "Lines is invalid"
//! once; `validate: false` skips that. `_ensure_no_duplicate_errors`
//! then drops repeated errors, which only a model with a has_many runs.
//! Saving the owner saves the records it holds (all of them for a new
//! owner, the new ones otherwise) under its key, a built record holding
//! the owner through its inverse belongs_to, so a required
//! `belongs_to :order` passes before the order has an id.
//!
//! The emitted models did none of it: a built line was never saved, and
//! an unsaved order made its line fail "Order must exist".
#[path = "support/emit_and_run.rs"]
mod emit_and_run;
#[path = "support/pg_probe.rs"]
mod pg_probe;

use roundhouse::project::BuildTarget;
use std::process::Command;

const APPLICATION_RECORD: &str = "class ApplicationRecord < ActiveRecord::Base\n  primary_abstract_class\nend\n";

const ORDER: &str = r##"class Order < ApplicationRecord
  validates :name, presence: true
  has_many :lines
  has_many :notes, validate: false
  validates :name, presence: true
  validate :no_rush

  def no_rush
    errors.add(:base, "No rush") if name == "rush"
    errors.add(:base, "No rush") if name == "rush"
  end
end
"##;

const LINE: &str = r##"class Line < ApplicationRecord
  belongs_to :order
  validates :sku, presence: true
  validates :qty, numericality: { less_than: 100 }, on: :audit
end
"##;

const NOTE: &str = r##"class Note < ApplicationRecord
  belongs_to :order
  validates :body, presence: true
end
"##;

const PLAIN: &str = r##"class Plain < ApplicationRecord
  validates :name, presence: true
  validates :name, presence: true
end
"##;

const PROBE: &str = r##"class HasManyProbe
  def self.line(label, record, context = nil)
    valid = context.nil? ? record.valid? : record.valid?(context)
    "#{label}: #{valid} #{record.errors.full_messages.inspect} #{record.errors.attribute_names.inspect}"
  end

  def self.lines
    out = []
    o = Order.new(name: "n")
    o.lines.build(sku: "")
    out << line("new owner built invalid", o)
    o = Order.new(name: "n")
    o.lines << Line.new(sku: "")
    o.lines << Line.new(sku: "")
    out << line("new owner pushed two invalid", o)
    o = Order.new(name: "n")
    o.notes.build(body: "")
    out << line("validate false", o)
    out << line("dedupe", Order.new)
    out << line("dedupe base", Order.new(name: "rush"))
    out << line("no has_many no dedupe", Plain.new)
    saved = Order.create!(name: "s")
    Line.create!(order: saved, sku: "a", qty: 1)
    Line.create!(order: saved, sku: "b", qty: 2)
    saved = Order.find(saved.id)
    out << line("saved unloaded", saved)
    saved.lines.to_a[0].sku = ""
    out << line("saved loaded invalid", saved)
    out << line("saved loaded invalid create ctx", saved, :create)
    out << line("saved loaded invalid audit ctx", saved, :audit)
    saved.lines.to_a[0].sku = "a"
    saved.lines.to_a[1].qty = 500
    out << line("saved loaded audit only", saved)
    out << line("saved loaded audit ctx", saved, :audit)
    saved.lines.to_a[1].qty = 2
    saved.lines.build(sku: "")
    out << line("saved built invalid", saved)
    out << "save: #{saved.save}"
    fresh = Order.find(saved.id)
    fresh.lines.build(sku: "")
    out << line("saved unloaded built", fresh)

    base = Line.count
    o = Order.new(name: "n")
    built = o.lines.build(sku: "ok")
    out << "built inverse: #{built.order.equal?(o)} #{built.valid?}"
    o.save!
    out << "new owner build save: #{Line.count - base} #{o.lines.size} #{o.lines.first.persisted?} #{built.order_id == o.id}"
    s = Order.create!(name: "s")
    s.lines.build(sku: "b")
    s.save!
    out << "saved owner build save: #{Line.count - base} #{s.lines.size}"
    q = Order.new(name: "q")
    pushed = Line.new(sku: "p")
    q.lines << pushed
    out << line("pushed valid", q)
    q.save!
    out << "pushed save: #{Line.count - base} #{pushed.persisted?} #{pushed.order_id == q.id}"
    orders = Order.count
    r = Order.new(name: "r")
    r.lines.build(sku: "")
    out << "invalid save: #{r.save} #{Order.count - orders} #{Line.count - base}"
    t = Order.create!(name: "t")
    kept = t.lines.create!(sku: "k")
    kept.sku = "changed"
    t.name = "t2"
    t.save!
    out << "changed loaded not saved: #{Line.find(kept.id).sku}"
    n = Order.new(name: "nv")
    n.notes.build(body: "x")
    n.save!
    out << "validate false saves: #{Note.where(order_id: n.id).count}"
    out
  end
end
"##;

/// Core's activerecord on SQLite; the emitted trees must print the same
/// (the tests compare with these too, on SQLite and PostgreSQL), so two
/// identical wrong answers still fail.
const EXPECTED: &[&str] = &[
    "new owner built invalid: false [\"Lines is invalid\"] [:lines]",
    "new owner pushed two invalid: false [\"Lines is invalid\"] [:lines]",
    "validate false: true [] []",
    "dedupe: false [\"Name can't be blank\"] [:name]",
    "dedupe base: false [\"No rush\"] [:base]",
    "no has_many no dedupe: false [\"Name can't be blank\", \"Name can't be blank\"] [:name]",
    "saved unloaded: true [] []",
    "saved loaded invalid: true [] []",
    "saved loaded invalid create ctx: true [] []",
    "saved loaded invalid audit ctx: false [\"Lines is invalid\"] [:lines]",
    "saved loaded audit only: true [] []",
    "saved loaded audit ctx: false [\"Lines is invalid\"] [:lines]",
    "saved built invalid: false [\"Lines is invalid\"] [:lines]",
    "save: false",
    "saved unloaded built: false [\"Lines is invalid\"] [:lines]",
    "built inverse: true true",
    "new owner build save: 1 1 true true",
    "saved owner build save: 2 1",
    "pushed valid: true [] []",
    "pushed save: 3 true true",
    "invalid save: false 0 3",
    "changed loaded not saved: k",
    "validate false saves: 1",
];

const SCHEMA: &str = r#"ActiveRecord::Schema.define(version: 1) do
  create_table "orders", force: :cascade do |t|
    t.string "name"
  end
  create_table "plains", force: :cascade do |t|
    t.string "name"
  end
  create_table "lines", force: :cascade do |t|
    t.integer "order_id"
    t.string "sku"
    t.integer "qty"
  end
  create_table "notes", force: :cascade do |t|
    t.integer "order_id"
    t.string "body"
  end
end
"#;

const STRUCTURE: &str = r#"CREATE TABLE orders (
    id bigserial NOT NULL,
    name character varying,
    CONSTRAINT orders_pkey PRIMARY KEY (id)
);
CREATE TABLE plains (
    id bigserial NOT NULL,
    name character varying,
    CONSTRAINT plains_pkey PRIMARY KEY (id)
);
CREATE TABLE lines (
    id bigserial NOT NULL,
    order_id bigint,
    sku character varying,
    qty integer,
    CONSTRAINT lines_pkey PRIMARY KEY (id)
);
CREATE TABLE notes (
    id bigserial NOT NULL,
    order_id bigint,
    body character varying,
    CONSTRAINT notes_pkey PRIMARY KEY (id)
);
"#;

const DATABASE_YML: &str =
    "default: &default\n  adapter: postgresql\n  encoding: unicode\n\ntest:\n  <<: *default\n  database: rh_e2e_has_many_autosave\n";

const FILES: &[(&str, &str)] = &[
    ("app/models/application_record.rb", APPLICATION_RECORD),
    ("app/models/order.rb", ORDER),
    ("app/models/line.rb", LINE),
    ("app/models/note.rb", NOTE),
    ("app/models/plain.rb", PLAIN),
    ("app/models/has_many_probe.rb", PROBE),
];

/// The model files on Core's activerecord over in-memory SQLite.
fn native_sqlite() -> Vec<String> {
    let oracle = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/rails_oracle.rb");
    let dir = std::env::temp_dir().join(format!("rh_has_many_autosave_native_{}", std::process::id()));
    for (path, text) in FILES.iter().chain([&("db/schema.rb", SCHEMA)]) {
        let file = dir.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    let loads: String = FILES.iter().map(|(path, _)| format!("load {path:?}\n")).collect();
    let program = format!(
        "require {oracle:?}\nRailsOracle.activate(\"activerecord\", sqlite3: \"2.9.2\")\nrequire \"active_support/all\"\nrequire \"active_record\"\n\
         abort \"native: activerecord is not Core's\" unless Gem.loaded_specs[\"activerecord\"].full_gem_path.start_with?(RailsOracle::CORE_GEM_HOME)\n\
         ActiveRecord::Base.establish_connection(adapter: \"sqlite3\", database: \":memory:\")\n\
         ActiveRecord::Migration.verbose = false\nload \"db/schema.rb\"\n{loads}puts HasManyProbe.lines\n"
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

/// The same files emitted on the SQLite CRuby tree.
fn emitted_sqlite() -> Vec<String> {
    let app = FILES
        .iter()
        .fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text))
        .write("db/schema.rb", SCHEMA)
        .write("config/database.yml", "test:\n  adapter: sqlite3\n  database: \":memory:\"\n");
    let (dir, errors) = app.emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let script = "require File.expand_path('main', Dir.pwd)\nMain.configure_default_adapter!\nputs HasManyProbe.lines\n";
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
        std::fs::read_to_string(dir.join("app/models/order.rb")).unwrap_or_default()
    );
    stdout.lines().map(str::to_string).collect()
}

#[test]
fn has_many_validates_and_saves_its_records_like_activerecord_on_sqlite() {
    let native = native_sqlite();
    assert_eq!(native, EXPECTED, "native");
    assert_eq!(emitted_sqlite(), native, "emitted");
}

/// The model files natively and emitted, same PostgreSQL.
#[test]
#[ignore = "requires pg gem, Core's pinned Rails and ROUNDHOUSE_PG_TEST_URL for a disposable local PostgreSQL server"]
fn has_many_validates_and_saves_its_records_like_activerecord_on_postgresql() {
    let mut files = FILES.to_vec();
    files.extend([
        ("roundhouse.yml", "database_adapter: postgresql\n"),
        ("config/database.yml", DATABASE_YML),
        ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
        ("db/structure.sql", STRUCTURE),
    ]);
    let (emitted, errors) =
        files.iter().fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text)).emit(BuildTarget::Ruby);
    let real: Vec<_> = errors.iter().filter(|e| !e.contains("database_adapter not supported")).collect();
    assert!(real.is_empty(), "{}", errors.join("\n"));
    let loads: Vec<&str> = FILES.iter().map(|(path, _)| path.trim_end_matches(".rb")).collect();
    let (native, emitted) =
        pg_probe::native_and_emitted("has-many-autosave", &files, &emitted, "rh_e2e_has_many_autosave", "HasManyProbe", &loads);
    assert_eq!(native, EXPECTED, "native");
    assert_eq!(emitted, native, "emitted");
}

/// `autosave: true` also saves every changed loaded record and nests
/// their errors (`lines.sku`), which the emitted model does not: a
/// check error at the declaration, as is the plain has_many it falls
/// back to otherwise.
#[test]
fn has_many_autosave_true_is_a_check_error() {
    let order = ORDER.replace("has_many :lines\n", "has_many :lines, autosave: true\n");
    let app = FILES
        .iter()
        .fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text))
        .write("app/models/order.rb", &order)
        .write("db/schema.rb", SCHEMA);
    let (_dir, errors) = app.emit(BuildTarget::Ruby);
    let seen = "`has_many autosave: true` not supported (all targets): on `lines`: Rails validates and saves every changed loaded record with the owner";
    assert!(errors.iter().any(|e| e.contains(seen)), "{errors:#?}");
}
