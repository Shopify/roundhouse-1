//! A `when` operand is a constant the source resolves lexically, like
//! any other. Core's `PaymentsApi::FixtureFactoryHelper` (a test support
//! module) reads `when PaymentAttemptCharge::FAILED` inside `module
//! PaymentsApi`: `PaymentsApi::PaymentAttemptCharge`. The analysis never
//! visited a `when` operand, so it stayed the bare spelling, and the emit's
//! shadow check rooted it as `::PaymentAttemptCharge` (`uninitialized
//! constant PaymentAttemptCharge`), while the same constant read in the
//! `else` branch was resolved.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const HELPER: &str = r##"module PaymentsApi
  module FixtureFactoryHelper
    extend ActiveSupport::Concern

    def charge_kind(status)
      case status.to_sym
      when PaymentAttemptCharge::FAILED, PaymentAttemptCharge::ERRORED
        :bad
      else
        PaymentAttemptCharge::FAILED
      end
    end
  end
end
"##;

const MODEL: &str = "module PaymentsApi\n  class PaymentAttemptCharge < ApplicationRecord\n    FAILED = :failed\n    ERRORED = :errored\n  end\nend\n";

const TEST: &str = r##"class KindTest < ActiveSupport::TestCase
  include PaymentsApi::FixtureFactoryHelper

  test "a when operand resolves inside the helper's namespace" do
    assert_equal :bad, charge_kind(:failed)
    assert_equal :bad, charge_kind(:errored)
    assert_equal :failed, charge_kind(:other)
  end
end
"##;

#[test]
fn a_when_operand_resolves_in_the_enclosing_namespace() {
    let run = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n")
        .write("app/models/payments_api/payment_attempt_charge.rb", MODEL)
        .write("app/controllers/application_controller.rb", "class ApplicationController < ActionController::Base\nend\n")
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write("test/support/helpers/payments_api/fixture_factory_helper.rb", HELPER)
        .write("test/models/kind_test.rb", TEST)
        .run_test("test/models/kind_test.rb");
    assert!(run.success, "{}\n{}\n{}", run.errors.join("\n"), run.stdout, run.stderr);
    assert!(run.stdout.contains("KindTest: 1 tests passed"), "{}\n{}", run.stdout, run.stderr);
}
