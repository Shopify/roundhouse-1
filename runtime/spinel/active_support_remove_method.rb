# frozen_string_literal: true

# Module#remove_possible_method and #remove_possible_singleton_method
# (Shopify/rails 5813559f4aed, 8.2.0.alpha
# activesupport/lib/active_support/core_ext/module/remove_method.rb),
# verbatim; Rails' doc comments dropped. Native boot loads it with
# active_support/all (the :load_active_support initializer), so a class
# body calls it with no require of its own (sprockets-rails 3.5.2's
# `class Rails::Application`: `remove_possible_method :assets`); a native
# gem's `require "active_support/core_ext/module/remove_method"` loads it
# (runtime/features, `project::FRAMEWORK_FEATURE_PORTS`).
#
# The PostgreSQL CRuby tree only (`project::apply_postgres_bootstrap`).

require_relative "active_support_redefine_method"

class Module
  def remove_possible_method(method)
    if method_defined?(method) || private_method_defined?(method)
      undef_method(method)
    end
  end

  def remove_possible_singleton_method(method)
    singleton_class.remove_possible_method(method)
  end
end
