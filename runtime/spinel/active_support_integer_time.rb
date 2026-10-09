# frozen_string_literal: true

# Integer#months and #years (Shopify/rails 5813559f4aed, 8.2.0.alpha
# activesupport/lib/active_support/core_ext/integer/time.rb), verbatim;
# Rails' doc comments dropped, and its requires (active_support/duration
# and core_ext/numeric/time) are the tree's ActiveSupport::Duration and
# Numeric port. Rails' :load_active_support initializer loads it with
# active_support/all, so a class body's `1.year` needs no require
# (`project::BOOT_LOADED_CORE_EXTENSIONS`); a native gem's
# `require "active_support/core_ext/integer/time"` loads it from
# runtime/features (`project::FRAMEWORK_FEATURE_PORTS`).
#
# The PostgreSQL CRuby tree only (`project::apply_postgres_bootstrap`).

require_relative "active_support_numeric_time"

class Integer
  def months
    ActiveSupport::Duration.months(self)
  end
  alias :month :months

  def years
    ActiveSupport::Duration.years(self)
  end
  alias :year :years
end
