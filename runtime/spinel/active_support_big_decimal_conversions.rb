# frozen_string_literal: true

# BigDecimal#to_s defaulting to the "F" format (Shopify/rails 5813559f4aed,
# 8.2.0.alpha activesupport/lib/active_support/core_ext/big_decimal/conversions.rb),
# verbatim. Required by the number helper port;
# `require "active_support/core_ext/big_decimal/conversions"` from a native gem
# loads it (runtime/features, `project::FRAMEWORK_FEATURE_PORTS`).
#
# The PostgreSQL CRuby tree only (`project::apply_postgres_bootstrap`).

require "bigdecimal"
require "bigdecimal/util"

module ActiveSupport
  module BigDecimalWithDefaultFormat # :nodoc:
    def to_s(format = "F")
      super(format)
    end
  end
end

BigDecimal.prepend(ActiveSupport::BigDecimalWithDefaultFormat)
