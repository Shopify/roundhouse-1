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

// Core's essentials shape: the runtime requires a support initializer
// that reopens `ActiveSupport::TestCase` to include a helper, and the
// helper's file prepends a compatibility module into a library class
// (`FasterFixtures::FixtureSet.prepend ModelUuidFixtureCompatibility`)
// as it loads. Both the include and the prepend are the helper chain's.
// Core's test_runtime.rb requires its runtime inside `begin … ensure`,
// and the tree has no port of every feature it names (Core's first is
// `active_resource/http_mock`; here a Rails feature with no port). The
// initializer is required relatively: Core's `require "support/…"` is
// answered on the test load path only under `Rails.env.test?`, and this
// probe's tree runs as development.
const PREPEND_RUNTIME: &str = r##"# frozen_string_literal: true

Thread.current[:__probe_runtime_loading] = true
begin
  require "active_support/core_ext/hash/indifferent_access"
  require_relative "initializers/active_support_test_case"
ensure
  Thread.current[:__probe_runtime_loading] = false
end
"##;

const PREPEND_INITIALIZER: &str = r##"# frozen_string_literal: true

require "support/helpers/active_record_test_helper"

module ActiveSupport
  class TestCase
    include ActiveRecordTestHelper
  end
end
"##;

const PREPEND_HELPER: &str = r##"# frozen_string_literal: true

require "widgets/fixture_set"
require "support/helpers/uuid_fixture_compatibility"

Widgets::FixtureSet.prepend(UuidFixtureCompatibility)

module ActiveRecordTestHelper
  def fixture_id(label)
    Widgets::FixtureSet.new.identify(label)
  end
end
"##;

const PREPEND_COMPAT: &str = r##"# frozen_string_literal: true

module UuidFixtureCompatibility
  def identify(label)
    "model-uuid:#{super}"
  end
end
"##;

const PREPEND_FIXTURE_SET: &str = r##"# frozen_string_literal: true

module Widgets
  class FixtureSet
    def identify(label)
      "v5:#{label}"
    end
  end
end
"##;

const PREPEND_TEST: &str = r##"# frozen_string_literal: true

require "test_helper"

module Foo
  class FixtureIdTest < ActiveSupport::TestCase
    test "the helper chain's prepend reaches the fixture set" do
      assert_equal "model-uuid:v5:snowdevil", fixture_id(:snowdevil)
      assert_equal [UuidFixtureCompatibility, Widgets::FixtureSet], Widgets::FixtureSet.ancestors.first(2)
    end
  end
end
"##;

fn prepend_app() -> emit_and_run::Overlay {
    app()
        .write("test/support/test_runtime.rb", PREPEND_RUNTIME)
        .write("test/support/initializers/active_support_test_case.rb", PREPEND_INITIALIZER)
        .write("test/support/helpers/active_record_test_helper.rb", PREPEND_HELPER)
        .write("test/support/helpers/uuid_fixture_compatibility.rb", PREPEND_COMPAT)
        .write("lib/widgets/fixture_set.rb", PREPEND_FIXTURE_SET)
        .write("components/foo/test/models/foo/fixture_id_test.rb", PREPEND_TEST)
}

/// The require the tree cannot answer is named, and the initializer
/// after it in the `begin` still loads, so the include and the helper
/// file's prepend reach the test.
#[test]
fn test_helper_chain_runs_the_helpers_prepend() {
    let run = prepend_app().run_test("test/models/foo_fixture_id_test.rb");
    run.assert_passes();
    assert!(
        run.stderr.contains("the file-level statement at test/support/test_runtime.rb:5 did not run: "),
        "the unported feature's require is named:\n{}",
        run.stderr
    );
}
