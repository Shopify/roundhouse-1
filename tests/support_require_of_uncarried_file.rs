//! A support file's `require` of an app file the tree carries no
//! declaration of: Ruby runs the required file, so the files it requires
//! in turn load too. Core's db test helper requires the read helper (a
//! Concern no test of the closure includes), and that file's own `require`
//! loads the fixtures module the db helper's `included` hook names.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const APP_CONTROLLER: &str = "class ApplicationController < ActionController::Base\nend\n";
const ROUTES: &str = "Rails.application.routes.draw do\nend\n";

const FIXTURES: &str = r##"# frozen_string_literal: true

module Widgets
  module Fixtures
    def fixture_label
      :fixtures
    end
  end
end
"##;

// A test mixin (its `included` block registers a callback): ingest
// splices it into the test classes that include it and carries no module
// of it, and nothing in the closure includes it: it is required only.
const READ_HELPER: &str = r##"# frozen_string_literal: true

require "support/helpers/widgets/fixtures"

module Widgets
  module ReadHelper
    extend ActiveSupport::Concern
    include Fixtures

    included do
      setup do
        @read_helper_ready = true
      end
    end
  end
end
"##;

const DB_HELPER: &str = r##"# frozen_string_literal: true

require "support/helpers/widgets/read_helper"

module Widgets
  module DbHelper
    def self.included(base)
      base.include(Widgets::Fixtures)
    end
  end
end
"##;

const WIDGET_TEST: &str = r##"# frozen_string_literal: true

require "test_helper"

module Foo
  class WidgetTest < ActiveSupport::TestCase
    include Widgets::DbHelper

    test "the fixtures a required but uncarried file requires are loaded" do
      assert_equal :fixtures, fixture_label
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
        .write("test/support/helpers/widgets/fixtures.rb", FIXTURES)
        .write("test/support/helpers/widgets/read_helper.rb", READ_HELPER)
        .write("test/support/helpers/widgets/db_helper.rb", DB_HELPER)
        .write("components/foo/test/models/foo/widget_test.rb", WIDGET_TEST)
}

#[test]
fn a_required_uncarried_file_loads_the_files_it_requires() {
    app().run_test("test/models/foo_widget_test.rb").assert_passes();
}

/// The db helper's program loads the fixtures file through the read
/// helper's own `require`, though ingest carries nothing of the read helper.
#[test]
fn the_program_of_a_requiring_file_loads_what_an_uncarried_file_requires() {
    let (emitted, _) = app().emit(roundhouse::project::BuildTarget::Ruby);
    let program = std::fs::read_to_string(emitted.join("app/models/roundhouse_files/test/support/helpers/widgets/db_helper.rb"))
        .expect("the db helper's program");
    let fixtures = program
        .find("require RoundhouseAutoload.path(\"models/widgets/fixtures\")")
        .unwrap_or_else(|| panic!("the fixtures file the read helper requires is loaded:\n{program}"));
    let helper = program.find("require RoundhouseAutoload.path(\"models/widgets/db_helper\")").expect("the helper itself");
    assert!(fixtures < helper, "before the helper that reads them:\n{program}");
}
