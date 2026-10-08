# frozen_string_literal: true

# ActiveSupport::StringInquirer, verbatim from activesupport 8.2.0.alpha
# (Shopify/rails 5813559f, the version Core pins:
# lib/active_support/string_inquirer.rb) for the PostgreSQL CRuby tree
# (`project::apply_postgres_bootstrap`). Core's SerializationOptions
# passes `format: ActiveSupport::StringInquirer.new("json")` into every
# record's serialization options.
module ActiveSupport
  # Wrapping a string in this class gives you a prettier way to test
  # for equality:
  #
  #   vehicle = ActiveSupport::StringInquirer.new('car')
  #   vehicle.car?   # => true
  #   vehicle.bike?  # => false
  class StringInquirer < String
    private
      def respond_to_missing?(method_name, include_private = false)
        method_name.end_with?("?") || super
      end

      def method_missing(method_name, ...)
        if method_name.end_with?("?")
          self == method_name[0..-2]
        else
          super
        end
      end
  end
end
