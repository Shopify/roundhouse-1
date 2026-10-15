//! `validates ..., numericality:` reads the attribute's raw value, as
//! ActiveModel's `prepare_value_for_validation` does (the
//! `<attr>_before_type_cast` of a record that came from the user, the
//! `read_attribute` of one that did not), not the reader: a model that
//! redefines a column's reader (Core's `money_bags` returns a MoneyBag
//! from `total_discounts`) still validates the decimal underneath. Core's
//! Order failed every `update!` with "Total discounts isn't a number"
//! because the check read the reader's MoneyBag.
//!
//! The probe runs natively on Core's pinned activerecord over in-memory
//! SQLite (tests/support/rails_oracle.rb) and emitted on CRuby; each must
//! print Rails' answers.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::process::Command;

const SCHEMA: &str = r#"ActiveRecord::Schema[8.0].define(version: 1) do
  create_table "ledgers", force: :cascade do |t|
    t.decimal "total", precision: 21, scale: 3
    t.integer "pages"
  end
end
"#;

const APPLICATION_RECORD: &str = "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n";

/// A concern whose reader wraps the column in a non-numeric value, as
/// `money_bags` does, and a reader the model itself redefines.
const WRAPPED: &str = r##"module Wrapped
  Bag = Struct.new(:amount)

  def total
    value = super
    value.nil? ? nil : Bag.new(value)
  end
end
"##;

const LEDGER: &str = r##"class Ledger < ApplicationRecord
  include Wrapped

  validates :total, numericality: { greater_than: -1_000, less_than: 1_000, allow_nil: true }
  validates :pages, numericality: { only_integer: true, allow_nil: true }

  def pages
    value = super
    value.nil? ? nil : "#{value} pages"
  end
end
"##;

const PROBE: &str = r##"class LedgerProbe
  def self.show(out, label, ledger)
    ledger.valid?
    out << "#{label}: #{ledger.errors.full_messages.inspect}"
  end

  def self.lines
    out = []
    show(out, "blank", Ledger.new)
    show(out, "new in range", Ledger.new(total: 12, pages: 3))
    show(out, "new too big", Ledger.new(total: 5000, pages: 3))
    show(out, "new not a number", Ledger.new(total: "abc"))
    show(out, "new fractional pages", Ledger.new(pages: "2.5"))
    saved = Ledger.create!(total: 7, pages: 4)
    loaded = Ledger.find(saved.id)
    show(out, "loaded", loaded)
    loaded.pages = 9
    show(out, "loaded changed", loaded)
    loaded.total = 10_000
    show(out, "loaded too big", loaded)
    out
  end
end
"##;

const EXPECTED: &[&str] = &[
    r#"blank: []"#,
    r#"new in range: []"#,
    r#"new too big: ["Total must be less than 1000"]"#,
    r#"new not a number: ["Total is not a number"]"#,
    r#"new fractional pages: ["Pages must be an integer"]"#,
    r#"loaded: []"#,
    r#"loaded changed: []"#,
    r#"loaded too big: ["Total must be less than 1000"]"#,
];

const FILES: &[(&str, &str)] = &[
    ("application_record", APPLICATION_RECORD),
    ("wrapped", WRAPPED),
    ("ledger", LEDGER),
    ("ledger_probe", PROBE),
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
    let dir = std::env::temp_dir().join(format!("rh_numericality_raw_native_{}", std::process::id()));
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
         {requires}puts LedgerProbe.lines\n"
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
fn numericality_validates_the_raw_value_not_a_redefined_reader() {
    assert_eq!(native(), expected(), "native activerecord vs the spelled-out answers");
    let run = app().run_ruby("puts LedgerProbe.lines");
    run.assert_passes();
    assert_eq!(run.stdout, expected(), "emitted vs Rails' answers");
}
