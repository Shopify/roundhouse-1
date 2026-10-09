# frozen_string_literal: true

# ActiveModel::Name, ActiveModel::Naming and ActiveModel::Conversion,
# ported from the activemodel native Core runs (Shopify/rails
# 5813559f4aed, 8.2.0.alpha: lib/active_model/naming.rb and
# conversion.rb) for the PostgreSQL CRuby tree, carried where app code
# (a gem compiled as source among it) names one of them. frozen_record
# 0.27.4's FrozenRecord::Base extends Naming and includes Conversion, and
# api_versioning's records, Core's FrozenRecord models, name themselves
# through them. Each method follows the native one, over the Inflector
# port (runtime/active_support_inflector.rb).
#
# Differences, at the edges:
# - ActiveSupport::Concern is not used: Conversion's `included` block
#   (`class_attribute :param_delimiter, instance_reader: false,
#   default: "-"`) is written out as Concern#append_features, and the
#   class_attribute as the class reader, writer and predicate and the
#   instance writer it defines: a class-level value a subclass inherits
#   until it assigns its own.
# - `Module#module_parents` and `silence_redefinition_of_method`
#   (core_ext/module/introspection.rb, redefine_method.rb) are not
#   reopened onto Module: Naming computes the parents it walks the way
#   module_parents does, and Naming.extended silences the redefinition
#   the way silence_redefinition_of_method does.
# - `Name#as_json` (delegated to the name, String#as_json) answers
#   through the ActiveSupport::JSON port's `Encoding.as_json`
#   (runtime/spinel/active_support_json.rb): this tree does not reopen
#   String with `as_json`.
# - `ActiveSupport::Ractors.make_shareable` and `on_main` are
#   `Ractor.make_shareable` and the block itself, which is what they are
#   on the main Ractor of a Ruby that has `Ractor.make_shareable`.

require_relative "active_support_json"

# ---- activemodel/lib/active_model/naming.rb
module ActiveModel
  class Name
    include Comparable

    attr_accessor :singular, :plural, :element, :collection,
      :singular_route_key, :route_key, :param_key, :i18n_key,
      :name

    alias_method :cache_key, :collection

    delegate :==, :===, :<=>, :=~, :"!~", :eql?, :match?, :to_s,
             :to_str, to: :name

    # `delegate :as_json, to: :name`: the name's String#as_json
    # (core_ext/object/json.rb), which this tree answers through the
    # ActiveSupport::JSON port rather than on String.
    def as_json(options = nil)
      ActiveSupport::JSON::Encoding.as_json(name, options)
    end

    def initialize(klass, namespace = nil, name = nil, locale = :en)
      @name = name || klass.name

      raise ArgumentError, "Class name cannot be blank. You need to supply a name argument when anonymous class given" if @name.blank?

      @unnamespaced = @name.delete_prefix("#{namespace.name}::") if namespace
      @klass        = klass
      @singular     = _singularize(@name)
      @plural       = ActiveSupport::Inflector.pluralize(@singular, locale)
      @uncountable  = @plural == @singular
      @element      = ActiveSupport::Inflector.underscore(ActiveSupport::Inflector.demodulize(@name))
      @human        = ActiveSupport::Inflector.humanize(@element)
      @collection   = ActiveSupport::Inflector.tableize(@name)
      @param_key    = (namespace ? _singularize(@unnamespaced) : @singular)
      @i18n_key     = ActiveSupport::Inflector.underscore(@name).to_sym

      @route_key          = (namespace ? ActiveSupport::Inflector.pluralize(@param_key, locale) : @plural.dup)
      @singular_route_key = ActiveSupport::Inflector.singularize(@route_key, locale)
      @route_key << "_index" if @uncountable
    end

    # Transform the model name into a more human format, using I18n. By
    # default, it will underscore then humanize the class name.
    def human(options = {})
      return @human if i18n_keys.empty? || i18n_scope.empty?

      key, *defaults = i18n_keys
      defaults << options[:default] if options[:default]
      defaults << MISSING_TRANSLATION

      translation = I18n.translate(key, scope: i18n_scope, count: 1, **options, default: defaults)
      translation = @human if translation == MISSING_TRANSLATION
      translation
    end

    def uncountable?
      @uncountable
    end

    def freeze
      i18n_keys
      i18n_scope
      super
    end

    private
      MISSING_TRANSLATION = -(2**60) # :nodoc:

      def _singularize(string)
        ActiveSupport::Inflector.underscore(string).tr("/", "_")
      end

      def i18n_keys
        @i18n_keys ||= if @klass.respond_to?(:lookup_ancestors)
          @klass.lookup_ancestors.filter_map do |klass|
            klass.model_name.i18n_key unless klass.name.blank?
          end
        else
          []
        end
      end

      def i18n_scope
        @i18n_scope ||= @klass.respond_to?(:i18n_scope) ? [@klass.i18n_scope, :models] : []
      end
  end

  module Naming
    def self.extended(base) # :nodoc:
      # silence_redefinition_of_method :model_name
      if base.method_defined?(:model_name) || base.private_method_defined?(:model_name)
        base.alias_method :__rails_redefine, :model_name
        base.remove_method :__rails_redefine
      end
      base.delegate :model_name, to: :class
    end

    # Returns an ActiveModel::Name object for module. It can be
    # used to retrieve all kinds of naming-related information
    # (See ActiveModel::Name for more information).
    def model_name
      return @_model_name if @_model_name

      namespace = Naming.module_parents(self).detect do |n|
        n.respond_to?(:use_relative_model_naming?) && n.use_relative_model_naming?
      end
      @_model_name = ActiveModel::Name.new(self, namespace)
      Ractor.make_shareable(@_model_name)
    end

    def self.plural(record_or_class)
      model_name_from_record_or_class(record_or_class).plural
    end

    def self.singular(record_or_class)
      model_name_from_record_or_class(record_or_class).singular
    end

    def self.uncountable?(record_or_class)
      model_name_from_record_or_class(record_or_class).uncountable?
    end

    def self.singular_route_key(record_or_class)
      model_name_from_record_or_class(record_or_class).singular_route_key
    end

    def self.route_key(record_or_class)
      model_name_from_record_or_class(record_or_class).route_key
    end

    def self.param_key(record_or_class)
      model_name_from_record_or_class(record_or_class).param_key
    end

    def self.model_name_from_record_or_class(record_or_class) # :nodoc:
      if record_or_class.respond_to?(:to_model)
        record_or_class.to_model.model_name
      else
        record_or_class.model_name
      end
    end
    private_class_method :model_name_from_record_or_class

    # Module#module_parents (core_ext/module/introspection.rb): the
    # enclosing modules by name, innermost first, then Object.
    def self.module_parents(mod) # :nodoc:
      parents = []
      name = mod.name
      parent_name = name =~ /::[^:]+\z/ ? -$` : nil
      if parent_name
        parts = parent_name.split("::")
        until parts.empty?
          parents << ActiveSupport::Inflector.constantize(parts * "::")
          parts.pop
        end
      end
      parents << Object unless parents.include? Object
      parents
    end

    private
      def inherited(base)
        super
        base.class_eval do
          @_model_name = nil
        end
      end
  end
end

# ---- activemodel/lib/active_model/conversion.rb
module ActiveModel
  module Conversion
    # `included do class_attribute :param_delimiter, instance_reader:
    # false, default: "-" end`, run as Concern#append_features runs it:
    # once, and not again for a class that already has the module.
    def self.append_features(base)
      # A module includer is a concern natively (an app concern is lowered
      # without its extend): Concern#append_features defers to its includers.
      unless base.is_a?(Class)
        base.extend(ActiveSupport::Concern) if defined?(ActiveSupport::Concern) && !base.instance_variable_defined?(:@_dependencies)
        return super unless base.instance_variable_defined?(:@_dependencies)
        base.instance_variable_get(:@_dependencies) << self
        return false
      end
      return false if base < self
      super
      base.extend ClassMethods
      base.instance_variable_set(:@__class_attr_param_delimiter, "-")
    end

    # If your object is already designed to implement all of the Active
    # Model you can use the default <tt>:to_model</tt> implementation,
    # which simply returns +self+.
    def to_model
      self
    end

    # Returns an Array of all key attributes if any of the attributes is
    # set, whether or not the object is persisted. Returns +nil+ if there
    # are no key attributes.
    def to_key
      key = respond_to?(:id) && id
      key ? Array(key) : nil
    end

    # Returns a +string+ representing the object's key suitable for use
    # in URLs, or +nil+ if <tt>persisted?</tt> is +false+.
    def to_param
      (persisted? && (key = to_key) && key.all?) ? key.join(self.class.param_delimiter) : nil
    end

    # Returns a +string+ identifying the path associated with the object.
    def to_partial_path
      self.class._to_partial_path
    end

    # The class_attribute instance writer (`instance_reader: false`).
    def param_delimiter=(value)
      @param_delimiter = value
    end

    module ClassMethods # :nodoc:
      # The class_attribute reader: the value the nearest class that
      # assigned one holds.
      def param_delimiter
        owner = self
        owner = owner.superclass until owner.instance_variable_defined?(:@__class_attr_param_delimiter)
        owner.instance_variable_get(:@__class_attr_param_delimiter)
      end

      # The class_attribute writer.
      def param_delimiter=(value)
        @__class_attr_param_delimiter = value
      end

      def param_delimiter?
        !!param_delimiter
      end

      # Provide a class level cache for #to_partial_path. This is an
      # internal method and should not be accessed directly.
      def _to_partial_path # :nodoc:
        @_to_partial_path ||= if respond_to?(:model_name)
          "#{model_name.collection}/#{model_name.element}".freeze
        else
          element = ActiveSupport::Inflector.underscore(ActiveSupport::Inflector.demodulize(name))
          collection = ActiveSupport::Inflector.tableize(name)
          "#{collection}/#{element}".freeze
        end
      end
    end
  end
end
