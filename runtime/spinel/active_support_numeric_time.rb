# frozen_string_literal: true

# Numeric#seconds, #minutes, #hours, #days, #weeks, #fortnights and
# #in_milliseconds (Shopify/rails 5813559f4aed, 8.2.0.alpha
# activesupport/lib/active_support/core_ext/numeric/time.rb), verbatim;
# Rails' doc comments dropped, and its requires (active_support/duration and
# the Time/Date calculations) are the tree's ActiveSupport::Duration
# (ruby_overlay active_support_duration.rb, with its Time arithmetic).
# `require "active_support/core_ext/numeric/time"` (or core_ext/numeric)
# from a native gem loads it (runtime/features,
# `project::FRAMEWORK_FEATURE_PORTS`). App code's `<int>.days` is emitted as
# `ActiveSupport::Duration.days(<int>)` and never needs it.
#
# The PostgreSQL CRuby tree only (`project::apply_postgres_bootstrap`).

require_relative "active_support_duration"

class Numeric
  def seconds
    ActiveSupport::Duration.seconds(self)
  end
  alias :second :seconds

  def minutes
    ActiveSupport::Duration.minutes(self)
  end
  alias :minute :minutes

  def hours
    ActiveSupport::Duration.hours(self)
  end
  alias :hour :hours

  def days
    ActiveSupport::Duration.days(self)
  end
  alias :day :days

  def weeks
    ActiveSupport::Duration.weeks(self)
  end
  alias :week :weeks

  def fortnights
    ActiveSupport::Duration.weeks(self * 2)
  end
  alias :fortnight :fortnights

  def in_milliseconds
    self * 1000
  end
end
