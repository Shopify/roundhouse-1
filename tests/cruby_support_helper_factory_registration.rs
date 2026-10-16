//! A test-support concern whose `included do` block registers factories
//! behind an `if fixture_factory_definitions.empty?` guard (Core's
//! `Merchandising::CatalogTestHelper`). The block's `if` was refused as a
//! statement the shared-test splice does not model, which left every test
//! of the including classes out of the test index (`not_in_index`).
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

/// The shape of Core's `ActiveSupport::TestCase` reopen and
/// `FixtureFactory::Registry::ClassMethods`: a per-class registry and the
/// class methods that fill it.
const REGISTRY: &str = r##"class ActiveSupport::TestCase
  class_attribute :fixture_factory_definitions, default: {}

  class << self
    def define_factories(&block)
      self.fixture_factory_definitions = {}
      instance_exec(&block) if block
    end

    def factory(name, options = {})
      fixture_factory_definitions[name] = options
    end
  end
end
"##;

const HELPER: &str = r##"module CatalogHelper
  extend ActiveSupport::Concern

  module Factories
    class << self
      def define(klass)
        klass.factory(:shop, class_name: "Shop")
        klass.factory(:catalog, class_name: "Catalog")
      end
    end
  end

  included do
    setup do
      @catalog_ready = true
    end

    if fixture_factory_definitions.empty?
      define_factories { Factories.define(self) }
    else
      Factories.define(self)
    end
  end
end
"##;

const TEST: &str = r##"class CatalogTest < ActiveSupport::TestCase
  include CatalogHelper

  test "the guard registered the helper's factories on the class" do
    assert @catalog_ready
    assert_equal [:shop, :catalog], self.class.fixture_factory_definitions.keys
  end
end
"##;

const PRESET: &str = r##"class ClearedCatalogTest < ActiveSupport::TestCase
  define_factories { factory(:own, class_name: "Own") }
  include CatalogHelper

  test "a registry that already has factories gets the helper's on top" do
    assert @catalog_ready
    assert_equal [:own, :shop, :catalog], self.class.fixture_factory_definitions.keys
  end
end
"##;

fn run(test: &str, path: &str) -> emit_and_run::Run {
    emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("app/controllers/application_controller.rb", "class ApplicationController < ActionController::Base\nend\n")
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write("lib/registry.rb", REGISTRY)
        .write("test/support/helpers/catalog_helper.rb", HELPER)
        .write(path, test)
        .run_test(path)
}

#[test]
fn an_included_blocks_guarded_factory_registration_runs_on_the_including_class() {
    let run = run(TEST, "test/models/catalog_test.rb");
    assert!(run.success, "{}\n{}\n{}", run.errors.join("\n"), run.stdout, run.stderr);
    assert!(run.stdout.contains("CatalogTest: 1 tests passed"), "{}\n{}", run.stdout, run.stderr);
    assert!(!run.stderr.contains("test_class_statement_not_run"), "{}", run.stderr);
}

#[test]
fn the_guards_else_branch_adds_to_a_registry_the_class_already_has() {
    let run = run(PRESET, "test/models/cleared_catalog_test.rb");
    assert!(run.success, "{}\n{}\n{}", run.errors.join("\n"), run.stdout, run.stderr);
    assert!(run.stdout.contains("ClearedCatalogTest: 1 tests passed"), "{}\n{}", run.stdout, run.stderr);
}
