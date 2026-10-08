# frozen_string_literal: true

# Class#descendants, filtered for reloaded classes (Shopify/rails
# 5813559f4aed, 8.2.0.alpha
# activesupport/lib/active_support/core_ext/class/subclasses.rb),
# verbatim; Rails' doc comments dropped, and its require of the
# descendants tracker is the tree's port of it.
# `require "active_support/core_ext/class/subclasses"` (or core_ext/class)
# from a native gem loads it (runtime/features,
# `project::FRAMEWORK_FEATURE_PORTS`), as Active Record's base.rb has
# before any gem natively.
#
# The PostgreSQL CRuby tree only (`project::apply_postgres_bootstrap`).

require_relative "active_support_descendants_tracker"

class Class
  def descendants
    subclasses.concat(subclasses.flat_map(&:descendants))
  end

  prepend ActiveSupport::DescendantsTracker::ReloadedClassesFiltering
end
