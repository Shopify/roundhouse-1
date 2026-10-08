//! Zeitwerk's `Rails.autoloaders.main.on_load("Cpath") { … }` in a boot
//! initializer (Core's components/shop_identity/config/initializers/
//! application_record.rb: `ApplicationRecord.include(
//! BelongsToShopThroughAssociation)`), against native Zeitwerk 2.8.2 and
//! Rails 8.2 on the same local PostgreSQL database.
//!
//! Zeitwerk runs a constant's callbacks right after its file loads
//! (loader/callbacks.rb:9). An include into ApplicationRecord therefore
//! lands after its body and before any subclass body, which is what makes
//! the concern's class macro (`shouts`) callable from `class Shop <
//! ApplicationRecord`: ingest lifts it to the end of ApplicationRecord's
//! body (`load_hook_includes::lift_autoload_includes`). A callback that
//! does anything else runs through runtime/spinel/rails_autoloaders.rb
//! when the emitted file of its constant reports its load.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;
#[path = "support/pg_probe.rs"]
mod pg_probe;

const STRUCTURE: &str = r#"CREATE TABLE shops (
    id bigserial NOT NULL,
    name character varying,
    CONSTRAINT shops_pkey PRIMARY KEY (id)
);
"#;

const DATABASE_YML: &str = r#"default: &default
  adapter: postgresql
  encoding: unicode

test:
  <<: *default
  database: rh_e2e_autoload_on_load
"#;

const APPLICATION_RECORD: &str = r#"class ApplicationRecord < ActiveRecord::Base
  self.abstract_class = true
end
"#;

/// The concern the initializer includes: a class macro whose body
/// declares an alias (macro_replay) and an instance method.
const SHOUTING: &str = r#"module Shouting
  extend ActiveSupport::Concern

  class_methods do
    def shouts(new_name, column)
      alias_attribute(new_name, column)
    end
  end

  def loud_name
    name.upcase
  end
end
"#;

const SHOP: &str = r#"class Shop < ApplicationRecord
  shouts :handle, :name
end
"#;

const CALL_LOG: &str = r#"class CallLog
  def self.lines
    @lines ||= []
  end

  def self.add(line)
    lines << line
  end
end
"#;

const INITIALIZER: &str = r#"Rails.autoloaders.main.on_load("ApplicationRecord") do
  ApplicationRecord.include(Shouting)
end
"#;

/// A callback that is no include: it runs when CallLog's file loads.
const LOG_INITIALIZER: &str = r#"Rails.autoloaders.main.on_load("CallLog") do |klass, _abspath|
  klass.add("loaded #{klass.name}")
end
"#;

const PROBE: &str = r##"class AutoloadOnLoadProbe
  def self.lines
    out = []
    out << "before: #{CallLog.lines.inspect}"
    CallLog.add("after")
    out << "log: #{CallLog.lines.inspect}"
    s = Shop.create!(handle: "abc")
    out << "macro: #{s.name.inspect} #{Shop.find(s.id).handle.inspect} #{Shop.where(handle: "abc").count}"
    out << "instance: #{s.loud_name.inspect}"
    out << "ancestors: #{ApplicationRecord.include?(Shouting)} #{Shop.ancestors.index(Shouting) > Shop.ancestors.index(ApplicationRecord)}"
    out
  end
end
"##;

/// Native boot with the real Zeitwerk: the app's models and concerns as
/// the main loader's roots, then the initializers, in order, as Rails
/// runs them after setting the loader up. Native only.
const NATIVE_BOOT: &str = r#"require "zeitwerk"

module Rails
  def self.autoloaders
    @autoloaders ||= Struct.new(:main).new(Zeitwerk::Loader.new)
  end
end

loader = Rails.autoloaders.main
loader.push_dir(File.join(__dir__, "app/models"))
loader.push_dir(File.join(__dir__, "app/models/concerns"))
loader.setup
Dir[File.join(__dir__, "config/initializers/*.rb")].sort.each { |file| load file }
"#;

/// What native Zeitwerk 2.8.2 and Rails 8.2 answer (checked on the task
/// PostgreSQL), so two identical wrong answers still fail.
const EXPECTED: [&str; 5] = [
    r#"before: ["loaded CallLog"]"#,
    r#"log: ["loaded CallLog", "after"]"#,
    r#"macro: "abc" "abc" 1"#,
    r#"instance: "ABC""#,
    r#"ancestors: true true"#,
];

const FILES: [(&str, &str); 11] = [
    ("roundhouse.yml", "database_adapter: postgresql\n"),
    ("config/database.yml", DATABASE_YML),
    ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
    ("db/structure.sql", STRUCTURE),
    ("app/models/application_record.rb", APPLICATION_RECORD),
    ("app/models/concerns/shouting.rb", SHOUTING),
    ("app/models/shop.rb", SHOP),
    ("app/models/call_log.rb", CALL_LOG),
    ("app/models/autoload_on_load_probe.rb", PROBE),
    ("config/initializers/application_record.rb", INITIALIZER),
    ("config/initializers/call_log.rb", LOG_INITIALIZER),
];

fn overlay() -> emit_and_run::Overlay {
    FILES.iter().fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text))
}

fn real_errors(errors: &[String]) -> Vec<&String> {
    errors.iter().filter(|e| !e.contains("database_adapter not supported")).collect()
}

#[test]
#[ignore = "requires pg gem, Core's pinned Rails and ROUNDHOUSE_PG_TEST_URL for a disposable local PostgreSQL server"]
fn autoload_on_load_matches_native_zeitwerk_on_postgresql() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    let native_files: Vec<(&str, &str)> = FILES.iter().copied().chain([("native_boot.rb", NATIVE_BOOT)]).collect();
    let (native, emitted_lines) = pg_probe::native_and_emitted(
        "autoload-on-load",
        &native_files,
        &emitted,
        "rh_e2e_autoload_on_load",
        "AutoloadOnLoadProbe",
        &["native_boot"],
    );
    assert_eq!(native, EXPECTED, "native");
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    assert_eq!(emitted_lines, native, "emitted");
}

/// The lifted include ends ApplicationRecord's body; the other callback
/// is carried for the port and CallLog's file reports its load.
#[test]
fn the_include_is_lifted_and_the_callback_carried() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    let read = |rel: &str| std::fs::read_to_string(emitted.join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"));
    let record = read("app/models/application_record.rb");
    assert!(record.contains("include Shouting"), "{record}");
    let call_log = read("app/models/call_log.rb");
    assert!(
        call_log.trim_end().ends_with("RoundhouseAutoload.autoloaded(\"CallLog\", __FILE__) if defined?(RoundhouseAutoload)"),
        "{call_log}"
    );
    let models = read("app/models.rb");
    assert!(models.contains("AUTOLOAD_CALLBACK_CPATHS = [\n    \"CallLog\",\n  ].freeze"), "{models}");
}

/// What the port does not carry is refused at the statement, by name.
#[test]
fn a_callback_without_a_literal_path_stays_refused() {
    for (initializer, reason) in [
        ("Rails.autoloaders.main.on_load do |cpath, value, abspath|\n  CallLog.add(cpath)\nend\n", "without a constant path"),
        ("name = \"CallLog\"\nRails.autoloaders.main.on_load(name) do |klass|\n  klass.add(1)\nend\n", "computed constant path"),
    ] {
        let (_emitted, errors) =
            overlay().write("config/initializers/call_log.rb", initializer).emit(roundhouse::project::BuildTarget::Ruby);
        assert!(
            errors.iter().any(|e| e.contains("does not run in the emitted app") && e.contains(reason)),
            "{reason}: {}",
            errors.join("\n")
        );
    }
}
