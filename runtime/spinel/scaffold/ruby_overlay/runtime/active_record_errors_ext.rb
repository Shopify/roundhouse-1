# frozen_string_literal: true
# ActiveModel::Errors#include?(attribute) on the validation accumulator.
#
# Lives on the CRuby overlay, not shared runtime/ruby/active_record/
# connection.rb: it asks the compiled ErrorAttributes port, which the
# typed shared runtime has no signature for.
module ActiveRecord
  module RuntimeValidations
    module Errors
      # `assert_includes errors, :publish_date` and `errors.include?(:name)`
      # ask for an error on the attribute. A Symbol can never be one of the
      # accumulator's full message Strings, so it is the attribute
      # question; any other argument stays Array's own (a message the
      # compiled rules test).
      def include?(value)
        return ::ActiveModel::ErrorAttributes.key?(self, value) if value.is_a?(Symbol)
        super
      end
    end
  end
end
