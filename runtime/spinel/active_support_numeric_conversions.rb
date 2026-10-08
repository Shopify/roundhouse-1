# frozen_string_literal: true

# Numeric#to_fs / #to_formatted_s over ActiveSupport::NumberHelper
# (Shopify/rails 5813559f4aed, 8.2.0.alpha
# activesupport/lib/active_support/core_ext/numeric/conversions.rb), verbatim;
# Rails' doc comments dropped, and its requires are the tree's ports.
# `require "active_support/core_ext/numeric/conversions"` (or
# core_ext/numeric) from a native gem loads it (runtime/features,
# `project::FRAMEWORK_FEATURE_PORTS`).
#
# The PostgreSQL CRuby tree whose lockfile has i18n only
# (`project::apply_postgres_bootstrap`).

require_relative "active_support_big_decimal_conversions"
require_relative "active_support_number_helper"

module ActiveSupport
  module NumericWithFormat
    def to_fs(format = nil, options = nil)
      return to_s if format.nil?

      case format
      when Integer, String
        to_s(format)
      when :phone
        ActiveSupport::NumberHelper.number_to_phone(self, options || {})
      when :currency
        ActiveSupport::NumberHelper.number_to_currency(self, options || {})
      when :percentage
        ActiveSupport::NumberHelper.number_to_percentage(self, options || {})
      when :delimited
        ActiveSupport::NumberHelper.number_to_delimited(self, options || {})
      when :rounded
        ActiveSupport::NumberHelper.number_to_rounded(self, options || {})
      when :human
        ActiveSupport::NumberHelper.number_to_human(self, options || {})
      when :human_size
        ActiveSupport::NumberHelper.number_to_human_size(self, options || {})
      when Symbol
        to_s
      else
        to_s(format)
      end
    end
    alias_method :to_formatted_s, :to_fs
  end
end

Integer.include ActiveSupport::NumericWithFormat
Float.include ActiveSupport::NumericWithFormat
BigDecimal.include ActiveSupport::NumericWithFormat
