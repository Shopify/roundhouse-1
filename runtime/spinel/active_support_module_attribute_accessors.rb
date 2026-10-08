# frozen_string_literal: true

# Module#mattr_reader, #mattr_writer, #mattr_accessor and their cattr_
# aliases (Shopify/rails 5813559f4aed, 8.2.0.alpha
# activesupport/lib/active_support/core_ext/module/attribute_accessors.rb),
# verbatim; Rails' doc comments dropped.
# `require "active_support/core_ext/module/attribute_accessors"` (and
# core_ext/class/attribute_accessors) from a native gem
# (state_machines-activemodel) loads it (runtime/features,
# `project::FRAMEWORK_FEATURE_PORTS`). The app's own mattr_accessor calls
# are lowered at ingest.
#
# The PostgreSQL CRuby tree only (`project::apply_postgres_bootstrap`).

class Module
  def mattr_reader(*syms, instance_reader: true, instance_accessor: true, default: nil, location: nil)
    raise TypeError, "module attributes should be defined directly on class, not singleton" if singleton_class?
    location ||= caller_locations(1, 1).first

    definition = []
    syms.each do |sym|
      raise NameError.new("invalid attribute name: #{sym}") unless /\A[_A-Za-z]\w*\z/.match?(sym)

      definition << "def self.#{sym}; @@#{sym}; end"

      if instance_reader && instance_accessor
        definition << "def #{sym}; @@#{sym}; end"
      end

      sym_default_value = (block_given? && default.nil?) ? yield : default
      class_variable_set("@@#{sym}", sym_default_value) unless sym_default_value.nil? && class_variable_defined?("@@#{sym}")
    end

    module_eval(definition.join(";"), location.path, location.lineno)
  end
  alias :cattr_reader :mattr_reader

  def mattr_writer(*syms, instance_writer: true, instance_accessor: true, default: nil, location: nil)
    raise TypeError, "module attributes should be defined directly on class, not singleton" if singleton_class?
    location ||= caller_locations(1, 1).first

    definition = []
    syms.each do |sym|
      raise NameError.new("invalid attribute name: #{sym}") unless /\A[_A-Za-z]\w*\z/.match?(sym)
      definition << "def self.#{sym}=(val); @@#{sym} = val; end"

      if instance_writer && instance_accessor
        definition << "def #{sym}=(val); @@#{sym} = val; end"
      end

      sym_default_value = (block_given? && default.nil?) ? yield : default
      class_variable_set("@@#{sym}", sym_default_value) unless sym_default_value.nil? && class_variable_defined?("@@#{sym}")
    end

    module_eval(definition.join(";"), location.path, location.lineno)
  end
  alias :cattr_writer :mattr_writer

  def mattr_accessor(*syms, instance_reader: true, instance_writer: true, instance_accessor: true, default: nil, &blk)
    location = caller_locations(1, 1).first
    mattr_reader(*syms, instance_reader: instance_reader, instance_accessor: instance_accessor, default: default, location: location, &blk)
    mattr_writer(*syms, instance_writer: instance_writer, instance_accessor: instance_accessor, default: default, location: location)
  end
  alias :cattr_accessor :mattr_accessor
end
