//! A Concern's `included do` block runs where the `include` stands, but its
//! constants resolve where the block was written: the Concern and what
//! encloses it. Core's PaymentsApi::FixtureFactoryHelper and
//! ShopifyPayments::FixtureFactoryHelper (which includes the first) each
//! define a `Factories` module and run `Factories.define(self)` from their
//! block. Carried onto a test class the bare `Factories` reached the later
//! module's, twice, so the first one's factories (`:payment_attempt`) were
//! never defined (FixtureFactory::NotFoundError).
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const ALPHA: &str = r##"module Alpha
  module Helper
    extend ActiveSupport::Concern

    module Factories
      def self.define(klass)
        (klass.instance_variable_get(:@defined) || klass.instance_variable_set(:@defined, [])) << :alpha
      end
    end

    included do
      Factories.define(self)
    end
  end
end
"##;

const BETA: &str = r##"module Beta
  module Helper
    extend ActiveSupport::Concern
    include Alpha::Helper

    module Factories
      def self.define(klass)
        (klass.instance_variable_get(:@defined) || klass.instance_variable_set(:@defined, [])) << :beta
      end
    end

    included do
      Factories.define(self)
    end
  end
end
"##;

const TEST: &str = r##"require "test_helper"

class FactoriesTest < ActiveSupport::TestCase
  include Beta::Helper

  test "each concern's block runs its own Factories" do
    assert_equal [:alpha, :beta], self.class.instance_variable_get(:@defined)
  end
end
"##;

#[test]
fn included_block_constants_resolve_in_the_concerns_own_scope() {
    emit_and_run::empty_app()
        .write("app/controllers/application_controller.rb", "class ApplicationController < ActionController::Base\nend\n")
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write("db/schema.rb", "ActiveRecord::Schema.define do\nend\n")
        .write("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n")
        .write("roundhouse.yml", "test_paths:\n  - test/support\n")
        .write("test/support/alpha_helper.rb", ALPHA)
        .write("test/support/beta_helper.rb", BETA)
        .write("test/models/factories_test.rb", TEST)
        .run_test("test/models/factories_test.rb")
        .assert_passes();
}
