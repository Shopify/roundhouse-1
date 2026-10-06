# `not_nil!` — type_toolkit's nil assertion (type_toolkit 0.0.5,
# lib/type_toolkit/ext/nil_assertions.rb), Shopify Core's spelling of
# Sorbet's `T.must`. Same behaviour as the gem: every object answers
# itself, nil raises `TypeToolkit::UnexpectedNilError`, which descends
# from Exception so a bare `rescue => e` does not swallow it.
#
# A reopen of Kernel and NilClass, so CRuby-overlay-only, like
# active_support_core_ext.rb. The analyzer types the call as the
# receiver with nil removed; `project` refuses it on every tree that
# does not load this file.

module Kernel
  def not_nil!
    self
  end
end

class NilClass
  def not_nil!
    raise TypeToolkit::UnexpectedNilError
  end
end

module TypeToolkit
  class UnexpectedNilError < Exception
    def initialize(message = "Called `not_nil!` on nil.")
      super
    end
  end
end
