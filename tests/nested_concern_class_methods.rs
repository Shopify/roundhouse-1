//! A concern that includes another concern by a rooted name
//! (`include ::MutationGuard` inside `Returns::Internal::MutationGuard`)
//! hands the includer the inner concern's `ClassMethods` and `included`
//! block, as ActiveSupport::Concern defers a concern's includes to its
//! includer. Core's `Refund` includes `Returns::Internal::MutationGuard`;
//! `save` read `readonly?` and failed with "undefined method 'readonly?' for
//! class Refund" because the class-side `readonly?` never reached it.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const SCHEMA: &str = r##"ActiveRecord::Schema.define(version: 1) do
  create_table "refunds", force: :cascade do |t|
    t.integer "order_id"
  end
end
"##;

#[test]
fn rooted_include_of_a_same_named_concern_reaches_the_model() {
    emit_and_run::empty_app()
        .write("db/schema.rb", SCHEMA)
        .write("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n")
        .write(
            "app/models/mutation_guard.rb",
            "module MutationGuard\n  extend ActiveSupport::Concern\n\n  class Gate\n    attr_accessor :disabled\n  end\n\n  module ClassMethods\n    def readonly?\n      !mutation_guard_gate.disabled\n    end\n  end\n\n  included do\n    class_attribute :mutation_guard_gate\n  end\n\nend\n",
        )
        .write(
            "app/models/returns/internal/mutation_guard.rb",
            "module Returns\n  module Internal\n    module MutationGuard\n      extend ActiveSupport::Concern\n      include ::MutationGuard\n\n      class << self\n        def gate\n          @gate ||= ::MutationGuard::Gate.new.tap { |gate| gate.disabled = true }\n        end\n      end\n\n      included do\n        self.mutation_guard_gate = Internal::MutationGuard.gate\n      end\n    end\n  end\nend\n",
        )
        .write(
            "app/models/refund.rb",
            "class Refund < ApplicationRecord\n  include Returns::Internal::MutationGuard\nend\n",
        )
        .run_ruby(
            r##"
raise "class readonly? missing" unless Refund.respond_to?(:readonly?)
raise "gate not set" if Refund.mutation_guard_gate.nil?
refund = Refund.new(order_id: 1)
raise "class guard closed" if Refund.readonly?
refund.save
puts "passed"
"##,
        )
        .assert_passes();
}
