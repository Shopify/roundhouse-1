# frozen_string_literal: true

# ActiveSupport::Tryable and `Object#try` / `#try!` (Shopify/rails
# 5813559f4aed, 8.2.0.alpha activesupport/lib/active_support/core_ext/
# object/try.rb), verbatim. `require "active_support/core_ext"` (which
# `require "rails/all"` reaches through active_support/rails) mixes it
# into Object and Delegator and answers nil on NilClass before any app
# code; gems' and Core's modules also `include ActiveSupport::Tryable`
# themselves (api_versioning's VisibilityOwner, Core's Shippable) for
# the receivers Sorbet sees as BasicObject descendants.
#
# The PostgreSQL CRuby tree only (`project::apply_postgres_bootstrap`).
require "delegate"

module ActiveSupport
  module Tryable # :nodoc:
    def try(*args, **kwargs, &block)
      if args.empty? && kwargs.empty? && block_given?
        if block.arity == 0
          instance_eval(&block)
        else
          yield self
        end
      elsif respond_to?(args.first)
        public_send(*args, **kwargs, &block)
      end
    end

    def try!(*args, **kwargs, &block)
      if args.empty? && kwargs.empty? && block_given?
        if block.arity == 0
          instance_eval(&block)
        else
          yield self
        end
      else
        public_send(*args, **kwargs, &block)
      end
    end
  end
end

class Object
  include ActiveSupport::Tryable
end

class Delegator
  include ActiveSupport::Tryable
end

class NilClass
  # Calling +try+ on +nil+ always returns +nil+.
  # It becomes especially helpful when navigating through associations that may return +nil+.
  def try(*, &)
    nil
  end

  def try!(*, &)
    nil
  end
end
