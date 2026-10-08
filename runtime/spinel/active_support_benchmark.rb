# frozen_string_literal: true

# ActiveSupport::Benchmark, verbatim from the Rails Core runs, 8.2.0.alpha
# (Shopify/rails 5813559f4aed, activesupport/lib/active_support/benchmark.rb),
# for the CRuby and JRuby trees, carried where the app names the module
# (Core's Trilogy connection timing). Each method follows that source;
# tests/active_support_utilities_runtime.rb holds it to native ActiveSupport.

module ActiveSupport
  module Benchmark # :nodoc:
    # Benchmark realtime in the specified time unit. By default,
    # the returned unit is in seconds.
    #
    # `unit` can be any of the values accepted by Ruby's `Process.clock_gettime`.
    def self.realtime(unit = :float_second, &block)
      time_start = Process.clock_gettime(Process::CLOCK_MONOTONIC, unit)
      yield
      Process.clock_gettime(Process::CLOCK_MONOTONIC, unit) - time_start
    end
  end
end
