//! An app's own Rails::Engine on the PostgreSQL CRuby tree is carried
//! compiled and runs as railties 8.2.0.alpha runs it: its class body at
//! load (a conditional `paths.add`, a `config.k = local = value` whose
//! local an initializer block closes over), its initializers in Rails'
//! order among the standard engine initializers (`after:` included), and
//! its `to_prepare` and `after_initialize` hooks. Shopify Core has 131
//! such engines (components/*/lib/*/engine.rb); they were dropped.
//! An initializer's loads of the app's own files resolve against the
//! tree: `require "feature"` an app `lib/` file answers, and
//! `Dir[config.root.join("lib/…/**/*.rb")]` (essentials' patch loader),
//! with a matched source the tree does not carry named. A body
//! statement that cannot run is named, error[railtie_body_statement_not_run],
//! and the rest run.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const ENGINE: &str = r#"require "rails"
require "foo/settings"

module Foo
  LIMIT = -> { 3 }

  class Engine < Rails::Engine
    if Rails.env.test?
      paths.add("test", load_path: true)
    end

    config.foo = foo_config = Settings.new(4)

    Rails.autoloaders.main.on_load("Foo::Settings") { |klass| klass }

    # First in the class: a later one gets `after:` the one before it
    # (railties initializable.rb), which `before: :set_load_path` cycles.
    initializer "foo.first", before: :set_load_path do
      ORDER << "foo.first"
    end

    initializer "foo.patches", after: :load_config_initializers do |app|
      Dir[config.root.join("lib/foo/patches/**/*.rb")].each { |file| require file }
      ORDER << "foo.patches #{foo_config.size}"
    end

    initializer "foo.second", after: "foo.patches" do
      require "foo/settings"
      ORDER << "foo.second #{LIMIT.call} #{"x".shout}"
    end

    config.to_prepare do
      ORDER << "to_prepare"
    end

    config.after_initialize do
      ORDER << "after_initialize"
    end
  end
end
"#;

#[test]
fn an_app_engine_is_carried_and_runs_its_body_initializers_and_hooks_in_rails_order() {
    let (dir, _) = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\ndatabase_adapter: postgresql\n")
        .write("config/database.yml", "test:\n  adapter: postgresql\n  database: rh_rails_engine_port\n")
        .write("lib/foo/engine.rb", ENGINE)
        .write("lib/foo/settings.rb", "module Foo\n  class Settings\n    def initialize(size)\n      @size = size\n    end\n\n    def size\n      @size\n    end\n  end\nend\n")
        .write("lib/foo/patches/string_shout.rb", "class String\n  def shout\n    upcase + \"!\"\n  end\nend\n")
        .write("lib/foo/patches/statement_only.rb", "Comparable.instance_method(:clamp)\n")
        .emit(BuildTarget::Ruby);
    let engine = std::fs::read_to_string(dir.join("app/models/foo/engine.rb")).expect("the engine is carried");
    assert!(engine.contains("class Engine < Rails::Engine"), "{engine}");
    assert!(engine.contains("run_body_statement(\"lib/foo/engine.rb:8\") do"), "{engine}");
    assert!(engine.contains("require RoundhouseAutoload.path(\"models/foo/settings\")"), "{engine}");
    assert!(
        engine.contains("RoundhouseAutoload.carried_files(\"lib/foo/patches/**/*.rb\", [\"models/string\"], [\"lib/foo/patches/statement_only.rb\"])"),
        "{engine}"
    );
    let models = std::fs::read_to_string(dir.join("app/models.rb")).expect("app/models.rb");
    let root = models
        .find("Rails::Engine.source_roots[\"Foo::Engine\"] = File.expand_path(\"../\", RoundhouseAutoload::ROOT)")
        .unwrap_or_else(|| panic!("{models}"));
    let load = models.find("  \"models/foo/engine\",\n].each { |file| require RoundhouseAutoload.path(file) }").unwrap_or_else(|| panic!("{models}"));
    let boot = models.find("Rails::Railtie.run_initializers_of_railties(Rails.application, :boot)").unwrap_or_else(|| panic!("{models}"));
    assert!(root < load && load < boot, "{models}");

    let out = emit_and_run::ruby()
        .current_dir(&*dir)
        .args([
            "-e",
            // boot.rb's order, from the pieces this needs (the full boot
            // needs the pg gem): Rails, the railtie and engine ports,
            // app/models (which loads the engine and runs the :boot
            // initializers), then Finisher's.
            "ORDER = []\n\
             require_relative 'runtime/active_support_lazy_load_hooks'\n\
             $LOAD_PATH.unshift(File.expand_path('runtime/features'))\n\
             require_relative 'runtime/rails'\n\
             Rails.env_name = 'test'\n\
             require_relative 'runtime/rails_railtie'\n\
             require_relative 'runtime/rails_engine'\n\
             require_relative 'app/models'\n\
             Rails::Railtie.run_initializers_of_railties(Rails.application, :finish)\n\
             puts ORDER.inspect\n\
             puts Foo::Engine.paths['test'].to_a == [File.expand_path('test')]\n\
             puts Dir.exist?('test') && $LOAD_PATH.include?(File.expand_path('test'))\n\
             puts Foo::Engine.root.to_s == Dir.pwd\n\
             puts Foo::Engine.config.foo.size\n\
             puts Rails::Engine.subclasses.inspect\n",
        ])
        .output()
        .expect("ruby");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stdout}\n{stderr}");
    assert_eq!(
        stdout,
        "[\"foo.first\", \"foo.patches 4\", \"foo.second 3 X!\", \"to_prepare\", \"after_initialize\"]\n\
         true\n\
         true\n\
         true\n\
         4\n\
         [Foo::Engine]\n",
        "{stderr}"
    );
    assert!(
        stderr.contains("roundhouse: error[railtie_body_statement_not_run]: Foo::Engine body statement at lib/foo/engine.rb:14 did not run: NoMethodError"),
        "{stderr}"
    );
    assert!(
        stderr.contains("roundhouse: error[engine_glob_file_not_carried]: lib/foo/patches/statement_only.rb matches lib/foo/patches/**/*.rb in the source, but the tree carries no file for it; it was not loaded"),
        "{stderr}"
    );
    assert!(stderr.contains("roundhouse: 1 railtie and engine initializer(s), hook(s) and body statement(s) did not run"), "{stderr}");
}
