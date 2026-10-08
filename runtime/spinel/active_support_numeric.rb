# frozen_string_literal: true

# active_support/core_ext/numeric.rb (Shopify/rails 5813559f4aed, 8.2.0.alpha):
# the three Numeric extensions, each the tree's port of Rails' file.
# `require "active_support/core_ext/numeric"` from a native gem (polaris,
# database_documentation) loads it (runtime/features,
# `project::FRAMEWORK_FEATURE_PORTS`).
#
# The PostgreSQL CRuby tree whose lockfile has i18n only
# (`project::apply_postgres_bootstrap`).

require_relative "active_support_numeric_bytes"
require_relative "active_support_numeric_time"
require_relative "active_support_numeric_conversions"
