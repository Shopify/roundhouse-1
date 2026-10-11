//! The singular association's builders (`emit::ruby::library::
//! apply_singular_association_builders`, runtime/spinel/
//! active_record_associations.rb): Rails defines `build_<name>`,
//! `create_<name>`, `create_<name>!`, `reload_<name>` and `reset_<name>`
//! per `has_one` and monomorphic `belongs_to` (associations/builder/
//! singular_association.rb). The call-site rewrite covers a literal
//! keyword hash on a typed receiver; the probe calls them with a hash
//! held in a variable and merged, from another class, on a belongs_to,
//! over a has_one with `dependent:` and an inverse.
//!
//! The probe runs natively on Core's pinned activerecord over in-memory
//! SQLite (tests/support/rails_oracle.rb) and emitted on CRuby, and
//! must print the same.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::process::Command;

const SCHEMA: &str = r#"ActiveRecord::Schema[8.0].define(version: 1) do
  create_table "regions", force: :cascade do |t|
    t.string "name"
  end
  create_table "orders", force: :cascade do |t|
    t.string "number"
    t.integer "region_id"
  end
  create_table "attributions", force: :cascade do |t|
    t.string "source"
    t.integer "order_id"
  end
  create_table "notes", force: :cascade do |t|
    t.string "body"
    t.integer "order_id"
  end
end
"#;

const MODELS: &[(&str, &str)] = &[
    ("application_record", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n"),
    ("region", "class Region < ApplicationRecord\nend\n"),
    (
        "order",
        "class Order < ApplicationRecord\n  belongs_to :region, optional: true\n  has_one :attribution, dependent: :destroy, inverse_of: :order\n  has_one :note\nend\n",
    ),
    ("attribution", "class Attribution < ApplicationRecord\n  belongs_to :order, inverse_of: :attribution\nend\n"),
    ("note", "class Note < ApplicationRecord\n  belongs_to :order\nend\n"),
];

const PROBE: &str = r##"class OrderMaker
  def initialize(order)
    @order = order
  end

  def attach(params)
    @order.build_attribution(params.merge(source: "merged"))
  end
end

module BuildersProbe
  def self.show(label)
    value = begin
      yield
    rescue StandardError => e
      [e.class.name, e.message]
    end
    puts "#{label}: #{value.inspect}"
  end

  def self.run
    order = Order.create!(number: "A1")
    params = { source: "web" }
    show("build from another class") do
      built = OrderMaker.new(order).attach(params)
      [built.class.name, built.source, built.order_id == order.id, built.persisted?, order.attribution.equal?(built)]
    end
    show("build inverse") { order.attribution.order.equal?(order) }
    show("build reader loaded") { order.association(:attribution).loaded? }
    show("create replaces and destroys the old") do
      old = order.attribution
      old.save!
      fresh = order.create_attribution(source: "second")
      [fresh.persisted?, Attribution.exists?(old.id), Attribution.count, order.attribution.source]
    end
    show("create! with a hash variable") { order.create_attribution!(params).source }
    show("build on a new owner") do
      o = Order.new(number: "B1")
      b = o.build_note(body: "n")
      [b.order_id, b.persisted?, o.note.body]
    end
    show("nullify the old") do
      first = order.create_note(body: "one")
      order.create_note(body: "two")
      [Note.find(first.id).order_id, Note.where(order_id: order.id).pluck(:body)]
    end
    show("create! on a new owner") { Order.new(number: "D1").create_attribution!(source: "x").persisted? }
    show("create on a new owner") { Order.new(number: "D2").create_note(body: "x").persisted? }
    show("belongs_to build") do
      o = Order.new(number: "C1")
      r = o.build_region(name: "north")
      [r.persisted?, o.region.equal?(r), o.region_id]
    end
    show("belongs_to create") do
      o = Order.new(number: "C2")
      r = o.create_region(name: "south")
      [r.persisted?, o.region_id == r.id, o.region.name]
    end
    show("reload") do
      o = Order.find(order.id)
      o.note
      Note.where(order_id: order.id).update_all(body: "changed")
      [o.note.body, o.reload_note.body]
    end
    show("reset") do
      o = Order.find(order.id)
      o.note
      [o.reset_note, o.association(:note).loaded?]
    end
  end
end
"##;

fn app() -> emit_and_run::Overlay {
    let mut app = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write("db/schema.rb", SCHEMA)
        .write("lib/builders_probe.rb", PROBE);
    for (name, source) in MODELS {
        app = app.write(&format!("app/models/{name}.rb"), source);
    }
    app
}

fn native() -> String {
    let oracle = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/rails_oracle.rb");
    let dir = std::env::temp_dir().join(format!("rh_singular_builders_native_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("schema.rb"), SCHEMA).unwrap();
    std::fs::write(dir.join("builders_probe.rb"), PROBE).unwrap();
    let mut requires = String::new();
    for (name, source) in MODELS {
        std::fs::write(dir.join(format!("{name}.rb")), source).unwrap();
        requires.push_str(&format!("require_relative {name:?}\n"));
    }
    let program = format!(
        "require {oracle:?}\nRailsOracle.activate(\"activerecord\", \"activemodel\", \"activesupport\")\n\
         require \"active_record\"\n\
         abort \"native: activerecord is not Core's\" unless Gem.loaded_specs[\"activerecord\"].full_gem_path.start_with?(RailsOracle::CORE_GEM_HOME)\n\
         ActiveRecord::Base.establish_connection(adapter: \"sqlite3\", database: \":memory:\")\n\
         ActiveRecord::Migration.verbose = false\n\
         ActiveRecord::Base.belongs_to_required_by_default = true\n\
         load File.expand_path(\"schema.rb\", __dir__)\n\
         {requires}require_relative \"builders_probe\"\n\
         BuildersProbe.run\n"
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
fn singular_builders_run_like_rails_for_any_call_shape() {
    let run = app().run_ruby("BuildersProbe.run");
    run.assert_passes();
    assert_eq!(run.stdout, native(), "emitted builders vs native activerecord");
}
