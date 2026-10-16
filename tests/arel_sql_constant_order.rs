//! `order(CONST)` where `CONST = Arel.sql("COALESCE(rank, 0) ASC, id ASC")`
//! (Core's `PointOfSaleDevice::CREATION_ORDER`). Rails takes an `Arel.sql`
//! literal past its column-name check however it reaches `order`; the
//! call-site-only rewrite handled `order(Arel.sql("…"))` but not a
//! constant, whose fragment then failed the column-name check.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::process::Command;

const SCHEMA: &str = r#"ActiveRecord::Schema[8.0].define(version: 1) do
  create_table "devices", force: :cascade do |t|
    t.string "name"
    t.integer "rank"
  end
end
"#;

const MODELS: &[(&str, &str)] = &[
    ("application_record", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n"),
    (
        "device",
        "class Device < ApplicationRecord\n  CREATION_ORDER = Arel.sql(\"COALESCE(devices.rank, 0) ASC, devices.id ASC\")\n  scope :by_creation, -> { order(CREATION_ORDER) }\n\n  def self.first_in_creation_order\n    order(CREATION_ORDER).first\n  end\nend\n",
    ),
];

const PROBE: &str = r##"module ArelSqlConstantOrderProbe
  def self.show(label)
    value = begin
      yield
    rescue StandardError => e
      [e.class.name, e.message[0, 120]]
    end
    puts "#{label}: #{value.inspect}"
  end

  def self.run
    Device.create!(name: "unranked-a")
    Device.create!(name: "ranked-2", rank: 2)
    Device.create!(name: "ranked-1", rank: 1)
    Device.create!(name: "unranked-b")
    show("class scope") { Device.by_creation.map(&:name) }
    show("relation order") { Device.where.not(name: "ranked-1").order(Device::CREATION_ORDER).map(&:name) }
    show("reorder") { Device.order(:name).reorder(Device::CREATION_ORDER).map(&:name) }
    show("first") { Device.first_in_creation_order.name }
    show("pick") { Device.order(Device::CREATION_ORDER).pick(:name) }
    show("literal") { Device.order(Arel.sql("devices.rank DESC, devices.id ASC")).map(&:name) }
  end
end
"##;

fn app() -> emit_and_run::Overlay {
    let mut app = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write("db/schema.rb", SCHEMA)
        .write("lib/arel_sql_constant_order_probe.rb", PROBE);
    for (name, source) in MODELS {
        app = app.write(&format!("app/models/{name}.rb"), source);
    }
    app
}

fn native() -> String {
    let oracle = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/rails_oracle.rb");
    let dir = std::env::temp_dir().join(format!("rh_arel_sql_constant_order_native_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("schema.rb"), SCHEMA).unwrap();
    std::fs::write(dir.join("arel_sql_constant_order_probe.rb"), PROBE).unwrap();
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
         {requires}require_relative \"arel_sql_constant_order_probe\"\n\
         ArelSqlConstantOrderProbe.run\n"
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
fn an_arel_sql_constant_is_an_order_fragment_not_a_column_name() {
    let run = app().run_ruby("ArelSqlConstantOrderProbe.run");
    run.assert_passes();
    assert_eq!(run.stdout, native(), "emitted constant Arel.sql order vs native activerecord");
}
