# frozen_string_literal: true

# Kernel#silence_warnings, #enable_warnings, #with_warnings and #suppress
# (Shopify/rails 5813559f4aed, 8.2.0.alpha
# activesupport/lib/active_support/core_ext/kernel/reporting.rb),
# verbatim; Rails' doc comments dropped. Rails' :load_active_support
# initializer loads it (active_support/all); a native gem's `require
# "active_support/core_ext/kernel/reporting"` loads it too (runtime/features,
# `project::FRAMEWORK_FEATURE_PORTS`). activeresource 6.2.0's
# `ActiveResource::Base.prefix=` redefines its readers in `silence_warnings`.
#
# The PostgreSQL CRuby tree only (`project::apply_postgres_bootstrap`).

module Kernel
  module_function

  def silence_warnings(&block)
    with_warnings(nil, &block)
  end

  def enable_warnings(&block)
    with_warnings(true, &block)
  end

  def with_warnings(flag)
    old_verbose, $VERBOSE = $VERBOSE, flag
    yield
  ensure
    $VERBOSE = old_verbose
  end

  def suppress(*exception_classes)
    yield
  rescue *exception_classes
  end
end
