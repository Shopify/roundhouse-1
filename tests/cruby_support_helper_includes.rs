//! A test-support module (a file under a test root without a test class)
//! that Core's tests include: its `include`s of the app's own modules, a
//! `setup(prepend: true)` in its `included` block, and the test macro its
//! `ClassMethods` defines. Each was a refusal or a silent drop that left
//! every test of the including classes without a result.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const TEST_GENERATOR: &str = r##"module TestGenerator
  def generate_test(name, location = nil, tags: nil, &block)
    test(name, &block)
  end
end
"##;

/// `Markets::TestHelper` (a plain module outside the test roots).
const PLAIN: &str = "module PlainHelper\n  def plain_value\n    7\n  end\nend\n";

/// `FixtureFactory::Registry` (a concern of a gem).
const REGISTRY: &str = "module Registry\n  extend ActiveSupport::Concern\n\n  included do\n    setup do\n      @registry = []\n    end\n  end\nend\n";

/// Core's `AccessAndAuth::InputFieldsAuthorizationHelper`: a concern whose
/// `ClassMethods` defines the `test_*` macro its includers call, and which
/// includes the app's modules.
const HELPER: &str = r##"module CatalogHelper
  extend ActiveSupport::Concern
  include PlainHelper
  include Registry

  module ClassMethods
    include TestGenerator

    def check_catalog(key, expected:)
      generate_test("catalog #{key} is #{expected.to_sentence}") do
        assert_equal 7, plain_value
        assert_equal [], @registry
        assert_equal expected, [key]
      end
    end
  end
end
"##;

/// A shared setup whose `included` block prepends a callback (events_test's
/// `setup(prepend: true)`): it runs before the includer's own `setup`.
const PREPEND: &str = r##"module PrependHelper
  extend ActiveSupport::Concern

  included do
    setup(prepend: true) do
      @order = [:prepended]
    end
  end
end
"##;

const TEST: &str = r##"class CatalogTest < ActiveSupport::TestCase
  include CatalogHelper
  include PrependHelper

  setup do
    @order << :own
  end

  check_catalog(:vendor, expected: [:vendor])

  test "plain" do
    assert_equal 7, plain_value
    assert_equal [:prepended, :own], @order
  end
end
"##;

#[test]
fn a_support_modules_app_includes_prepended_setup_and_test_macro_run() {
    let run = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("app/controllers/application_controller.rb", "class ApplicationController < ActionController::Base\nend\n")
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write("lib/test_generator.rb", TEST_GENERATOR)
        .write("lib/plain_helper.rb", PLAIN)
        .write("lib/registry.rb", REGISTRY)
        .write("test/support/helpers/catalog_helper.rb", HELPER)
        .write("test/support/helpers/prepend_helper.rb", PREPEND)
        .write("test/models/catalog_test.rb", TEST)
        .run_test("test/models/catalog_test.rb");
    assert!(run.success, "{}\n{}\n{}", run.errors.join("\n"), run.stdout, run.stderr);
    assert!(run.stdout.contains("CatalogTest: 2 tests passed"), "{}\n{}", run.stdout, run.stderr);
    assert!(!run.stderr.contains("test_class_statement_not_run"), "{}", run.stderr);
}
