# frozen_string_literal: true

# Module#remove_possible_method and #remove_possible_singleton_method
# (Shopify/rails 5813559f4aed, 8.2.0.alpha
# activesupport/lib/active_support/core_ext/module/remove_method.rb),
# verbatim; Rails' doc comments dropped. sprockets-rails' railtie calls it
# in its `Rails::Application` reopen (`remove_possible_method :assets`)
# before redefining the method, and native Rails loads it with
# active_support/all (`project::BOOT_LOADED_CORE_EXTENSIONS`).
#
# The PostgreSQL CRuby tree only (`project::apply_postgres_bootstrap`).

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
