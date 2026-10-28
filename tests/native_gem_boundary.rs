//! A CRuby tree that is not PostgreSQL's loads the app's gems natively
//! against the runtime's Rails, as the PostgreSQL tree does
//! (`project::apply_native_gem_boundary`). A gem's `require
//! "active_record"` answers from the runtime's port (runtime/features),
//! never the gem home's activerecord, and its `require "x/railtie" if
//! defined?(::Rails)` subclasses the runtime's Rails::Railtie, whose
//! initializers and hooks boot runs. Before, the first loaded the gem
//! home's activerecord over the runtime's (Arel's `superclass mismatch
//! for class SelectManager`, state_machines-activerecord's shape), and
//! the second raised NameError: uninitialized constant Rails::Railtie
//! (state_machines-audit_trail's shape) before any test ran.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    trackish (2.1.0)\n    sourcegem (1.0.0)\n      trackish\n\nDEPENDENCIES\n  sourcegem\n";

/// A gem shaped like state_machines-activerecord and
/// state_machines-audit_trail together: it requires Active Record and
/// defines a railtie when Rails is defined.
const TRACKISH: &str = r#"require "active_record"

module Trackish
  class << self
    attr_accessor :configured_by, :after_initialize_ran
  end

  def self.base
    ActiveRecord::Base
  end
end

require "trackish/railtie" if defined?(::Rails)
"#;

const RAILTIE: &str = r#"module Trackish
  class Railtie < ::Rails::Railtie
    config.trackish_enabled = true

    initializer "trackish.configure" do |app|
      Trackish.configured_by = app.class.name if config.trackish_enabled
    end

    config.after_initialize { Trackish.after_initialize_ran = true }

    generators do
      require "trackish/generator"
    end
  end
end
"#;

fn gem_home(root: &std::path::Path) -> std::path::PathBuf {
    let home = root.join("tmp-gem-home");
    let lib = home.join("gems/trackish-2.1.0/lib");
    std::fs::create_dir_all(lib.join("trackish")).unwrap();
    std::fs::write(lib.join("trackish.rb"), TRACKISH).unwrap();
    std::fs::write(lib.join("trackish/railtie.rb"), RAILTIE).unwrap();
    std::fs::create_dir_all(home.join("specifications")).unwrap();
    std::fs::write(
        home.join("specifications/trackish-2.1.0.gemspec"),
        "Gem::Specification.new do |s|\n  s.name = \"trackish\"\n  s.version = \"2.1.0\"\n  s.summary = \"x\"\n  s.authors = [\"x\"]\n  s.files = [\"lib/trackish.rb\", \"lib/trackish/railtie.rb\"]\n  s.require_paths = [\"lib\"]\nend\n",
    )
    .unwrap();
    home
}

/// The ambient gem path, after the test's gem home: the emitted tree's
/// own gems (sqlite3) resolve as before, and an activerecord installed
/// there is what the boundary keeps out.
fn gem_path(home: &std::path::Path) -> String {
    let out = emit_and_run::ruby().args(["-e", "print Gem.path.join(File::PATH_SEPARATOR)"]).output().expect("ruby");
    format!("{}:{}", home.display(), String::from_utf8_lossy(&out.stdout))
}

fn app() -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\ngem_sources:\n  sourcegem: vendor/gems/sourcegem-1.0.0\n")
        .write("Gemfile.lock", LOCK)
        .write("vendor/gems/sourcegem-1.0.0/lib/sourcegem.rb", "require \"trackish\"\n\nmodule Sourcegem\nend\n")
        .write("app/models/oracle.rb", "class Oracle\n  def ask\n    Sourcegem\n  end\nend\n")
}

#[test]
fn a_native_gem_loads_against_the_runtime_s_active_record_and_railtie() {
    let scratch = tempfile_dir();
    let home = gem_home(&scratch);
    let path = gem_path(&home);
    let run = app().run_ruby_env(
        "puts Trackish.base.equal?(ActiveRecord::Base)\n\
         puts Trackish::Railtie.superclass\n\
         puts Trackish.configured_by\n\
         puts Trackish.after_initialize_ran.inspect\n\
         puts $LOADED_FEATURES.grep(%r{/(activerecord|railties)-}).empty?\n\
         require \"rails/engine\"\n\
         puts Rails::Engine.superclass\n\
         begin\n  require \"active_storage/engine\"\n  puts :active_storage_loaded\nrescue LoadError => e\n  puts e.message[/framework_feature_not_ported/]\nend\n",
        &[("GEM_PATH", &path), ("GEM_HOME", &home.display().to_string())],
    );
    run.assert_passes();
    assert_eq!(
        run.stdout,
        "true\nRails::Railtie\nRails::Application\ntrue\ntrue\nRails::Railtie\nframework_feature_not_ported\n",
        "{}",
        run.stderr
    );
    let _ = std::fs::remove_dir_all(&scratch);
}

#[test]
fn boot_runs_the_railties_initializers_around_the_app_s_code() {
    let (dir, _) = app().emit(BuildTarget::Ruby);
    let facades = std::fs::read_to_string(dir.join("runtime/gem_facades.rb")).expect("gem_facades.rb");
    let features = facades.find("$LOAD_PATH.unshift(File.expand_path(\"features\", __dir__))").unwrap_or_else(|| panic!("{facades}"));
    let railtie = facades.find("require_relative \"rails_railtie\"\n").unwrap_or_else(|| panic!("{facades}"));
    let dependency = facades.find("[\"trackish\"].each do |feature|").unwrap_or_else(|| panic!("{facades}"));
    assert!(features < railtie && railtie < dependency, "{facades}");
    let models = std::fs::read_to_string(dir.join("app/models.rb")).expect("app/models.rb");
    assert!(models.contains("Rails::Railtie.run_initializers_of_railties(Rails.application, :boot)\n"), "{models}");
    assert!(!models.contains("RoundhouseAutoload.eager_load! if RoundhouseAutoload.eager_load?"), "{models}");
    let boot = std::fs::read_to_string(dir.join("boot.rb")).expect("boot.rb");
    let finish = boot.find("Rails::Railtie.run_initializers_of_railties(Rails.application, :finish)").unwrap_or_else(|| panic!("{boot}"));
    let loads = boot.find("require_relative \"app/models\"").expect("app/models");
    assert!(loads < finish, "{boot}");
    for feature in ["rails", "rails/railtie", "rails/engine", "active_record"] {
        assert!(dir.join(format!("runtime/features/{feature}.rb")).exists(), "runtime/features/{feature}.rb");
    }
}

#[test]
fn an_app_with_no_native_gems_keeps_its_boot() {
    let (dir, _) = emit_and_run::empty_app()
        .write("app/models/oracle.rb", "class Oracle\nend\n")
        .emit(BuildTarget::Ruby);
    let facades = std::fs::read_to_string(dir.join("runtime/gem_facades.rb")).expect("gem_facades.rb");
    assert!(!facades.contains("File.expand_path(\"features\", __dir__)"), "{facades}");
    assert!(!dir.join("runtime/framework_feature_refusal.rb").exists());
    let models = std::fs::read_to_string(dir.join("app/models.rb")).expect("app/models.rb");
    assert!(models.contains("RoundhouseAutoload.eager_load! if RoundhouseAutoload.eager_load?"), "{models}");
}

fn tempfile_dir() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("rh-native-gem-boundary-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
