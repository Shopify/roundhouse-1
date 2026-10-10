//! A test-support Concern a support initializer includes into
//! `ActiveSupport::TestCase` registers its `included do` callbacks on
//! every test.
//!
//! Core's ActiveRecordTestHelper (components/platform/essentials/test/
//! support/helpers/active_record_test_helper.rb) declares `included do
//! setup(:default_pod_selection); teardown(:reset_pod_selection); prepend
//! SharedPoolPatch end`, and test/support/initializers/
//! active_support_test_case.rb includes it into ActiveSupport::TestCase;
//! `default_pod_selection` sets `Podding::Current.pod_id = 0`. The block
//! declares setups, so the test-mixin splice took the module as a shared
//! test module and blanked its `extend ActiveSupport::Concern` and
//! `included do` from the library body: no test class includes it, the
//! reopen's include ran nothing, and every test that selects a pod raised
//! `ArgumentError: Unspecified pod_id` (13 unit-1000 ids). The library
//! body now stays the Concern, its block carried onto the includer, and
//! the `setup` it calls while app/models eager-loads is held until the
//! test runner's callbacks load.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const HELPER: &str = r#"module PodHelper
  extend ActiveSupport::Concern

  module Patch
    def patched?
      true
    end
  end

  included do
    setup(:default_pod)
    teardown(:reset_pod)
    prepend Patch
  end

  def default_pod
    $pod = 0
  end

  def reset_pod
    $pod = nil
  end
end
"#;

const INITIALIZER: &str = r#"require_relative "../helpers/pod_helper"

module ActiveSupport
  class TestCase
    include PodHelper
  end
end
"#;

const TEST: &str = r#"require "test_helper"

class PodTest < ActiveSupport::TestCase
  test "the concern's included block registered its setup" do
    assert_equal 0, $pod
    assert patched?
  end
end
"#;

#[test]
fn a_support_concern_included_into_test_case_runs_its_included_block() {
    let run = emit_and_run::empty_app()
        .write("app/controllers/application_controller.rb", "class ApplicationController < ActionController::Base\nend\n")
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write(
            "roundhouse.yml",
            "test_paths:\n  - test/support/helpers\n  - test/support/initializers\n  - test/models\n",
        )
        .write("test/support/helpers/pod_helper.rb", HELPER)
        .write("test/support/initializers/active_support_test_case.rb", INITIALIZER)
        .write("test/test_helper.rb", "require_relative \"support/initializers/active_support_test_case\"\n")
        .write("test/models/pod_test.rb", TEST)
        .run_test("test/models/pod_test.rb");
    run.assert_passes();
    assert!(run.stdout.contains("1 tests passed") || run.stdout.contains("1 runs"), "{}", run.stdout);
}
