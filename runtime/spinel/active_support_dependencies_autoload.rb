# frozen_string_literal: true

# ActiveSupport::Autoload (Shopify/rails 5813559f4aed, 8.2.0.alpha
# activesupport/lib/active_support/dependencies/autoload.rb), verbatim;
# Rails' doc comments dropped. `require "active_support"` loads it before
# Bundler.require, so runtime/gem_facades.rb loads it before the gems: a gem
# that `extend ActiveSupport::Autoload`s (shopify_api, activeresource) does
# so as it loads. `require "active_support/dependencies/autoload"` answers
# from it (runtime/features, `project::FRAMEWORK_FEATURE_PORTS`).
#
# The PostgreSQL CRuby tree only (`project::apply_postgres_bootstrap`).

require "active_support/inflector/methods"

module ActiveSupport
  module Autoload
    def autoload(const_name, path = @_at_path)
      unless path
        full = [name, @_under_path, const_name.to_s].compact.join("::")
        path = Inflector.underscore(full)
      end

      if @_eager_autoload
        @_eagerloaded_constants ||= []
        @_eagerloaded_constants << const_name
      end

      super const_name, path
    end

    def autoload_under(path)
      @_under_path, old_path = path, @_under_path
      yield
    ensure
      @_under_path = old_path
    end

    def autoload_at(path)
      @_at_path, old_path = path, @_at_path
      yield
    ensure
      @_at_path = old_path
    end

    def eager_autoload
      old_eager, @_eager_autoload = @_eager_autoload, true
      yield
    ensure
      @_eager_autoload = old_eager
    end

    def eager_load!
      if @_eagerloaded_constants
        @_eagerloaded_constants.each { |const_name| const_get(const_name) }
        @_eagerloaded_constants = nil
      end
    end
  end
end
