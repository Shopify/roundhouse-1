//! The emitted CRuby tree's boot loads files in the order native boot
//! does, in three places Shopify Core's unit-1000 sample reached:
//!
//! - test/test_helper.rb activates the locked bundled gems before its own
//!   requires, as Bundler.setup runs before any of a Rails test's. mocha
//!   2.5.0's parameter_matchers/equivalent_uri.rb requires "cgi"; ahead of
//!   the activation that loaded Ruby 4.0's stub cgi.rb, the locked cgi
//!   0.4.2 never loaded, and vcr's `CGI.method(:parse)` raised NameError
//!   for every booted Core test.
//! - A boot file nested under a namespace whose own file is an autoload
//!   loads after that file, as Zeitwerk loads a namespace's file before
//!   its children's. Core's pagination/ast/elastic_search/*.rb loaded
//!   before pagination/ast.rb, which required them back (a circular
//!   require, an error under Core's StrictWarnings).
//! - An autoload the app's own code registers for a file of the tree
//!   (Component::Config.inherited's `config.autoload(:Test, ".../test.rb")`
//!   under the path `const_source_location` gives) loads that file's
//!   program, which runs its requires, not its bare declarations. Core's
//!   PlatformComponent::Essentials::Config::Test then raised NameError on
//!   ActiveSupport::Cache::TestProxyCacheStore, which only its program's
//!   `require` loads.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

/// Core's lock: cgi 0.4.2 (no longer a default gem in Ruby 4.0), mocha,
/// and dalli, a gem the tree's runtime/gem_facades.rb requires.
const LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    activesupport (8.0.2)\n    cgi (0.4.2)\n    dalli (3.2.9)\n    mocha (2.5.0)\n      ruby2_keywords (>= 0.0.5)\n    ruby2_keywords (0.0.5)\n\nDEPENDENCIES\n  activesupport\n  cgi\n  dalli\n  mocha\n";

/// The gem the tree declares and requires on demand.
const GEMS: &str = "runtime_gems:\n  - {constant: Dalli, gem: dalli}\n";

fn mocha_app() -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\nextensions:\n  - ext\n")
        .write("ext/gems.yml", GEMS)
        .write("Gemfile.lock", LOCK)
        .write(
            "sorbet/rbi/gems/dalli@3.2.9.rbi",
            "# typed: true\nmodule Dalli\n  class << self\n    def register(middleware); end\n  end\nend\nmodule Dalli::OpentelemetryMiddleware; end\n",
        )
        .write(
            "lib/cache_config.rb",
            "class CacheConfig\n  def initialize\n    Dalli.register(Dalli::OpentelemetryMiddleware)\n  end\nend\n",
        )
        .write("app/models/probe_token.rb", "class ProbeToken\n  def self.value\n    \"real\"\n  end\nend\n")
        .write(
            "test/models/probe_token_test.rb",
            r#"require "test_helper"

class ProbeTokenTest < ActiveSupport::TestCase
  test "a stubbed token" do
    ProbeToken.stubs(:value).returns("stubbed")
    assert_equal "stubbed", ProbeToken.value
    assert_equal 32, SecureRandom.hex(16).length
  end
end
"#,
        )
}

fn leading_requires(content: &str) -> Vec<&str> {
    content.lines().skip_while(|line| line.starts_with('#')).take_while(|line| line.starts_with("require")).collect()
}

#[test]
fn the_test_helper_activates_the_locked_bundled_gems_before_its_own_requires() {
    let (dir, errors) = mocha_app().emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let pins = std::fs::read_to_string(dir.join("runtime/bundled_gems.rb")).expect("bundled_gems");
    assert!(pins.contains("[\"cgi\", \"0.4.2\"]"), "{pins}");
    let helper = std::fs::read_to_string(dir.join("test/test_helper.rb")).expect("test_helper");
    let requires = leading_requires(&helper);
    assert_eq!(requires.first(), Some(&"require_relative \"../runtime/bundled_gems\""), "{requires:#?}");
    assert!(requires.contains(&"require \"mocha/api\""), "{requires:#?}");
    assert_eq!(helper.matches("runtime/bundled_gems").count(), 1, "{requires:#?}");
    // A test file's own stdlib requires come after its test helper, which
    // activates the gems first.
    let test = std::fs::read_to_string(dir.join("test/models/probe_token_test.rb")).expect("test file");
    let requires = leading_requires(&test);
    assert_eq!(requires.first(), Some(&"require_relative \"../test_helper\""), "{requires:#?}");
    assert!(requires.contains(&"require \"securerandom\""), "{requires:#?}");

    // A gem home holding the locked cgi and a mocha whose entry requires
    // "cgi" and, as vcr does, reads `CGI.method(:parse)` at load.
    let home = dir.join("tmp-bundled-gem-home");
    let gem = |name: &str, version: &str, files: &[(&str, &str)]| {
        let lib = home.join(format!("gems/{name}-{version}/lib"));
        for (path, body) in files {
            let file = lib.join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, body).unwrap();
        }
        std::fs::create_dir_all(home.join("specifications")).unwrap();
        std::fs::write(
            home.join(format!("specifications/{name}-{version}.gemspec")),
            format!("Gem::Specification.new do |s|\n  s.name = {name:?}\n  s.version = {version:?}\n  s.summary = \"x\"\n  s.authors = [\"x\"]\n  s.require_paths = [\"lib\"]\nend\n"),
        )
        .unwrap();
    };
    gem(
        "cgi",
        "0.4.2",
        &[(
            "cgi.rb",
            "class CGI\n  VERSION = \"0.4.2\"\n  def self.parse(query)\n    query.split(\"&\").each_with_object({}) { |pair, out| key, value = pair.split(\"=\", 2); (out[key] ||= []) << value }\n  end\nend\n",
        )],
    );
    // The locked dalli, which boot.rb requires: the locked-external wiring
    // requires a gem the app's code names where its name is its namespace.
    gem("dalli", "3.2.9", &[("dalli.rb", "module Dalli\n  VERSION = \"3.2.9\"\nend\n")]);
    // Above any installed mocha, so the helper's unpinned require finds it.
    gem(
        "mocha",
        "99.0.0",
        &[(
            "mocha/api.rb",
            "require \"cgi\"\nmodule Mocha\n  PARSE = CGI.method(:parse)\n  module API\n    def mocha_setup; end\n    def mocha_verify(*); end\n    def mocha_teardown(*); end\n  end\nend\n",
        )],
    );
    let installed = std::process::Command::new("ruby").args(["-e", "print Gem.path.join(File::PATH_SEPARATOR)"]).output().expect("ruby");
    let gem_path = format!("{}{}{}", home.display(), if cfg!(windows) { ";" } else { ":" }, String::from_utf8_lossy(&installed.stdout));
    let out = emit_and_run::ruby()
        .current_dir(&dir)
        .env("BLOG_DB", ":memory:")
        .env("GEM_PATH", &gem_path)
        .args(["-Itest", "-I.", "-e", "require \"test_helper\"; p CGI::VERSION; p Mocha::PARSE.call(\"a=1&a=2\")"])
        .output()
        .expect("ruby");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(stdout.lines().collect::<Vec<_>>(), ["\"0.4.2\"", "{\"a\" => [\"1\", \"2\"]}"], "{stdout}");
}

/// Core's components/platform/essentials/app/models/pagination/ast.rb,
/// cut down: a namespace's file whose nested Structs (reopened as classes
/// for their constants) make each of them a boot file of its own.
const PAGINATION_AST: &str = r#"module Pagination
  module Ast
    module NodeInterface
      def simplify
        self
      end
    end

    NoConditionsNode = Struct.new(:node_type) do
      include NodeInterface
    end

    module ElasticSearch
      FilterNode = Struct.new(:clauses, keyword_init: true) do
        include Pagination::Ast::NodeInterface
      end

      FilterChildNode = Struct.new(:verb, keyword_init: true) do
        include Pagination::Ast::NodeInterface

        def initialize(verb:)
          raise ArgumentError, "bad verb" unless FilterChildNode::VALID_VERBS.include?(verb)
          super(verb: verb)
        end
      end

      class FilterChildNode
        VALID_VERBS = [:has_child, :has_parent].freeze
      end
    end

    NO_CONDITIONS = NoConditionsNode.new(:none)
  end
end
"#;

#[test]
fn a_boot_file_nested_under_an_autoloaded_namespace_loads_after_the_namespace_file() {
    let run = emit_and_run::empty_app()
        .write("config/environments/development.rb", "Rails.application.configure do\n  config.eager_load = false\nend\n")
        .write("app/models/pagination/ast.rb", PAGINATION_AST)
        .run_ruby_env(
            "p Pagination::Ast::ElasticSearch::FilterChildNode::VALID_VERBS\n\
             p Pagination::Ast::ElasticSearch::FilterChildNode.new(verb: :has_child).verb\n\
             p Pagination::Ast::NO_CONDITIONS.simplify.node_type",
            &[("RUBYOPT", "-w")],
        );
    run.assert_passes();
    assert_eq!(run.stdout.lines().collect::<Vec<_>>(), ["[:has_child, :has_parent]", ":has_child", ":none"], "{}", run.stdout);
    assert!(!run.stderr.contains("circular require"), "{}", run.stderr);
    let models = std::fs::read_to_string(run.emitted.join("app/models.rb")).expect("models.rb");
    assert!(
        models.contains("\"models/pagination/ast/elastic_search/filter_child_node\" => [\"Pagination::Ast\", \"Pagination::Ast::ElasticSearch\"],"),
        "{}",
        models.lines().filter(|l| l.contains("pagination")).collect::<Vec<_>>().join("\n")
    );
}

/// gems/component/lib/component/config.rb: each environment's config is
/// an autoload the `inherited` hook registers next to the subclass's file,
/// reached by `Config.for(env)`'s `const_get`.
const COMPONENT_CONFIG: &str = r#"module Component
  class Config
    class << self
      def inherited(config)
        return super unless config.superclass == Config

        if (class_name = config.name) &&
            (config_location = Object.const_source_location(class_name)) &&
            (parent_dirname = File.dirname(config_location[0]))
          config_dirname = File.join(parent_dirname, "config")
          config.autoload(:Development, File.join(config_dirname, "development.rb"))
          config.autoload(:Test, File.join(config_dirname, "test.rb"))
          config.autoload(:Production, File.join(config_dirname, "production.rb"))
        end

        super
      end

      def for(env)
        case env
        when "development", "test", "production"
          const_get(env.capitalize)
        else
          raise "Unknown config for the environment: #{env}"
        end
      end
    end

    def initialize(env)
    end
  end
end
"#;

#[test]
fn an_autoload_the_app_registers_for_a_tree_file_loads_its_program() {
    let run = emit_and_run::empty_app()
        .write("config/environments/development.rb", "Rails.application.configure do\n  config.eager_load = false\nend\n")
        .write("config/application.rb", "require \"platform/essentials/config\"\n")
        .write("lib/component/config.rb", COMPONENT_CONFIG)
        .write(
            "lib/platform/essentials/config.rb",
            "require \"component/config\"\n\nmodule PlatformComponent\n  module Essentials\n    class Config < ::Component::Config\n      def store_name\n        \"base\"\n      end\n    end\n  end\nend\n",
        )
        // Core's config/test.rb requires active_support/cache/
        // test_proxy_cache_store first; that file requires its superclass
        // first, so its constant is no autoload and only this require
        // defines it.
        .write(
            "lib/platform/essentials/config/test.rb",
            r#"require "platform/testing/stores"

module PlatformComponent
  module Essentials
    class Config < ::Component::Config
      class Test < Config
        def initialize(env)
          super
          @store = Platform::Testing::TestStore.new(env.fetch("name", "test"))
        end

        def store_name
          @store.name
        end
      end
    end
  end
end
"#,
        )
        .write(
            "lib/platform/testing/stores.rb",
            "require \"platform/testing/base_store\"\n\nmodule Platform\n  module Testing\n    class TestStore < BaseStore\n      def initialize(name)\n        @name = name\n      end\n    end\n  end\nend\n",
        )
        .write(
            "lib/platform/testing/base_store.rb",
            "module Platform\n  module Testing\n    class BaseStore\n      attr_reader :name\n    end\n  end\nend\n",
        )
        .run_ruby(
            "p PlatformComponent::Essentials::Config.for(\"test\").new({\"name\" => \"probe\"}).store_name\n\
             p PlatformComponent::Essentials::Config::Test.superclass",
        );
    run.assert_passes();
    assert_eq!(run.stdout.lines().collect::<Vec<_>>(), ["\"probe\"", "PlatformComponent::Essentials::Config"], "{}", run.stdout);
}
