//! Boot initializers' statements, run by the emitted PostgreSQL boot
//! (`ingest::initializer_programs`, `project::initializer_program_block`,
//! `project::apply_class_load_hooks`).
//!
//! Rails `load`s each `config/initializers` file in sorted order, after
//! the gems' railtie initializers and before the Finisher's
//! `after_initialize` hooks. `ActiveSupport.on_load(:hook)` registers a
//! block that `run_load_hooks(:hook, base)` later `class_eval`s on
//! `base`, or runs at once when the base already loaded. An app's own
//! class can fire a hook at the end of its body. A statement the emitted
//! boot cannot carry stays an `initializer_unsupported` gap with its
//! reason; one that raises at boot names itself and boot continues.

use std::path::{Path, PathBuf};
use std::process::Command;

const MANIFEST_DIR: &str = env!("CARGO_MANIFEST_DIR");

const FIRST: &str = r#"# frozen_string_literal: true

require "set"

BootLog.record("first: top")

ActiveSupport.on_load(:active_record) do
  BootLog.record("first: active_record on #{name}")
end

ActiveSupport.on_load(:widget_registry) do
  WidgetRegistry.register("from first")
end

Rails.application.config.after_initialize do
  BootLog.record("first: after_initialize")
end

BootLog.explode!
BootLog.record("first: after the raise")

def a_helper
  1
end
"#;

const SECOND: &str = r#"# frozen_string_literal: true

seen = BootLog.entries.size
BootLog.record("second: top saw #{seen}")

ActiveSupport.on_load(:never_fired) do
  BootLog.record("never")
end

configure_widgets

Rails.autoloaders.main.on_load("WidgetRegistry") do |klass|
  BootLog.record("zeitwerk")
end
"#;

const BOOT_LOG: &str = r#"module BootLog
  def self.entries
    @entries ||= []
  end

  def self.record(entry)
    entries << entry
  end

  def self.explode!
    raise ArgumentError, "boom"
  end
end
"#;

const WIDGET_REGISTRY: &str = r#"module WidgetRegistry
  def self.registered
    @registered ||= []
  end

  def self.register(name)
    registered << name
  end

  ActiveSupport.run_load_hooks(:widget_registry, self)
end
"#;

const DATABASE_YML: &str = "default: &default\n  adapter: postgresql\n\ntest:\n  <<: *default\n  database: initializer_programs\n";

struct TempDir(PathBuf);
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn temp_dir(tag: &str) -> TempDir {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "roundhouse-initprog-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    TempDir(dir)
}

fn copy_tree(from: &Path, to: &Path) {
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            std::fs::create_dir_all(&target).unwrap();
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

fn fixture_app() -> TempDir {
    let app = temp_dir("app");
    copy_tree(&Path::new(MANIFEST_DIR).join("fixtures/tiny-blog"), &app.0);
    let write = |rel: &str, text: &str| {
        let path = app.0.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    write("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n");
    write("app/models/boot_log.rb", BOOT_LOG);
    write("app/models/widget_registry.rb", WIDGET_REGISTRY);
    write("config/initializers/10_first.rb", FIRST);
    write("config/initializers/20_second.rb", SECOND);
    write("roundhouse.yml", "database_adapter: postgresql\n");
    write("config/database.yml", DATABASE_YML);
    app
}

fn emit(app: &roundhouse::app::App) -> (TempDir, Vec<(String, String)>) {
    let out = temp_dir("out");
    let files = roundhouse::project::target_files(
        app,
        &Path::new(MANIFEST_DIR).join("fixtures/tiny-blog"),
        roundhouse::project::BuildTarget::Ruby,
    )
    .expect("package PostgreSQL Ruby project");
    for (name, content) in &files {
        let path = out.0.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }
    (out, files)
}

fn gaps_in<'a>(app: &'a roundhouse::app::App, file: &str) -> Vec<&'a roundhouse::app::InitializerGap> {
    app.initializer_gaps
        .iter()
        .filter(|g| app.sources.get(g.span.file.0 as usize - 1).is_some_and(|s| s.path.ends_with(file)))
        .collect()
}

#[test]
fn carried_statements_and_their_refusals() {
    let dir = fixture_app();
    let app = roundhouse::ingest::ingest_app(&dir.0).expect("ingest fixture");
    let programs: Vec<(&str, usize)> =
        app.initializer_programs.iter().map(|p| (p.file.as_str(), p.statements)).collect();
    assert_eq!(
        programs,
        [("config/initializers/10_first.rb", 7), ("config/initializers/20_second.rb", 3)],
        "{:?}",
        app.initializer_programs
    );
    assert_eq!(app.class_load_hooks.len(), 1, "{:?}", app.class_load_hooks);
    assert_eq!(app.class_load_hooks[0].class, "WidgetRegistry");
    assert_eq!(app.class_load_hooks[0].base, "WidgetRegistry");

    let first = gaps_in(&app, "config/initializers/10_first.rb");
    assert_eq!(first.len(), 1, "{first:?}");
    assert_eq!(first[0].construct, "def");
    assert!(first[0].detail.starts_with("`def` in an initializer"), "{}", first[0].detail);
    let second = gaps_in(&app, "config/initializers/20_second.rb");
    let second: Vec<(&str, &str)> = second.iter().map(|g| (g.construct.as_str(), g.detail.as_str())).collect();
    // The Zeitwerk callback is carried (runtime/spinel/rails_autoloaders.rb).
    assert_eq!(second.len(), 2, "{second:?}");
    assert_eq!(second[0].0, "ActiveSupport.on_load");
    assert!(second[0].1.starts_with("nothing fires `:never_fired`"), "{second:?}");
    assert!(second[1].1.starts_with("`configure_widgets` has an implicit receiver"), "{second:?}");
}

/// Boot runs both initializers in order, the `:active_record` hook at
/// registration (the runtime's base is loaded), a raising statement named
/// and skipped, the app hook and then Zeitwerk's `on_load(
/// "WidgetRegistry")` when Finisher's eager loading loads `WidgetRegistry`,
/// and `after_initialize` after it. Boot loads the `pg` gem (it never connects):
/// run with `--ignored` and a `GEM_PATH` that has it.
#[test]
#[ignore = "requires ruby with the pg gem"]
fn boot_runs_initializer_statements_in_rails_order() {
    let dir = fixture_app();
    let app = roundhouse::ingest::ingest_app(&dir.0).expect("ingest fixture");
    let (out, files) = emit(&app);
    let boot = &files.iter().find(|(p, _)| p == "boot.rb").expect("boot.rb").1;
    let at = |needle: &str| boot.find(needle).unwrap_or_else(|| panic!("{needle} missing:\n{boot}"));
    let fire = at("ActiveSupport.run_load_hooks(:active_record, ActiveRecord::Base)");
    let first = at("RoundhouseInitializerConfigInitializers10First.run_initializer\n");
    let second = at("RoundhouseInitializerConfigInitializers20Second.run_initializer\n");
    let finish = at("Rails::Railtie.run_initializers_of_railties(Rails.application, :finish)");
    assert!(fire < first && first < second && second < finish, "{boot}");
    let registry = &files.iter().find(|(p, _)| p == "app/models/widget_registry.rb").expect("registry").1;
    assert!(
        registry.ends_with("ActiveSupport.run_load_hooks(:widget_registry, WidgetRegistry)\n"),
        "{registry}"
    );

    let output = Command::new("ruby")
        .args(["-e", "require \"./boot\"\nputs BootLog.entries\nputs WidgetRegistry.registered.inspect\n"])
        .current_dir(&out.0)
        .env_remove("DATABASE_URL")
        .env_remove("ROUNDHOUSE_DATABASE_URL")
        .output()
        .expect("ruby");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stdout={stdout}\nstderr={stderr}");
    assert_eq!(
        stdout.lines().collect::<Vec<_>>(),
        [
            "first: top",
            "first: active_record on ActiveRecord::Base",
            "first: after the raise",
            "second: top saw 3",
            // Zeitwerk's on_load("WidgetRegistry"), once its file loaded,
            // after its body's load hook: Finisher's `eager_load!` (the
            // fixture's test environment leaves `config.eager_load` nil,
            // true by default) loads it before the `after_initialize`
            // hooks.
            "zeitwerk",
            "first: after_initialize",
            "[\"from first\"]",
        ],
        "stderr={stderr}"
    );
    let named: Vec<&str> = stderr.lines().filter(|l| l.contains("error[initializer_statement_not_run]")).collect();
    assert_eq!(named.len(), 1, "{stderr}");
    assert!(
        named[0].contains("config/initializers/10_first.rb:19 BootLog.explode! did not run: ArgumentError: boom"),
        "{}",
        named[0]
    );
}

/// A vendored gem's file of declarations only (two modules, the second
/// reading the first as its body runs). Nothing in the emitted tree
/// autoloads a plain gem's constants.
const GEM_SHAPES: &str = "module Gqlc\n  module Shapes\n    ALL = %i[a b c].freeze\n  end\n\n  class Plain\n    SIZE = Shapes::ALL.size\n\n    def self.x = SIZE\n  end\nend\n";

/// An initializer that requires it, recording what it sees before and
/// after the require.
const REQUIRES_GEM_FILE: &str = "BootLog.record(defined?(Gqlc::Plain).inspect)\nrequire \"gqlc/plain\"\nBootLog.record(Gqlc::Plain.x)\nBootLog.record(Gqlc::Shapes::ALL.last)\n";

const SIMPLE_BOOT_LOG: &str = "module BootLog\n  def self.entries\n    @entries ||= []\n  end\n\n  def self.record(entry)\n    entries << entry\n  end\nend\n";

fn gem_file_fixture_app() -> TempDir {
    let app = temp_dir("gemfile");
    copy_tree(&Path::new(MANIFEST_DIR).join("fixtures/tiny-blog"), &app.0);
    let write = |rel: &str, text: &str| {
        let path = app.0.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    write("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n");
    write("app/models/boot_log.rb", SIMPLE_BOOT_LOG);
    write("config/initializers/plain.rb", REQUIRES_GEM_FILE);
    write("vendor/gems/gqlc-1.0.0/lib/gqlc/plain.rb", GEM_SHAPES);
    write("roundhouse.yml", "database_adapter: postgresql\ngem_sources:\n  gqlc: vendor/gems/gqlc-1.0.0\n");
    write("Gemfile.lock", "GEM\n  remote: https://rubygems.org/\n  specs:\n    gqlc (1.0.0)\n\nDEPENDENCIES\n  gqlc\n");
    write("config/database.yml", DATABASE_YML);
    app
}

/// An initializer's `require` of a gem file of declarations only was
/// applied as a no-op, as for a lib file whose constants autoload: the
/// gem's constants have none, so the file never loaded and the next
/// statement raised NameError at boot. It is carried as
/// `RoundhouseAutoload.require_source`, which loads the files of the
/// classes the gem file declares (SOURCE_FILES), at that statement.
#[test]
fn an_initializer_requiring_a_gem_file_of_declarations_carries_the_require() {
    let dir = gem_file_fixture_app();
    let app = roundhouse::ingest::ingest_app(&dir.0).expect("ingest fixture");
    let gaps = gaps_in(&app, "config/initializers/plain.rb");
    assert!(gaps.is_empty(), "{gaps:?}");
    let programs: Vec<(&str, usize)> =
        app.initializer_programs.iter().map(|p| (p.file.as_str(), p.statements)).collect();
    assert_eq!(programs, [("config/initializers/plain.rb", 4)]);
    let (_out, files) = emit(&app);
    let holder = &files
        .iter()
        .find(|(p, _)| p == "app/models/roundhouse_initializer_config_initializers_plain.rb")
        .expect("holder")
        .1;
    assert!(holder.contains("RoundhouseAutoload.require_source(\"vendor/gems/gqlc-1.0.0/lib/gqlc/plain.rb\")"), "{holder}");
    let models = &files.iter().find(|(p, _)| p == "app/models.rb").expect("app/models.rb").1;
    assert!(
        models.contains("  SOURCE_FILES = {\n    \"vendor/gems/gqlc-1.0.0/lib/gqlc/plain.rb\" => [\"models/gqlc/shapes\", \"models/gqlc/plain\"],\n  }.freeze\n"),
        "{models}"
    );
}

/// The gem file nests (`module Gqlc; class Plain`), so `Shapes` in
/// Plain's `SIZE = Shapes::ALL.size` reads `Gqlc::Shapes` lexically.
/// Constant values are rooted like method bodies; `Gqlc::Shapes::ALL` is
/// a value constant, and taking only class names as targets made the
/// compact-form rule root it to a `::Shapes` that does not exist (the
/// boot test below then failed with `uninitialized constant Shapes`).
#[test]
fn a_gem_constant_reading_a_sibling_module_keeps_its_lexical_target() {
    let dir = gem_file_fixture_app();
    let app = roundhouse::ingest::ingest_app(&dir.0).expect("ingest fixture");
    let (_out, files) = emit(&app);
    let plain = &files.iter().find(|(p, _)| p == "app/models/gqlc/plain.rb").expect("plain.rb").1;
    assert!(!plain.contains("= ::Shapes"), "{plain}");
    assert!(plain.contains("SIZE = Shapes::ALL.size") || plain.contains("SIZE = Gqlc::Shapes::ALL.size"), "{plain}");
}

/// The boot runs it as native Ruby does: the constants are undefined
/// before the require and defined after it. Boot loads the `pg` gem:
/// run with `--ignored` and a `GEM_PATH` that has it.
#[test]
#[ignore = "requires ruby with the pg gem"]
fn boot_loads_a_gem_file_an_initializer_requires_where_it_requires_it() {
    let dir = gem_file_fixture_app();
    let native = Command::new("ruby")
        .arg("-I")
        .arg(dir.0.join("vendor/gems/gqlc-1.0.0/lib"))
        .args(["-e", &format!("{SIMPLE_BOOT_LOG}{REQUIRES_GEM_FILE}p BootLog.entries\n")])
        .output()
        .expect("ruby");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    assert_eq!(String::from_utf8_lossy(&native.stdout), "[\"nil\", 3, :c]\n", "native control");

    let app = roundhouse::ingest::ingest_app(&dir.0).expect("ingest fixture");
    let (out, _files) = emit(&app);
    let output = Command::new("ruby")
        .args(["-e", "require \"./boot\"\np BootLog.entries\n"])
        .current_dir(&out.0)
        .env_remove("DATABASE_URL")
        .env_remove("ROUNDHOUSE_DATABASE_URL")
        .output()
        .expect("ruby");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stdout={stdout}\nstderr={stderr}");
    assert!(!stderr.contains("initializer_statement_not_run"), "{stderr}");
    assert_eq!(stdout, "[\"nil\", 3, :c]\n", "stderr={stderr}");
}

/// A lambda's `return` returns from the lambda, so the statement that
/// stores it runs (Core's essentials initializers/verdict.rb:
/// `config.on_flag_error = ->(error, handle) { return false if ... }` in
/// `Verdict.configure`; r9 refused the whole statement, so the test
/// env's `config.flag_store = Verdict::Flags::BinaryStore.new` never ran
/// and 35 tests raised "Handle ... not found in flags.yml"). A `return`
/// in a plain block still ends the file and is refused.
const LAMBDA_RETURNS: &str = "module FlagConfig\n  def self.handlers\n    @handlers ||= {}\n  end\nend\n\
FlagConfig.handlers[:arrow] = ->(error) {\n  return false if error.nil?\n  [1, 2].each { |x| return x if x == error }\n  :other\n}\n\
FlagConfig.handlers[:kernel] = lambda do |error|\n  return :none unless error\n  error * 2\nend\n\
BootLog.record(FlagConfig.handlers[:arrow].call(nil))\nBootLog.record(FlagConfig.handlers[:arrow].call(2))\n\
BootLog.record(FlagConfig.handlers[:arrow].call(5))\nBootLog.record(FlagConfig.handlers[:kernel].call(nil))\n\
BootLog.record(FlagConfig.handlers[:kernel].call(4))\n";
const BLOCK_RETURN: &str = "[1].each { |x| return if x }\n";

fn lambda_return_fixture_app() -> TempDir {
    let app = temp_dir("lambda-return");
    copy_tree(&Path::new(MANIFEST_DIR).join("fixtures/tiny-blog"), &app.0);
    let write = |rel: &str, text: &str| {
        let path = app.0.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    write("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n");
    write("app/models/boot_log.rb", SIMPLE_BOOT_LOG);
    let (module, statements) = LAMBDA_RETURNS.split_at(LAMBDA_RETURNS.find("FlagConfig.handlers[:arrow]").unwrap());
    write("app/models/flag_config.rb", module);
    write("config/initializers/lambdas.rb", statements);
    write("config/initializers/zz_block.rb", BLOCK_RETURN);
    write("roundhouse.yml", "database_adapter: postgresql\n");
    write("config/database.yml", DATABASE_YML);
    app
}

#[test]
fn a_lambda_return_in_an_initializer_is_carried() {
    let dir = lambda_return_fixture_app();
    let app = roundhouse::ingest::ingest_app(&dir.0).expect("ingest fixture");
    let gaps = gaps_in(&app, "config/initializers/lambdas.rb");
    assert!(gaps.is_empty(), "{gaps:?}");
    let block = gaps_in(&app, "config/initializers/zz_block.rb");
    assert_eq!(block.len(), 1, "{block:?}");
    assert_eq!(block[0].detail, "`return` in an initializer");
}

/// The boot runs the lambdas as native Ruby does. Boot loads the `pg`
/// gem: run with `--ignored` and a `GEM_PATH` that has it.
#[test]
#[ignore = "requires ruby with the pg gem"]
fn boot_runs_an_initializer_lambda_that_returns() {
    let native = Command::new("ruby")
        .args(["-e", &format!("{SIMPLE_BOOT_LOG}{LAMBDA_RETURNS}p BootLog.entries\n")])
        .output()
        .expect("ruby");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    assert_eq!(String::from_utf8_lossy(&native.stdout), "[false, 2, :other, :none, 8]\n", "native control");

    let dir = lambda_return_fixture_app();
    let app = roundhouse::ingest::ingest_app(&dir.0).expect("ingest fixture");
    let (out, _files) = emit(&app);
    let output = Command::new("ruby")
        .args(["-e", "require \"./boot\"\np BootLog.entries\n"])
        .current_dir(&out.0)
        .env_remove("DATABASE_URL")
        .env_remove("ROUNDHOUSE_DATABASE_URL")
        .output()
        .expect("ruby");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stdout={stdout}\nstderr={stderr}");
    assert!(!stderr.contains("initializer_statement_not_run"), "{stderr}");
    assert_eq!(stdout, "[false, 2, :other, :none, 8]\n", "stderr={stderr}");
}
