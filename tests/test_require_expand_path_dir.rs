//! A test file's `require File.expand_path("../../support/helpers/x",
//! __dir__)` loads the helper the way a `require_relative` does. Core's
//! server_intents metaobject tests `include CustomDataIntentHelpers` that
//! way; the slicer (v3.41) follows the form, and the emit resolves it to
//! the carried helper (without the helper carried, every test of the
//! files raised NameError for the constant).
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const APP_CONTROLLER: &str = "class ApplicationController < ActionController::Base\nend\n";
const ROUTES: &str = "Rails.application.routes.draw do\nend\n";

const HELPER: &str = r##"# frozen_string_literal: true

module WidgetIntentHelpers
  def widget_label
    :helped
  end
end
"##;

const WIDGET_TEST: &str = r##"# frozen_string_literal: true

require "test_helper"
require File.expand_path("../../support/helpers/widget_intent_helpers", __dir__)

module Foo
  class WidgetTest < ActiveSupport::TestCase
    include WidgetIntentHelpers

    test "the helper the expanded require names is loaded" do
      assert_equal :helped, widget_label
    end
  end
end
"##;

#[test]
fn a_require_of_an_expanded_path_from_dir_loads_the_carried_helper() {
    emit_and_run::empty_app()
        .write("app/controllers/application_controller.rb", APP_CONTROLLER)
        .write("config/routes.rb", ROUTES)
        .write("config/environments/test.rb", "Rails.application.configure do\n  config.eager_load = false\nend\n")
        .write("roundhouse.yml", "test_paths:\n  - test/support\n  - components/foo/test/support/helpers\n  - components/foo/test/models/foo\n")
        .write("components/foo/test/support/helpers/widget_intent_helpers.rb", HELPER)
        .write("components/foo/test/models/foo/widget_test.rb", WIDGET_TEST)
        .run_test("test/models/foo_widget_test.rb")
        .assert_passes();
}
