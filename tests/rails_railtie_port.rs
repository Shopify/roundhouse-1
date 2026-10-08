//! `require "rails/railtie"` on the PostgreSQL CRuby tree answers from the
//! runtime's Rails::Railtie port (runtime/rails_railtie.rb), and boot runs
//! the gems' railtie initializers where Application#initialize! would.
//! Shopify Core's hashie 5.0.0 (a dependency of its panama_client gem
//! source) runs `require 'hashie/railtie' if defined?(::Rails)`, whose
//! railtie sets `Hashie.logger = Rails.logger` and, on Rails >= 6,
//! includes Hashie::Extensions::ActiveSupport::CoreExt::Hash into
//! Hashie::Mash so `Mash#except` takes symbol keys. Without the port the
//! require hit the railties checkout (real Rails over the ports), and with
//! the checkout off $LOAD_PATH it raised LoadError, which hashie rescues:
//! both initializers were skipped.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    hashlike (5.0.0)\n    panamaish (1.0.0)\n      hashlike\n\nDEPENDENCIES\n  panamaish\n";

/// hashie 5.0.0's lib/hashie.rb tail, railtie.rb and
/// extensions/active_support/core_ext/hash.rb, renamed; Mash reduced to
/// its string-key conversion.
const HASHLIKE: &str = r#"require 'logger'

module Hashlike
  class << self
    attr_writer :logger

    def logger
      @logger ||= Logger.new($stdout)
    end
  end

  class Mash < Hash
    def initialize(source = {})
      super()
      source.each { |k, v| self[convert_key(k)] = v }
    end

    def convert_key(key)
      key.respond_to?(:to_sym) ? key.to_s : key
    end
  end

  require 'hashlike/railtie' if defined?(::Rails)
end
"#;

const RAILTIE: &str = r#"begin
  require 'rails/railtie'

  module Hashlike
    class Railtie < Rails::Railtie
      # Set the Hashlike.logger to use Rails.logger when used with rails.
      initializer 'hashlike.configure_logger', after: 'initialize_logger' do
        Hashlike.logger = Rails.logger
      end

      initializer 'hashlike.patch_hash_except', after: 'load_active_support' do
        if Rails::VERSION::MAJOR >= 6
          require 'hashlike/extensions/active_support/core_ext/hash'
          Hashlike::Mash.send(:include, Hashlike::Extensions::ActiveSupport::CoreExt::Hash)
        end
      end

      config.after_initialize { |app| Hashlike.after_initialize_ran = app.class.name }
    end
  end
rescue LoadError => e
  Hashlike.logger.info("Hashlike skipping railtie as #{e.message}")
end

module Hashlike
  class << self
    attr_accessor :after_initialize_ran
  end
end
"#;

const CORE_EXT: &str = r#"module Hashlike
  module Extensions
    module ActiveSupport
      module CoreExt
        module Hash
          def except(*keys)
            string_keys = keys.map { |key| convert_key(key) }
            super(*string_keys)
          end
        end
      end
    end
  end
end
"#;

fn gem_home(root: &std::path::Path) -> std::path::PathBuf {
    let home = root.join("tmp-gem-home");
    let gem = home.join("gems/hashlike-5.0.0/lib");
    std::fs::create_dir_all(gem.join("hashlike/extensions/active_support/core_ext")).unwrap();
    std::fs::write(gem.join("hashlike.rb"), HASHLIKE).unwrap();
    std::fs::write(gem.join("hashlike/railtie.rb"), RAILTIE).unwrap();
    std::fs::write(gem.join("hashlike/extensions/active_support/core_ext/hash.rb"), CORE_EXT).unwrap();
    std::fs::create_dir_all(home.join("specifications")).unwrap();
    std::fs::write(
        home.join("specifications/hashlike-5.0.0.gemspec"),
        "Gem::Specification.new do |s|\n  s.name = \"hashlike\"\n  s.version = \"5.0.0\"\n  s.summary = \"x\"\n  s.authors = [\"x\"]\n  s.files = [\"lib/hashlike.rb\", \"lib/hashlike/railtie.rb\", \"lib/hashlike/extensions/active_support/core_ext/hash.rb\"]\n  s.require_paths = [\"lib\"]\nend\n",
    )
    .unwrap();
    home
}

#[test]
fn a_gem_s_railtie_defines_against_the_port_and_its_initializers_run_at_boot() {
    let (dir, _) = emit_and_run::empty_app()
        .write(
            "roundhouse.yml",
            "test_paths:\n  - test\ngem_sources:\n  panamaish: vendor/gems/panamaish-1.0.0\ndatabase_adapter: postgresql\n",
        )
        .write("config/database.yml", "test:\n  adapter: postgresql\n  database: rh_rails_railtie_port\n")
        .write("Gemfile.lock", LOCK)
        .write("vendor/gems/panamaish-1.0.0/lib/panamaish.rb", "require 'hashlike'\n\nmodule Panamaish\nend\n")
        .write("lib/oracle.rb", "class Oracle\n  def ask\n    Panamaish\n  end\nend\n")
        .emit(BuildTarget::Ruby);
    let feature = std::fs::read_to_string(dir.join("runtime/features/rails/railtie.rb")).expect("rails/railtie feature");
    assert!(feature.contains("require_relative \"../../rails_railtie\""), "{feature}");
    let boot = std::fs::read_to_string(dir.join("boot.rb")).expect("boot.rb");
    let finish = boot.find("Rails::Railtie.run_initializers_of_railties(Rails.application, :finish)").unwrap_or_else(|| panic!("{boot}"));
    let gems = boot.find("require_relative \"runtime/gem_facades\"").expect("gem facades");
    let railtie = boot.find("require_relative \"runtime/rails_railtie\"").unwrap_or_else(|| panic!("{boot}"));
    let models = boot.find("require_relative \"app/models\"").expect("app/models");
    assert!(gems < railtie && railtie < models && models < finish, "{boot}");
    // The :boot phase runs once app/models has loaded the boot files and
    // the carried engines, before eager loading, as Rails runs it before
    // `eager_load!`.
    let autoload = std::fs::read_to_string(dir.join("app/models.rb")).expect("app/models.rb");
    let run = autoload.find("Rails::Railtie.run_initializers_of_railties(Rails.application, :boot)").unwrap_or_else(|| panic!("{autoload}"));
    let eager = autoload.find("RoundhouseAutoload.eager_load! if RoundhouseAutoload.eager_load?").unwrap_or_else(|| panic!("{autoload}"));
    assert!(run < eager, "{autoload}");
    let facades = std::fs::read_to_string(dir.join("runtime/gem_facades.rb")).expect("gem_facades.rb");
    let features = facades.find("$LOAD_PATH.unshift(File.expand_path(\"features\", __dir__))").unwrap_or_else(|| panic!("{facades}"));
    let dependency = facades.find("[\"hashlike\"].each do |feature|").unwrap_or_else(|| panic!("{facades}"));
    assert!(features < dependency, "{facades}");

    let home = gem_home(&dir);
    let out = emit_and_run::ruby()
        .current_dir(&*dir)
        .env("RH_TEST_GEMS", &home)
        .env("RAILS_ENV", "test")
        .args([
            "-e",
            // boot.rb's order, from the pieces this needs (the full boot
            // needs the pg gem): the hooks and features path
            // gem_facades.rb sets up, Rails, the gem source's dependency
            // as gem_facades.rb requires it, then Application#initialize!.
            // The gem home goes ahead of the ambient one.
            "Gem.paths = { 'GEM_PATH' => [ENV['RH_TEST_GEMS'], *Gem.path].join(File::PATH_SEPARATOR) }\n\
             require_relative 'runtime/active_support_lazy_load_hooks'\n\
             $LOAD_PATH.unshift(File.expand_path('runtime/features'))\n\
             require_relative 'runtime/module_delegate'\n\
             require_relative 'runtime/active_support_inflector'\n\
             require_relative 'runtime/rails'\n\
             require 'hashlike'\n\
             require_relative 'runtime/rails_railtie'\n\
             Rails::Railtie.run_initializers_of_railties(Rails.application)\n\
             puts Hashlike.logger.equal?(Rails.logger)\n\
             puts Hashlike::Mash.new(a: 1, b: 2).except(:a).inspect\n\
             puts Hashlike::Railtie.superclass\n\
             puts Hashlike.after_initialize_ran\n\
             puts Rails::VERSION::STRING\n\
             puts $LOADED_FEATURES.grep(%r{/railties[-/]}).empty?\n",
        ])
        .output()
        .expect("ruby");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stdout}\n{stderr}");
    assert!(!stdout.contains("skipping railtie"), "{stdout}");
    assert_eq!(stdout, "true\n{\"b\" => 2}\nRails::Railtie\nRails::Application\n8.2.0.alpha\ntrue\n", "{stderr}");
}

/// The port's initializer order is Rails::Initializable::Collection's:
/// load order across railties, `before:`/`after:` within the collection,
/// Bootstrap names first and Finisher names last.
#[test]
fn railtie_initializers_run_in_rails_order() {
    let (dir, _) = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\ndatabase_adapter: postgresql\n")
        .write("config/database.yml", "test:\n  adapter: postgresql\n  database: rh_rails_railtie_port\n")
        .write("lib/oracle.rb", "class Oracle\nend\n")
        .emit(BuildTarget::Ruby);
    let out = emit_and_run::ruby()
        .current_dir(&*dir)
        .args([
            "-e",
            "require_relative 'runtime/active_support_lazy_load_hooks'\n\
             $LOAD_PATH.unshift(File.expand_path('runtime/features'))\n\
             require_relative 'runtime/module_delegate'\n\
             require_relative 'runtime/active_support_inflector'\n\
             require 'rails/railtie'\n\
             ORDER = []\n\
             ActiveSupport.on_load(:before_initialize) { |app| ORDER << :before_initialize }\n\
             class One < Rails::Railtie\n\
               initializer('one.a') { ORDER << 'one.a' }\n\
               initializer('one.b') { |app| ORDER << \"one.b #{app}\" }\n\
               initializer('one.c', before: 'one.a') { ORDER << 'one.c' }\n\
               initializer('one.late', after: :finisher_hook) { ORDER << 'one.late' }\n\
               initializer('one.tests', group: :tests) { ORDER << 'never' }\n\
               config.to_prepare { ORDER << :to_prepare }\n\
               config.after_initialize { ORDER << :after_initialize }\n\
               config.one_option = 7\n\
             end\n\
             class Two < Rails::Railtie\n\
               initializer('two.a', before: 'one.b') { ORDER << 'two.a' }\n\
             end\n\
             Rails::Railtie.run_initializers_of_railties(:app)\n\
             Rails::Railtie.run_initializers_of_railties(:again)\n\
             puts ORDER.inspect\n\
             puts One.config.one_option\n\
             puts Rails::Railtie.subclasses.inspect\n\
             puts One.railtie_name\n\
             begin\n  Rails::Railtie.send(:new)\nrescue RuntimeError => e\n  puts e.message\nend\n",
        ])
        .output()
        .expect("ruby");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(
        stdout,
        "[:before_initialize, \"one.c\", \"one.a\", \"two.a\", \"one.b app\", :to_prepare, :after_initialize, \"one.late\"]\n\
         7\n\
         [One, Two]\n\
         one\n\
         Rails::Railtie is abstract, you cannot instantiate it directly.\n"
    );
}

/// A railtie that needs Rails the tree does not port (shopify_session
/// 7.40's initializer includes ActionController's
/// RequestForgeryProtection::ProtectionMethods; bugsnag 5.5's
/// after_initialize hook reads `Rails.configuration.filter_parameters`)
/// does not run, and boot names each one, error[gem_railtie_not_run];
/// the other railties run. `require "rails"` and
/// ActiveSupport::OrderedOptions (graphql 2.6's railtie body) answer
/// from the runtime.
#[test]
fn a_railtie_needing_unported_rails_is_named_and_the_rest_run() {
    let (dir, _) = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\ndatabase_adapter: postgresql\n")
        .write("config/database.yml", "test:\n  adapter: postgresql\n  database: rh_rails_railtie_port\n")
        .write("lib/oracle.rb", "class Oracle\nend\n")
        .emit(BuildTarget::Ruby);
    let out = emit_and_run::ruby()
        .current_dir(&*dir)
        .args([
            "-e",
            "require_relative 'runtime/active_support_lazy_load_hooks'\n\
             $LOAD_PATH.unshift(File.expand_path('runtime/features'))\n\
             require_relative 'runtime/module_delegate'\n\
             require_relative 'runtime/active_support_inflector'\n\
             require 'rails'\n\
             RAN = []\n\
             class Sessionish < Rails::Railtie\n\
               config.sessionish = ActiveSupport::OrderedOptions.new\n\
               config.sessionish.enabled = true\n\
               initializer('sessionish.protect') { Unported::ProtectionMethods }\n\
               initializer('sessionish.after') { RAN << :after_protect }\n\
               config.after_initialize { raise NoMethodError, 'filter_parameters' }\n\
               config.app_middleware.use :SomeMiddleware\n\
             end\n\
             class Fine < Rails::Railtie\n\
               initializer('fine') { RAN << config.sessionish.enabled }\n\
             end\n\
             Rails::Railtie.run_initializers_of_railties(:app)\n\
             puts RAN.inspect\n",
        ])
        .output()
        .expect("ruby");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stdout}\n{stderr}");
    assert_eq!(stdout, "[:after_protect, true]\n", "{stderr}");
    assert!(
        stderr.contains("roundhouse: error[gem_railtie_not_run]: Sessionish initializer \"sessionish.protect\" did not run: NameError: uninitialized constant Sessionish::Unported"),
        "{stderr}"
    );
    assert!(stderr.contains("roundhouse: error[gem_railtie_not_run]: config.after_initialize hook (-e:"), "{stderr}");
    assert!(stderr.contains("did not run: NoMethodError: filter_parameters"), "{stderr}");
    assert!(
        stderr.contains("roundhouse: error[gem_railtie_middleware_not_applied]: config.app_middleware.use(:SomeMiddleware)"),
        "{stderr}"
    );
    assert!(stderr.contains("roundhouse: 2 railtie and engine initializer(s), hook(s) and body statement(s) did not run (error[gem_railtie_not_run], error[railtie_body_statement_not_run])"), "{stderr}");
}

/// bugsnag 5.5's railtie, reduced: its `bugsnag.use_rack_middleware`
/// initializer writes `app.config.middleware` (the application's
/// MiddlewareStackProxy; boot names the operation, as the tree has no
/// stack to apply it to), and its after_initialize hook reads
/// `Rails.configuration.filter_parameters`, which an app initializer
/// sets. Boot runs the Finisher part (to_prepare, after_initialize) after
/// the app's code and initializers, so the hook sees the app's list.
#[test]
fn a_railtie_s_finisher_hooks_run_after_the_app_s_initializers() {
    let (dir, _) = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\ndatabase_adapter: postgresql\n")
        .write("config/database.yml", "test:\n  adapter: postgresql\n  database: rh_rails_railtie_port\n")
        .write("config/application.rb", "require \"rails\"\n\nmodule RailtieApp\n  class Application < Rails::Application\n  end\nend\n")
        .write("config/initializers/filter_parameter_logging.rb", "Rails.application.config.filter_parameters += [:passw, \"secret\"]\n")
        .write("lib/oracle.rb", "class Oracle\nend\n")
        .emit(BuildTarget::Ruby);
    let boot = std::fs::read_to_string(dir.join("boot.rb")).expect("boot.rb");
    let models = boot.find("require_relative \"app/models\"").expect("app/models");
    let finish = boot.find("Rails::Railtie.run_initializers_of_railties(Rails.application, :finish)").unwrap_or_else(|| panic!("{boot}"));
    assert!(models < finish, "{boot}");
    let out = emit_and_run::ruby()
        .current_dir(&*dir)
        .args([
            "-e",
            "require_relative 'runtime/active_support_lazy_load_hooks'\n\
             $LOAD_PATH.unshift(File.expand_path('runtime/features'))\n\
             require_relative 'runtime/module_delegate'\n\
             require_relative 'runtime/active_support_inflector'\n\
             require_relative 'runtime/rails'\n\
             require_relative 'runtime/rails_configuration'\n\
             require_relative 'config/application'\n\
             require 'rails/railtie'\n\
             FILTERS = []\n\
             class Bugsnagish < Rails::Railtie\n\
               config.after_initialize do\n\
                 FILTERS.concat(::Rails.configuration.filter_parameters.map { |f| f.is_a?(Symbol) || f.is_a?(String) ? /\\A#{f}\\z/ : f })\n\
               end\n\
               initializer 'bugsnagish.use_rack_middleware' do |app|\n\
                 begin\n\
                   app.config.middleware.insert_after ActionDispatch::DebugExceptions, :BugsnagRack\n\
                 rescue\n\
                   app.config.middleware.use :BugsnagRack\n\
                 end\n\
               end\n\
             end\n\
             Rails::Railtie.run_initializers_of_railties(Rails.application, :boot)\n\
             puts FILTERS.inspect\n\
             Rails::Railtie.run_initializers_of_railties(Rails.application, :finish)\n\
             puts FILTERS.inspect\n\
             puts Rails.configuration.middleware.equal?(Bugsnagish.config.app_middleware)\n",
        ])
        .output()
        .expect("ruby");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stdout}\n{stderr}");
    assert_eq!(stdout, "[]\n[/\\Apassw\\z/, /\\Asecret\\z/]\ntrue\n", "{stderr}");
    assert!(
        stderr.contains("roundhouse: error[gem_railtie_middleware_not_applied]: config.app_middleware.use(:BugsnagRack)"),
        "{stderr}"
    );
    assert!(!stderr.contains("gem_railtie_not_run"), "{stderr}");
}

/// worldwide 1.29.0's railtie, reduced: its `worldwide.setup`
/// initializer appends the gem's namespace to
/// `application.config.eager_load_namespaces`, which in railties is the
/// one `@@eager_load_namespaces` list every Railtie::Configuration
/// shares (Application::Configuration inherits it; the Finisher's
/// `eager_load!` walks it). The application config answers that list,
/// so the initializer runs instead of failing as gem_railtie_not_run.
#[test]
fn a_railtie_appends_to_the_application_s_eager_load_namespaces() {
    let (dir, _) = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\ndatabase_adapter: postgresql\n")
        .write("config/database.yml", "test:\n  adapter: postgresql\n  database: rh_rails_railtie_port\n")
        .write("config/application.rb", "require \"rails\"\n\nmodule RailtieApp\n  class Application < Rails::Application\n  end\nend\n")
        .write("lib/oracle.rb", "class Oracle\nend\n")
        .emit(BuildTarget::Ruby);
    let out = emit_and_run::ruby()
        .current_dir(&*dir)
        .args([
            "-e",
            "require_relative 'runtime/active_support_lazy_load_hooks'\n\
             $LOAD_PATH.unshift(File.expand_path('runtime/features'))\n\
             require_relative 'runtime/module_delegate'\n\
             require_relative 'runtime/active_support_inflector'\n\
             require_relative 'runtime/rails'\n\
             require_relative 'runtime/rails_configuration'\n\
             require_relative 'config/application'\n\
             require 'rails/railtie'\n\
             module Worldish\n\
               class Railtie < ::Rails::Railtie\n\
                 initializer 'worldish.setup' do |application|\n\
                   application.config.eager_load_namespaces << Worldish\n\
                 end\n\
               end\n\
             end\n\
             Rails::Railtie.run_initializers_of_railties(Rails.application, :boot)\n\
             puts Rails.configuration.eager_load_namespaces.include?(Worldish)\n\
             puts Rails.application.config.eager_load_namespaces.equal?(Worldish::Railtie.config.eager_load_namespaces)\n",
        ])
        .output()
        .expect("ruby");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stdout}\n{stderr}");
    assert_eq!(stdout, "true\ntrue\n", "{stderr}");
    assert!(!stderr.contains("gem_railtie_not_run"), "{stderr}");
}

/// A native gem's railtie requiring one of the app's own gems never loads
/// the gem home's copy over the compiled gem. Shopify Core's
/// shopify_session 7.40 railtie initializer requires shopify_session/rack,
/// whose utils/coder.rb requires "paquito": the gem home's paquito-1.0.2
/// loaded and redefined every constant of Core's compiled paquito. The
/// copy is registered as loaded, not loaded; the require raises
/// LoadError, and the initializer is named not run with the reason.
#[test]
fn a_railtie_cannot_load_the_gem_home_copy_of_a_compiled_gem() {
    const PATH_LOCK: &str = "PATH\n  remote: gems/paqlike\n  specs:\n    paqlike (1.0.2)\n\nGEM\n  remote: https://rubygems.org/\n  specs:\n    sessionish (7.40.0)\n      paqlike\n\nDEPENDENCIES\n  paqlike!\n  sessionish\n";
    let (dir, _) = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\ndatabase_adapter: postgresql\n")
        .write("config/database.yml", "test:\n  adapter: postgresql\n  database: rh_rails_railtie_port\n")
        .write("Gemfile.lock", PATH_LOCK)
        .write("lib/oracle.rb", "class Oracle\nend\n")
        .emit(BuildTarget::Ruby);
    let facades = std::fs::read_to_string(dir.join("runtime/gem_facades.rb")).expect("gem_facades.rb");
    let start = facades.find("RH_COMPILED_GEM_COPIES = [\"paqlike\"]").unwrap_or_else(|| panic!("{facades}"));
    let block = &facades[start..];
    let block = &block[..block.find("end.to_h.freeze\n").expect("block end") + "end.to_h.freeze\n".len()];

    let home = dir.join("tmp-gem-home");
    let gems: [(&str, Vec<(&str, &str)>, &str); 2] = [
        ("paqlike-1.0.2", vec![("paqlike.rb", "module Paqlike\n  NATIVE = true\n  ORIGIN = :native\nend\n")], ""),
        (
            "sessionish-7.40.0",
            vec![
                ("sessionish.rb", "module Sessionish\nend\nrequire 'sessionish/railtie' if defined?(::Rails::Railtie)\n"),
                (
                    "sessionish/railtie.rb",
                    "module Sessionish\n  class Railtie < ::Rails::Railtie\n    initializer 'sessionish.configure' do\n      require 'sessionish/coder'\n    end\n    initializer('sessionish.after') { RAN << :after }\n  end\nend\n",
                ),
                ("sessionish/coder.rb", "require 'paqlike'\n\nmodule Sessionish\n  CODER = Paqlike::ORIGIN\nend\n"),
            ],
            "  s.add_dependency \"paqlike\"\n",
        ),
    ];
    for (gem, files, deps) in gems {
        let (name, version) = gem.rsplit_once('-').unwrap();
        let lib = home.join("gems").join(gem).join("lib");
        for (path, body) in &files {
            let file = lib.join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, body).unwrap();
        }
        std::fs::create_dir_all(home.join("specifications")).unwrap();
        let listed: Vec<String> = files.iter().map(|(p, _)| format!("\"lib/{p}\"")).collect();
        std::fs::write(
            home.join(format!("specifications/{gem}.gemspec")),
            format!(
                "Gem::Specification.new do |s|\n  s.name = \"{name}\"\n  s.version = \"{version}\"\n  s.summary = \"x\"\n  s.authors = [\"x\"]\n  s.files = [{}]\n  s.require_paths = [\"lib\"]\n{deps}end\n",
                listed.join(", ")
            ),
        )
        .unwrap();
    }
    let script = format!(
        "Gem.paths = {{ 'GEM_PATH' => [ENV['RH_TEST_GEMS'], *Gem.path].join(File::PATH_SEPARATOR) }}\n\
         {block}\
         require_relative 'runtime/active_support_lazy_load_hooks'\n\
         $LOAD_PATH.unshift(File.expand_path('runtime/features'))\n\
         require_relative 'runtime/module_delegate'\n\
         require_relative 'runtime/active_support_inflector'\n\
         require_relative 'runtime/rails'\n\
         # The compiled gem (app/models/paqlike.rb in the tree).\n\
         module Paqlike\n  ORIGIN = :compiled\nend\n\
         RAN = []\n\
         require 'rails/railtie'\n\
         require 'sessionish'\n\
         Rails::Railtie.run_initializers_of_railties(Rails.application)\n\
         puts Paqlike::ORIGIN\n\
         puts defined?(Paqlike::NATIVE).inspect\n\
         puts RH_COMPILED_GEM_COPIES.inspect\n\
         puts RAN.inspect\n\
         puts Gem.loaded_specs['sessionish'].version\n\
         puts $LOADED_FEATURES.grep(%r{{/paqlike-}}).inspect\n"
    );
    let out = emit_and_run::ruby()
        .current_dir(&*dir)
        .env("RH_TEST_GEMS", &home)
        .args(["-e", &script])
        .output()
        .expect("ruby");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stdout}\n{stderr}");
    assert_eq!(stdout, "compiled\nnil\n{\"paqlike\" => \"1.0.2\"}\n[:after]\n7.40.0\n[]\n", "{stderr}");
    assert!(
        stderr.contains("roundhouse: error[gem_railtie_not_run]: Sessionish::Railtie initializer \"sessionish.configure\" did not run: LoadError: cannot load such file -- paqlike (error[compiled_gem_native_copy]: \"paqlike\" is a file of the paqlike gem this tree carries compiled; the gem home's paqlike-1.0.2 is not loaded over it)"),
        "{stderr}"
    );
    assert!(!stderr.contains("already initialized constant"), "{stderr}");
}

/// `Rails.app` / `Application#revision` / `Rails.version` (railties
/// 8.2.0.alpha rails.rb, application.rb:396-424, gem_version.rb), on the
/// emitted runtime. Each case runs in its own process (the revision is
/// read once per process) from a scratch Rails.root; the expected lines
/// are what native railties 5813559f answers for the same root and env.
#[test]
fn rails_app_revision_reads_env_then_the_revision_file_then_git() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static SEQ: AtomicUsize = AtomicUsize::new(0);
    let (dir, _) = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\ndatabase_adapter: postgresql\n")
        .write("config/database.yml", "test:\n  adapter: postgresql\n  database: rh_rails_railtie_port\n")
        .write("lib/oracle.rb", "class Oracle\nend\n")
        .emit(BuildTarget::Ruby);
    let script = "require File.join(ARGV[0], 'runtime/active_support_core_ext')\n\
                  require File.join(ARGV[0], 'runtime/rails_railtie')\n\
                  p [Rails.version, Rails.gem_version.to_s, Rails.app.is_a?(Rails::Application)]\n\
                  p Rails.app.revision\n\
                  p Rails.application.revision\n\
                  Rails.app.revision = 42\n\
                  p Rails.app.revision\n";
    let case = |revision_env: Option<&str>, revision_file: Option<&str>| {
        let root = std::env::temp_dir().join(format!(
            "rh-rails-revision-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        if let Some(body) = revision_file {
            std::fs::write(root.join("REVISION"), body).unwrap();
        }
        let mut cmd = emit_and_run::ruby();
        cmd.current_dir(&root).env_remove("REVISION").args(["-e", script, &dir.to_string_lossy()]);
        if let Some(v) = revision_env {
            cmd.env("REVISION", v);
        }
        let out = cmd.output().expect("ruby");
        let _ = std::fs::remove_dir_all(&root);
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let head = "[\"8.2.0.alpha\", \"8.2.0.alpha\", true]\n";
    assert_eq!(case(Some("abc123"), None), format!("{head}\"abc123\"\n\"abc123\"\n\"42\"\n"));
    // A blank ENV["REVISION"] falls through to the file, stripped.
    assert_eq!(case(Some("   "), Some("  rev-file \n")), format!("{head}\"rev-file\"\n\"rev-file\"\n\"42\"\n"));
    // A blank file is nil, with no git fallback (only a read error falls back).
    assert_eq!(case(None, Some("\n")), format!("{head}nil\nnil\n\"42\"\n"));
    // No file and a root outside any git checkout: git fails, nil.
    assert_eq!(case(None, None), format!("{head}nil\nnil\n\"42\"\n"));
}

/// A railtie's and an isolated engine's names go through the app's
/// acronyms, as railties' `generate_railtie_name` and `isolate_namespace`
/// call ActiveSupport::Inflector.underscore: the port (loaded with the
/// gems, runtime/gem_facades.rb) and native Rails 8.2.0.alpha
/// (tests/support/rails_oracle.rb) agree.
#[test]
fn railtie_and_engine_names_follow_the_apps_acronyms_as_rails() {
    const PROGRAM: &str = "ActiveSupport::Inflector.inflections(:en) { |i| i.acronym 'GraphQL' }\n\
                           class GraphQLTie < Rails::Railtie; end\n\
                           class APIKeyTie < Rails::Railtie; end\n\
                           module GraphQLKit\n  class Engine < Rails::Engine\n    isolate_namespace GraphQLKit\n  end\nend\n\
                           p [GraphQLTie.railtie_name, APIKeyTie.railtie_name, GraphQLKit::Engine.engine_name, GraphQLKit::Engine.config.default_scope, GraphQLKit::Engine.isolated?]\n";
    // The engine's root is the nearest directory above its file with a
    // lib/ (`find_root_with_flag "lib"`): each program runs from one.
    let probe = std::env::temp_dir().join(format!("rh_railtie_names_{}", std::process::id()));
    std::fs::create_dir_all(probe.join("lib")).unwrap();
    let run = |name: &str, header: String| {
        let script = probe.join(name);
        std::fs::write(&script, format!("{header}{PROGRAM}")).unwrap();
        emit_and_run::ruby().arg(&script).current_dir(&probe).output().expect("ruby")
    };
    let oracle = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/rails_oracle.rb");
    let native = run("native.rb", format!("require {oracle:?}\nRailsOracle.activate(\"railties\")\nrequire 'rails'\n"));
    let native_out = String::from_utf8_lossy(&native.stdout).into_owned();
    assert!(native.status.success(), "native: {native_out}\n{}", String::from_utf8_lossy(&native.stderr));
    assert_eq!(native_out, "[\"graphql_tie\", \"api_key_tie\", \"graphql_kit\", {module: \"graphql_kit\"}, true]\n");
    let (dir, _) = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\ndatabase_adapter: postgresql\n")
        .write("config/database.yml", "test:\n  adapter: postgresql\n  database: rh_rails_railtie_port\n")
        .write("lib/oracle.rb", "class Oracle\nend\n")
        .emit(BuildTarget::Ruby);
    let runtime = dir.join("runtime");
    let out = run(
        "emitted.rb",
        format!(
            "require File.join({runtime:?}, 'active_support_lazy_load_hooks')\n\
             $LOAD_PATH.unshift(File.join({runtime:?}, 'features'))\n\
             require File.join({runtime:?}, 'module_delegate')\n\
             require File.join({runtime:?}, 'active_support_inflector')\n\
             require 'rails'\n"
        ),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(stdout, native_out);
    std::fs::remove_dir_all(&probe).unwrap();
}
