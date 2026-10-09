//! The app's own `test/test_helper.rb` require chain: Core's helper
//! requires `platform/testing/test_helper`, which requires a runtime file
//! and globs an engine's `test/support/initializers`, whose
//! `active_support_test_case.rb` reopens `ActiveSupport::TestCase` to
//! include the shared helpers (`ActiveRecordTestHelper#build_fixture`,
//! `SqlAssertions#assert_sql`). Natively every test's `require
//! "test_helper"` runs that chain; the emitted test must too.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const APP_CONTROLLER: &str = "class ApplicationController < ActionController::Base\nend\n";
const ROUTES: &str = "Rails.application.routes.draw do\nend\n";

// Core's shape: the runtime is required inside the helper's own load
// block (`StatsdMetrics.measure_load_test_environment do … end`), which
// the harness's helper does not run.
const TEST_HELPER: &str = r##"# frozen_string_literal: true

module LoadMetrics
  def self.measure
    yield
  end
end

LoadMetrics.measure do
  require "rails/test_help"
  require "support/test_runtime"
end
"##;

const TEST_RUNTIME: &str = r##"# frozen_string_literal: true

Thread.current[:__probe_runtime_loading] = true
begin
  require "support/helpers/widget_assertions"
ensure
  Thread.current[:__probe_runtime_loading] = false
end
"##;

const BASE_ASSERTIONS: &str = r##"# frozen_string_literal: true

module BaseAssertions
  def assert_base(value)
    assert_equal :base, value
  end
end
"##;

const WIDGET_ASSERTIONS: &str = r##"# frozen_string_literal: true

require "support/helpers/base_assertions"

module WidgetAssertions
  def assert_widget(value)
    assert_equal :widget, value
  end
end

module ActiveSupport
  class TestCase
    include BaseAssertions
    include WidgetAssertions
  end
end
"##;

const WIDGET_TEST: &str = r##"# frozen_string_literal: true

require "test_helper"

module Foo
  class WidgetTest < ActiveSupport::TestCase
    test "uses a helper the test helper chain includes" do
      assert_widget :widget
      assert_base :base
    end
  end
end
"##;

fn app() -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("app/controllers/application_controller.rb", APP_CONTROLLER)
        .write("config/routes.rb", ROUTES)
        .write("config/environments/test.rb", "Rails.application.configure do\n  config.eager_load = false\nend\n")
        .write("roundhouse.yml", "test_paths:\n  - test/support\n  - components/foo/test/models/foo\n")
        .write("test/test_helper.rb", TEST_HELPER)
        .write("test/support/test_runtime.rb", TEST_RUNTIME)
        .write("test/support/helpers/base_assertions.rb", BASE_ASSERTIONS)
        .write("test/support/helpers/widget_assertions.rb", WIDGET_ASSERTIONS)
        .write("components/foo/test/models/foo/widget_test.rb", WIDGET_TEST)
}

#[test]
fn test_helper_chain_reaches_every_test() {
    app().run_test("test/models/foo_widget_test.rb").assert_passes();
}

/// The test requires the runtime's program after the harness's helper,
/// where its `require "test_helper"` ran the app's: an eager-loaded app
/// would define the helpers regardless, a test env that is not must not
/// depend on that. `rails/test_help` is Rails', no program of the app's.
#[test]
fn emitted_test_requires_the_helpers_programs() {
    let (emitted, _) = app().emit(roundhouse::project::BuildTarget::Ruby);
    let test = std::fs::read_to_string(emitted.join("test/models/foo_widget_test.rb")).expect("emitted test");
    let helper = test.find("require_relative \"../test_helper\"\n").expect("harness helper required");
    let runtime = test
        .find("RoundhouseAutoload.file_statement(\"test/test_helper.rb:11\") { require_relative \"../../app/models/roundhouse_files/test/support/test_runtime\" }\n")
        .unwrap_or_else(|| panic!("the helper's runtime program is required:\n{test}"));
    assert!(helper < runtime, "after the harness helper:\n{test}");
    assert!(!test.contains("test_help\""), "Rails' own feature is the harness's:\n{test}");
}
