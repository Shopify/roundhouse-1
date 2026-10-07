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
    let run = boot.find("Rails::Railtie.run_initializers_of_railties(Rails.application)").unwrap_or_else(|| panic!("{boot}"));
    let gems = boot.find("require_relative \"runtime/gem_facades\"").expect("gem facades");
    let models = boot.find("require_relative \"app/models\"").expect("app/models");
    assert!(gems < run && run < models, "{boot}");
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
    assert!(stderr.contains("roundhouse: 2 gem railtie initializer(s) and hook(s) did not run (error[gem_railtie_not_run])"), "{stderr}");
}
