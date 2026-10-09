//! A Rails-dependent gem's test helper carried as a gem source
//! (`gem_sources:`) is the app's code: a test class's `include` of it is
//! lowered and its helpers run in the emitted test. Shopify Core's sampled
//! tests include `Cusco::TestHelper` (GraphApi mutation tests; Core's
//! BackgroundQueue::Job also names `Cusco::Middleware::Error`),
//! `MarketManagers::TestHelpers` and `ExtensionsRuntime::Testing::TestHelper`;
//! cusco and market_managers are `RAILS_DEPENDENT_GEMS`, so behind their
//! RBIs each constant was error[gem_requires_rails] (ctl capped probe, e2e
//! round 4: cusco 58 rows, market_managers 15). The harness now carries
//! them (bin/check-target GEM_SOURCES).
//!
//! The helper here is cusco's TestHelper cut to a stub and a lookup that
//! raises the gem's own error class; config/application.rb's
//! `Bundler.require` loads the gem's entry, which requires both files.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    activesupport (8.0.2)\n    cusco (3.1.0)\n\nDEPENDENCIES\n  activesupport\n  cusco\n";

const RBI: &str = "# typed: true\nmodule Cusco\n  module TestHelper\n    def stub_cusco_response(name); end\n  end\n  module Middleware\n    class Error < StandardError; end\n  end\nend\n";

const ENTRY: &str = "require \"active_support\"\nrequire \"cusco/middleware/error\"\nrequire \"cusco/test_helper\"\n\nmodule Cusco\n  VERSION = \"3.1.0\"\nend\n";

const ERROR: &str = "module Cusco\n  module Middleware\n    class Error < StandardError\n      def initialize(name)\n        super(\"cusco request #{name} failed\")\n      end\n    end\n  end\nend\n";

const HELPER: &str = "module Cusco\n  module TestHelper\n    def stub_cusco_response(name)\n      @cusco_stubs ||= {}\n      @cusco_stubs[name] = \"stubbed #{name}\"\n    end\n\n    def cusco_response(name)\n      stubs = @cusco_stubs || {}\n      stubs.fetch(name) { raise Cusco::Middleware::Error.new(name) }\n    end\n  end\nend\n";

/// Core's config/application.rb runs `Bundler.require(*Rails.groups)`,
/// which loads lib/cusco.rb (and through it the helper and the error).
const APPLICATION: &str = "require_relative \"boot\"\n\nrequire \"rails\"\nrequire \"active_record/railtie\"\n\nBundler.require(*Rails.groups)\n\nmodule CuscoProbeApp\n  class Application < Rails::Application\n  end\nend\n";

const CARRIED: &str = "test_paths:\n  - test\ngem_sources:\n  cusco: vendor/gems/cusco-3.1.0\n";

const STUB_TEST: &str = r#"require "test_helper"

class StubTest < ActiveSupport::TestCase
  include Cusco::TestHelper

  test "the carried helper stubs and its error raises" do
    stub_cusco_response("shop")
    assert_equal "stubbed shop", cusco_response("shop")
    error = assert_raises(Cusco::Middleware::Error) { cusco_response("order") }
    assert_equal "cusco request order failed", error.message
    assert_equal true, StubTest.include?(Cusco::TestHelper)
  end
end
"#;

fn app(config: &str) -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("roundhouse.yml", config)
        .write(
            "Gemfile",
            "source \"https://rubygems.org\"\n\ngem \"activesupport\"\ngem \"cusco\"\n",
        )
        .write("Gemfile.lock", LOCK)
        .write("sorbet/rbi/gems/cusco@3.1.0.rbi", RBI)
        .write("vendor/gems/cusco-3.1.0/lib/cusco.rb", ENTRY)
        .write(
            "vendor/gems/cusco-3.1.0/lib/cusco/middleware/error.rb",
            ERROR,
        )
        .write("vendor/gems/cusco-3.1.0/lib/cusco/test_helper.rb", HELPER)
        .write("config/boot.rb", "require \"bundler/setup\"\n")
        .write("config/application.rb", APPLICATION)
        .write(
            "config/routes.rb",
            "Rails.application.routes.draw do\nend\n",
        )
        .write(
            "test/test_helper.rb",
            "require \"active_support/test_case\"\n",
        )
        .write("test/models/stub_test.rb", STUB_TEST)
}

#[test]
fn a_carried_rails_dependent_gem_s_test_helper_include_is_not_refused() {
    let (emitted, errors) = app(CARRIED).emit(BuildTarget::Ruby);
    assert!(
        !errors.iter().any(|e| e.contains("gem_requires_rails")),
        "{errors:#?}"
    );
    let test =
        std::fs::read_to_string(emitted.join("test/models/stub_test.rb")).expect("stub_test.rb");
    assert!(!test.contains("gem_requires_rails"), "{test}");
    assert!(test.contains("Cusco::TestHelper"), "{test}");
}

#[test]
fn a_carried_rails_dependent_gem_s_test_helper_runs_in_emitted_ruby() {
    app(CARRIED)
        .run_test("test/models/stub_test.rb")
        .assert_passes();
}
