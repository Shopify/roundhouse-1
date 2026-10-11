//! A Concern's `included do` block runs on a test class that includes it.
//!
//! `ActiveSupport::Concern#append_features` class_evals the block on the
//! includer where the `include` stands. A test-support module declaring
//! its registrations there (`define_things { … }`, `use_fixtures`)
//! emitted as a plain module with no hook, and a test class is none of
//! the includers ingest supplies a block to, so the registrations never
//! ran: the test's lookup found nothing. The block's statements are now
//! carried onto each including test class, once, and a parent test
//! class's include is not run again on its subclass.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const CONCERN: &str = r#"module Things
  extend ActiveSupport::Concern

  module ClassMethods
    def registry
      @registry ||= {}
    end

    def define_things(&block)
      registry.merge!(block.call)
    end

    def runs
      @runs ||= []
    end

    def note_run
      runs << name
    end
  end

  included do
    note_run
    define_things do
      { plan: -> { "plan-#{name}" }, shop: -> { "shop" } }
    end
  end

  def thing(key)
    self.class.registry.fetch(key).call
  end
end
"#;

const TEST: &str = r#"require "test_helper"
require "support/helpers/things"

class FirstTest < ActiveSupport::TestCase
  include Things

  test "the block's registration ran on the including class" do
    assert_equal "plan-FirstTest", thing(:plan)
    assert_equal "shop", thing(:shop)
    assert_equal ["FirstTest"], self.class.runs
  end
end

class ChildTest < FirstTest
  test "a subclass inherits the registration without running the block again" do
    assert_equal "plan-FirstTest", thing(:plan)
    assert_equal [], self.class.runs
  end
end
"#;

#[test]
fn a_concerns_included_block_runs_on_an_including_test_class() {
    let run = emit_and_run::empty_app()
        .write("app/controllers/application_controller.rb", "class ApplicationController < ActionController::Base\nend\n")
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write("roundhouse.yml", "test_paths:\n  - test/support/helpers\n  - test/models\n")
        .write("test/support/helpers/things.rb", CONCERN)
        .write("test/test_helper.rb", "")
        .write("test/models/first_test.rb", TEST)
        .run_test("test/models/first_test.rb");
    run.assert_passes();
}
