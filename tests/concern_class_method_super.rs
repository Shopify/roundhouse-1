//! A model's own class method that calls `super` reaches the Concern's
//! `class_methods` definition of the same name, as in Rails (the
//! Concern's `ClassMethods` sits behind the model's singleton).
//!
//! The emit copies a Concern's class side onto each includer, and the
//! includer's own definition replaced the copy, so Core's
//! `Delivery::MethodRateGroup.cache_key_by_id(...)` (`super` with a
//! different default `version:`) raised "super: no superclass method".
//!
//! The probe runs natively on Core's pinned activerecord over in-memory
//! SQLite (tests/support/rails_oracle.rb) and emitted on CRuby.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::process::Command;

const SCHEMA: &str = r#"ActiveRecord::Schema[8.0].define(version: 1) do
  create_table "widgets", force: :cascade do |t|
    t.string "name"
  end
end
"#;

const APPLICATION_RECORD: &str = "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n";

const CACHEABLE: &str = r##"module Cacheable
  extend ActiveSupport::Concern

  class_methods do
    def key(shop_id:, id:, scope: "", version: 1)
      "#{name}/#{shop_id}/#{id}/#{scope}/v#{version}"
    end

    def label(name, suffix = "")
      "#{name}#{suffix}"
    end

    def plain
      "plain"
    end
  end
end
"##;

const WIDGET: &str = r##"class Widget < ApplicationRecord
  include Cacheable

  class << self
    def key(shop_id:, id:, scope: "", version: 7)
      super
    end

    def label(name, suffix = "!")
      super(name.upcase, suffix)
    end
  end
end
"##;

const PROBE: &str = r##"class WidgetProbe
  def self.lines
    [
      Widget.key(shop_id: 1, id: 2),
      Widget.key(shop_id: 1, id: 2, scope: "s", version: 3),
      Widget.label("a"),
      Widget.label("a", "?"),
      Widget.plain,
    ]
  end
end
"##;

const EXPECTED: &[&str] = &[
    "Widget/1/2//v7",
    "Widget/1/2/s/v3",
    "A!",
    "A?",
    "plain",
];

const FILES: &[(&str, &str)] = &[
    ("application_record", APPLICATION_RECORD),
    ("cacheable", CACHEABLE),
    ("widget", WIDGET),
    ("widget_probe", PROBE),
];

fn app() -> emit_and_run::Overlay {
    let mut app = emit_and_run::empty_app()
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write("db/schema.rb", SCHEMA);
    for (name, source) in FILES {
        app = app.write(&format!("app/models/{name}.rb"), source);
    }
    app
}

fn expected() -> String {
    EXPECTED.iter().map(|l| format!("{l}\n")).collect()
}

fn native() -> String {
    let oracle = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/rails_oracle.rb");
    let dir = std::env::temp_dir().join(format!("rh_concern_super_native_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("schema.rb"), SCHEMA).unwrap();
    let mut requires = String::new();
    for (name, source) in FILES {
        std::fs::write(dir.join(format!("{name}.rb")), source).unwrap();
        requires.push_str(&format!("require_relative {name:?}\n"));
    }
    let program = format!(
        "require {oracle:?}\nRailsOracle.activate(\"activerecord\", \"activemodel\", \"activesupport\")\n\
         require \"active_record\"\n\
         abort \"native: activerecord is not Core's\" unless Gem.loaded_specs[\"activerecord\"].full_gem_path.start_with?(RailsOracle::CORE_GEM_HOME)\n\
         ActiveRecord::Base.establish_connection(adapter: \"sqlite3\", database: \":memory:\")\n\
         ActiveRecord::Migration.verbose = false\n\
         load File.expand_path(\"schema.rb\", __dir__)\n\
         {requires}puts WidgetProbe.lines\n"
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
fn a_model_class_method_calling_super_reaches_the_concerns_definition() {
    assert_eq!(native(), expected(), "native activerecord vs the spelled-out answers");
    let run = app().run_ruby("puts WidgetProbe.lines");
    run.assert_passes();
    assert_eq!(run.stdout, expected(), "emitted vs Rails' answers");
}
