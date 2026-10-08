# frozen_string_literal: true

# ActiveModel::Validations, ported from the activemodel native Core runs
# (Shopify/rails 5813559f4aed, 8.2.0.alpha: lib/active_model/
# validations.rb, validations/*.rb, errors.rb, error.rb, nested_error.rb,
# translation.rb and callbacks.rb) for the PostgreSQL CRuby tree, carried
# where app code (a gem compiled as source among it) names the module.
# api_versioning's ApiVersionRecord and ApiChangeRecord, FrozenRecord
# classes Core loads at boot, include it and declare `validates`,
# `validate` and `with_options` rules in their bodies; Core keeps more
# such classes whose superclass is no model. Their class bodies are
# replayed over this port (`emit::ruby::library`), so `valid?`, `errors`
# (an ActiveModel::Errors of ActiveModel::Error objects) and the messages
# answer as ActiveModel does, through I18n and activemodel's English
# locale (runtime/rails_framework_locales.rb).
#
# Each method follows the native one. Differences, at the edges:
# - ActiveSupport::Concern is not used: Validations' and
#   Validations::Callbacks' `included` blocks are written out as
#   Concern#append_features runs them (once, and not again for a class
#   that already has the module), and the class_attributes
#   (`_validators`, Error's `i18n_customize_full_message`) as the readers,
#   writers and predicates class_attribute defines: a class-level value a
#   subclass inherits until it assigns its own.
# - The core_ext methods the native files call (`extract_options!`,
#   `symbolize_keys`, `slice!`, `assert_valid_keys`, `deep_dup`,
#   `String#remove`, `presence`, `in?`) are written out where they are
#   called, through ActiveModel::ValidationsSupport, so the port does not
#   depend on the tree's core_ext overlay. `blank?`/`present?` ask the
#   value when it answers them, and core_ext/object/blank.rb's answers
#   otherwise. `camelize` and `humanize` go through the Inflector port.
# - `delegate ... to: :@errors` is written out as forwarding methods.
# - ActiveSupport::InspectBackport is not applied: Ruby 4.0's
#   Kernel#inspect reads `instance_variables_to_inspect` itself, which is
#   what the backport supplies on older Rubies.
# - Validator, EachValidator and BlockValidator are their own port
#   (runtime/active_model_validator.rb), as ActiveSupport::Callbacks
#   (runtime/active_support_callbacks.rb) and Name/Naming
#   (runtime/active_model_naming.rb) are.
# - ActiveSupport::OptionMerger and Object#with_options ride with this
#   port (runtime/active_support_option_merger.rb).

require "bigdecimal/util"
require_relative "active_support_lazy_load_hooks"
require_relative "active_support_callbacks"
require_relative "active_model_naming"
require_relative "active_model_validator"
require_relative "active_support_option_merger"

module ActiveModel
  # The core_ext answers the native files rely on.
  module ValidationsSupport # :nodoc:
    BLANK_RE = /\A[[:space:]]*\z/

    module_function

    # Array#extract_options! (core_ext/array/extract_options.rb).
    def extract_options!(args)
      last = args.last
      if last.is_a?(Hash) && (last.respond_to?(:extractable_options?) ? last.extractable_options? : last.instance_of?(Hash))
        args.pop
      else
        {}
      end
    end

    # Object#blank? (core_ext/object/blank.rb).
    def blank?(value)
      return value.blank? if value.respond_to?(:blank?)
      case value
      when ::String
        value.empty? || BLANK_RE.match?(value)
      when ::Numeric, true
        false
      else
        value.respond_to?(:empty?) ? !!value.empty? : !value
      end
    end

    def present?(value)
      !blank?(value)
    end

    # Hash#symbolize_keys (core_ext/hash/keys.rb).
    def symbolize_keys(hash)
      hash.transform_keys { |key| key.to_sym rescue key }
    end

    # Hash#assert_valid_keys (core_ext/hash/keys.rb).
    def assert_valid_keys(hash, *valid_keys)
      valid_keys.flatten!
      hash.each_key do |k|
        unless valid_keys.include?(k)
          raise ArgumentError.new("Unknown key: #{k.inspect}. Valid keys are: #{valid_keys.map(&:inspect).join(', ')}")
        end
      end
    end

    # Object#deep_dup (core_ext/object/deep_dup.rb), Array's and Hash's.
    def deep_dup(value)
      case value
      when ::Array
        value.map { |v| deep_dup(v) }
      when ::Hash
        hash = value.dup
        value.each_pair do |key, v|
          if ::String === key || ::Symbol === key
            hash[key] = deep_dup(v)
          else
            hash.delete(key)
            hash[deep_dup(key)] = deep_dup(v)
          end
        end
        hash
      when ::Method, ::UnboundMethod
        value
      else
        value.dup
      end
    end
  end

  # ---- activemodel/lib/active_model/callbacks.rb
  module Callbacks
    def self.extended(base) # :nodoc:
      base.class_eval do
        include ActiveSupport::Callbacks
      end
    end

    def define_model_callbacks(*callbacks)
      options = ValidationsSupport.extract_options!(callbacks)
      options = {
        skip_after_callbacks_if_terminated: true,
        scope: [:kind, :name],
        only: [:before, :around, :after]
      }.merge!(options)

      types = Array(options.delete(:only))

      callbacks.each do |callback|
        define_callbacks(callback, options)

        types.each do |type|
          send("_define_#{type}_model_callback", self, callback)
        end
      end
    end

    private
      def _define_before_model_callback(klass, callback)
        klass.define_singleton_method("before_#{callback}") do |*args, **options, &block|
          ValidationsSupport.assert_valid_keys(options, :if, :unless, :prepend)
          set_callback(:"#{callback}", :before, *args, options, &block)
        end
      end

      def _define_around_model_callback(klass, callback)
        klass.define_singleton_method("around_#{callback}") do |*args, **options, &block|
          ValidationsSupport.assert_valid_keys(options, :if, :unless, :prepend)
          set_callback(:"#{callback}", :around, *args, options, &block)
        end
      end

      def _define_after_model_callback(klass, callback)
        klass.define_singleton_method("after_#{callback}") do |*args, **options, &block|
          ValidationsSupport.assert_valid_keys(options, :if, :unless, :prepend)
          options[:prepend] = true
          conditional = ActiveSupport::Callbacks::Conditionals::Value.new { |v|
            v != false
          }
          options[:if] = Array(options[:if]) + [conditional]
          set_callback(:"#{callback}", :after, *args, options, &block)
        end
      end
  end

  # ---- activemodel/lib/active_model/translation.rb
  module Translation
    include ActiveModel::Naming

    singleton_class.attr_accessor :raise_on_missing_translations

    # Returns the +i18n_scope+ for the class. Override if you want custom lookup.
    def i18n_scope
      :activemodel
    end

    # When localizing a string, it goes through the lookup returned by this
    # method, which is used in ActiveModel::Name#human,
    # ActiveModel::Errors#full_messages and
    # ActiveModel::Translation#human_attribute_name.
    def lookup_ancestors
      ancestors.select { |x| x.respond_to?(:model_name) }
    end

    MISSING_TRANSLATION = -(2**60) # :nodoc:

    # Transforms attribute names into a more human format, such as "First name"
    # instead of "first_name".
    def human_attribute_name(attribute, options = {})
      attribute = attribute.to_s

      if attribute.include?(".")
        namespace, _, attribute = attribute.rpartition(".")
        namespace.tr!(".", "/")

        if ValidationsSupport.present?(attribute)
          key = "#{namespace}.#{attribute}"
          separator = "/"
        else
          key = namespace
          separator = "."
        end

        defaults = lookup_ancestors.map do |klass|
          :"#{i18n_scope}.attributes.#{klass.model_name.i18n_key}#{separator}#{key}"
        end
        defaults << :"#{i18n_scope}.attributes.#{key}"
        defaults << :"attributes.#{key}"
      else
        defaults = lookup_ancestors.map do |klass|
          :"#{i18n_scope}.attributes.#{klass.model_name.i18n_key}.#{attribute}"
        end
      end

      raise_on_missing = options.fetch(:raise, Translation.raise_on_missing_translations)

      defaults << :"attributes.#{attribute}"
      defaults << options[:default] if options[:default]
      defaults << MISSING_TRANSLATION unless raise_on_missing

      translation = I18n.translate(defaults.shift, count: 1, raise: raise_on_missing, **options, default: defaults)
      if translation == MISSING_TRANSLATION
        translation = ValidationsSupport.present?(attribute) ? ActiveSupport::Inflector.humanize(attribute) : ActiveSupport::Inflector.humanize(namespace)
      end
      translation
    end
  end

  ActiveSupport.run_load_hooks(:active_model_translation, Translation)

  # ---- activemodel/lib/active_model/error.rb
  class Error
    CALLBACKS_OPTIONS = [:if, :unless, :on, :allow_nil, :allow_blank, :strict].freeze
    MESSAGE_OPTIONS = [:message].freeze

    # class_attribute :i18n_customize_full_message, default: false
    @__class_attr_i18n_customize_full_message = false

    class << self
      def i18n_customize_full_message
        owner = self
        owner = owner.superclass until owner.instance_variable_defined?(:@__class_attr_i18n_customize_full_message)
        owner.instance_variable_get(:@__class_attr_i18n_customize_full_message)
      end

      def i18n_customize_full_message=(value)
        @__class_attr_i18n_customize_full_message = value
      end

      def i18n_customize_full_message?
        !!i18n_customize_full_message
      end
    end

    def i18n_customize_full_message
      defined?(@i18n_customize_full_message) ? @i18n_customize_full_message : self.class.i18n_customize_full_message
    end

    def i18n_customize_full_message=(value)
      @i18n_customize_full_message = value
    end

    def i18n_customize_full_message?
      !!i18n_customize_full_message
    end

    def self.full_message(attribute, message, base) # :nodoc:
      return message if attribute == :base

      base_class = base.class
      attribute = attribute.to_s

      if i18n_customize_full_message && base_class.respond_to?(:i18n_scope)
        attribute = attribute.gsub(/\[\d+\]/, "")
        parts = attribute.split(".")
        attribute_name = parts.pop
        namespace = parts.join("/") unless parts.empty?
        attributes_scope = "#{base_class.i18n_scope}.errors.models"

        if namespace
          defaults = base_class.lookup_ancestors.map do |klass|
            [
              :"#{attributes_scope}.#{klass.model_name.i18n_key}/#{namespace}.attributes.#{attribute_name}.format",
              :"#{attributes_scope}.#{klass.model_name.i18n_key}/#{namespace}.format",
            ]
          end
        else
          defaults = base_class.lookup_ancestors.map do |klass|
            [
              :"#{attributes_scope}.#{klass.model_name.i18n_key}.attributes.#{attribute_name}.format",
              :"#{attributes_scope}.#{klass.model_name.i18n_key}.format",
            ]
          end
        end

        defaults.flatten!
      else
        defaults = []
      end

      defaults << :"errors.format"
      defaults << "%{attribute} %{message}"

      attr_name = ActiveSupport::Inflector.humanize(attribute.gsub(/\.base\z/, "").tr(".", "_"))
      attr_name = base_class.human_attribute_name(attribute, {
        default: attr_name,
        base: base,
      })

      I18n.t(defaults.shift,
        default:  defaults,
        attribute: attr_name,
        message:   message)
    end

    def self.generate_message(attribute, type, base, options) # :nodoc:
      type = options.delete(:message) if options[:message].is_a?(Symbol)
      value = (attribute != :base ? base.read_attribute_for_validation(attribute) : nil)

      options = {
        model: base.model_name.human,
        attribute: base.class.human_attribute_name(attribute, { base: base }),
        value: value,
        object: base
      }.merge!(options)

      if base.class.respond_to?(:i18n_scope)
        i18n_scope = base.class.i18n_scope.to_s
        attribute = attribute.to_s.gsub(/\[\d+\]/, "")

        defaults = base.class.lookup_ancestors.flat_map do |klass|
          [ :"#{i18n_scope}.errors.models.#{klass.model_name.i18n_key}.attributes.#{attribute}.#{type}",
            :"#{i18n_scope}.errors.models.#{klass.model_name.i18n_key}.#{type}" ]
        end
        defaults << :"#{i18n_scope}.errors.messages.#{type}"

        catch(:exception) do
          translation = I18n.translate(defaults.first, **options, default: defaults.drop(1), throw: true)
          return translation unless translation.nil?
        end unless options[:message]
      else
        defaults = []
      end

      defaults << :"errors.attributes.#{attribute}.#{type}"
      defaults << :"errors.messages.#{type}"

      key = defaults.shift
      defaults = options.delete(:message) if options[:message]
      options[:default] = defaults

      I18n.translate(key, **options)
    end

    def initialize(base, attribute, type = :invalid, **options)
      @base = base
      @attribute = attribute
      @raw_type = type
      @type = type || :invalid
      @options = options
    end

    def initialize_dup(other) # :nodoc:
      @attribute = @attribute.dup
      @raw_type = @raw_type.dup
      @type = @type.dup
      @options = ValidationsSupport.deep_dup(@options)
    end

    # The object which the error belongs to
    attr_reader :base
    # The attribute of +base+ which the error belongs to
    attr_reader :attribute
    # The type of error, defaults to +:invalid+ unless specified
    attr_reader :type
    # The raw value provided as the second parameter when calling
    # <tt>errors#add</tt>
    attr_reader :raw_type
    # The options provided when calling <tt>errors#add</tt>
    attr_reader :options

    # Returns the error message.
    def message
      case raw_type
      when Symbol
        self.class.generate_message(attribute, raw_type, @base, options.except(*CALLBACKS_OPTIONS))
      else
        raw_type
      end
    end

    # Returns the error details.
    def details
      { error: raw_type }.merge(options.except(*CALLBACKS_OPTIONS + MESSAGE_OPTIONS))
    end
    alias_method :detail, :details

    # Returns the full error message.
    def full_message
      self.class.full_message(attribute, message, @base)
    end

    # See if error matches provided +attribute+, +type+, and +options+.
    def match?(attribute, type = nil, **options)
      if @attribute != attribute || (type && @type != type)
        return false
      end

      options.each do |key, value|
        if @options[key] != value
          return false
        end
      end

      true
    end

    # See if error matches provided +attribute+, +type+, and +options+ exactly.
    def strict_match?(attribute, type, **options)
      return false unless match?(attribute, type)

      options.except(*CALLBACKS_OPTIONS + MESSAGE_OPTIONS) == @options.except(*CALLBACKS_OPTIONS + MESSAGE_OPTIONS)
    end

    def ==(other) # :nodoc:
      other.is_a?(self.class) && attributes_for_hash == other.attributes_for_hash
    end
    alias eql? ==

    def hash # :nodoc:
      attributes_for_hash.hash
    end

    private
      def instance_variables_to_inspect
        [:@attribute, :@type, :@options].freeze
      end

    protected
      def attributes_for_hash
        [@base, @attribute, @raw_type, @options.except(*CALLBACKS_OPTIONS)]
      end
  end

  ActiveSupport.run_load_hooks(:active_model_error, Error)

  # ---- activemodel/lib/active_model/nested_error.rb
  class NestedError < Error
    def initialize(base, inner_error, override_options = {})
      @base = base
      @inner_error = inner_error
      @attribute = override_options.fetch(:attribute) { inner_error.attribute }
      @type = override_options.fetch(:type) { inner_error.type }
      @raw_type = inner_error.raw_type
      @options = inner_error.options
    end

    attr_reader :inner_error

    # delegate :message, to: :@inner_error
    def message
      @inner_error.message
    end
  end

  # ---- activemodel/lib/active_model/errors.rb
  class Errors
    include Enumerable

    # delegate :each, :clear, :empty?, :size, :uniq!, to: :@errors
    def each(...)
      @errors.each(...)
    end

    def clear(...)
      @errors.clear(...)
    end

    def empty?(...)
      @errors.empty?(...)
    end

    def size(...)
      @errors.size(...)
    end

    def uniq!(...)
      @errors.uniq!(...)
    end

    # The actual array of +Error+ objects
    # This method is aliased to <tt>objects</tt>.
    attr_reader :errors
    alias :objects :errors

    # Pass in the instance of the object that is using the errors object.
    def initialize(base)
      @base = base
      @errors = []
    end

    def initialize_dup(other) # :nodoc:
      @errors = ValidationsSupport.deep_dup(other.errors)
      super
    end

    # Copies the errors from <tt>other</tt>.
    # For copying errors but keep <tt>@base</tt> as is.
    def copy!(other) # :nodoc:
      @errors = ValidationsSupport.deep_dup(other.errors)
      @errors.each { |error|
        error.instance_variable_set(:@base, @base)
      }
    end

    # Imports one error.
    # Imported errors are wrapped as a NestedError,
    # providing access to original error object.
    # If attribute or type needs to be overridden, use +override_options+.
    def import(error, override_options = {})
      override_options = override_options.dup
      [:attribute, :type].each do |key|
        if override_options.key?(key)
          override_options[key] = override_options[key].to_sym
        end
      end
      @errors.append(NestedError.new(@base, error, override_options))
    end

    # Merges the errors from <tt>other</tt>,
    # each Error wrapped as NestedError.
    def merge!(other)
      return errors if equal?(other)

      other.errors.each { |error|
        import(error)
      }
    end

    # Search for errors matching +attribute+, +type+, or +options+.
    #
    # Only supplied params will be matched.
    def where(attribute, type = nil, **options)
      attribute, type, options = normalize_arguments(attribute, type, **options)
      @errors.select { |error|
        error.match?(attribute, type, **options)
      }
    end

    # Returns +true+ if the error messages include an error for the given key
    # +attribute+, +false+ otherwise.
    def include?(attribute)
      @errors.any? { |error|
        error.match?(attribute.to_sym)
      }
    end
    alias :has_key? :include?
    alias :key? :include?

    # Delete messages for +key+. Returns the deleted messages.
    def delete(attribute, type = nil, **options)
      attribute, type, options = normalize_arguments(attribute, type, **options)
      matches = where(attribute, type, **options)
      matches.each do |error|
        @errors.delete(error)
      end
      deleted = matches.map(&:message)
      ValidationsSupport.present?(deleted) ? deleted : nil
    end

    # When passed a symbol or a name of a method, returns an array of errors
    # for the method.
    def [](attribute)
      messages_for(attribute)
    end

    # Returns all error attribute names
    def attribute_names
      @errors.map(&:attribute).uniq.freeze
    end

    # Returns a Hash that can be used as the JSON representation for this
    # object.
    def as_json(options = nil)
      to_hash(options && options[:full_messages])
    end

    # Returns a Hash of attributes with their error messages. If +full_messages+
    # is +true+, it will contain full messages (see +full_message+).
    def to_hash(full_messages = false)
      message_method = full_messages ? :full_message : :message
      group_by_attribute.transform_values do |errors|
        errors.map(&message_method)
      end
    end

    undef :to_h

    EMPTY_ARRAY = [].freeze # :nodoc:

    # Returns a Hash of attributes with an array of their error messages.
    def messages
      hash = to_hash
      hash.default = EMPTY_ARRAY
      hash.freeze
      hash
    end

    # Returns a Hash of attributes with an array of their error details.
    def details
      hash = group_by_attribute.transform_values do |errors|
        errors.map(&:details)
      end
      hash.default = EMPTY_ARRAY
      hash.freeze
      hash
    end

    # Returns a Hash of attributes with an array of their Error objects.
    def group_by_attribute
      @errors.group_by(&:attribute)
    end

    # Adds a new error of +type+ on +attribute+.
    # More than one error can be added to the same +attribute+.
    # If no +type+ is supplied, <tt>:invalid</tt> is assumed.
    def add(attribute, type = :invalid, **options)
      attribute, type, options = normalize_arguments(attribute, type, **options)
      error = Error.new(@base, attribute, type, **options)

      if exception = options[:strict]
        exception = ActiveModel::StrictValidationFailed if exception == true
        raise exception, error.full_message
      end

      @errors.append(error)

      error
    end

    # Returns +true+ if an error matches provided +attribute+ and +type+,
    # or +false+ otherwise. +type+ is treated the same as for +add+.
    def added?(attribute, type = :invalid, options = {})
      attribute, type, options = normalize_arguments(attribute, type, **options)

      if type.is_a? Symbol
        @errors.any? { |error|
          error.strict_match?(attribute, type, **options)
        }
      else
        messages_for(attribute).include?(type)
      end
    end

    # Returns +true+ if an error on the attribute with the given type is
    # present, or +false+ otherwise. +type+ is treated the same as for +add+.
    def of_kind?(attribute, type = :invalid)
      attribute, type = normalize_arguments(attribute, type)

      if type.is_a? Symbol
        !where(attribute, type).empty?
      else
        messages_for(attribute).include?(type)
      end
    end

    # Returns all the full error messages in an array.
    def full_messages
      @errors.map(&:full_message)
    end
    alias :to_a :full_messages

    # Returns all the full error messages for a given attribute in an array.
    def full_messages_for(attribute, type = nil)
      where(attribute, type).map(&:full_message).freeze
    end

    # Returns all the error messages for a given attribute in an array.
    def messages_for(attribute, type = nil)
      where(attribute, type).map(&:message)
    end

    # Returns a full message for a given attribute.
    def full_message(attribute, message)
      Error.full_message(attribute, message, @base)
    end

    # Translates an error message in its default scope
    # (<tt>activemodel.errors.messages</tt>).
    def generate_message(attribute, type = :invalid, options = {})
      Error.generate_message(attribute, type, @base, options)
    end

    private
      def instance_variables_to_inspect
        [:@errors].freeze
      end

      def normalize_arguments(attribute, type, **options)
        # Evaluate proc first
        if type.respond_to?(:call)
          type = type.call(@base, options)
        end

        [attribute.to_sym, type, options]
      end
  end

  # Raised when a validation cannot be corrected by end users and are considered
  # exceptional.
  class StrictValidationFailed < StandardError
  end

  # Raised when attribute values are out of range.
  class RangeError < ::RangeError
  end

  # ---- activemodel/lib/active_model/validations.rb
  module Validations
    # `included do ... end`, run as Concern#append_features runs it:
    #
    #   extend ActiveModel::Naming
    #   extend ActiveModel::Callbacks
    #   extend ActiveModel::Translation
    #
    #   extend  HelperMethods
    #   include HelperMethods
    #
    #   define_callbacks :validate, scope: :name
    #
    #   class_attribute :_validators, instance_writer: false, default: Hash.new { |h, k| h[k] = [] }
    def self.append_features(base)
      return false if base < self
      super
      base.extend ClassMethods
      base.class_eval do
        extend ActiveModel::Naming
        extend ActiveModel::Callbacks
        extend ActiveModel::Translation

        extend  HelperMethods
        include HelperMethods

        define_callbacks :validate, scope: :name

        @__class_attr__validators = Hash.new { |h, k| h[k] = [] }
      end
    end

    # The class_attribute's instance reader and predicate.
    def _validators
      defined?(@_validators) ? @_validators : self.class._validators
    end

    def _validators?
      !!_validators
    end

    module ClassMethods
      # The class_attribute's reader, writer and predicate: the value the
      # nearest class that assigned one holds.
      def _validators
        owner = self
        owner = owner.superclass until owner.instance_variable_defined?(:@__class_attr__validators)
        owner.instance_variable_get(:@__class_attr__validators)
      end

      def _validators=(value)
        @__class_attr__validators = value
      end

      def _validators?
        !!_validators
      end

      # Validates each attribute against a block.
      def validates_each(*attr_names, &block)
        validates_with BlockValidator, _merge_attributes(attr_names), &block
      end

      VALID_OPTIONS_FOR_VALIDATE = [:on, :if, :unless, :prepend, :except_on].freeze # :nodoc:

      # Adds a validation method or block to the class.
      def validate(*args, &block)
        options = ValidationsSupport.extract_options!(args)

        if args.all?(Symbol)
          options.each_key do |k|
            unless VALID_OPTIONS_FOR_VALIDATE.include?(k)
              raise ArgumentError.new("Unknown key: #{k.inspect}. Valid keys are: #{VALID_OPTIONS_FOR_VALIDATE.map(&:inspect).join(', ')}. Perhaps you meant to call `validates` instead of `validate`?")
            end
          end
        end

        if options.key?(:on)
          options = options.merge(if: [predicate_for_validation_context(options[:on]), *options[:if]])
        end

        if options.key?(:except_on)
          options = options.dup
          options[:except_on] = Array(options[:except_on])
          options[:unless] = [
            ->(o) { options[:except_on].intersect?(Array(o.validation_context)) },
            *options[:unless]
          ]
        end

        set_callback(:validate, *args, options, &block)
      end

      # List all validators that are being used to validate the model using
      # +validates_with+ method.
      def validators
        _validators.values.flatten.uniq
      end

      # Clears all of the validators and validations.
      def clear_validators!
        reset_callbacks(:validate)
        _validators.clear
      end

      # List all validators that are being used to validate a specific attribute.
      def validators_on(*attributes)
        attributes.flat_map do |attribute|
          _validators[attribute.to_sym]
        end
      end

      # Returns +true+ if +attribute+ is an attribute method, +false+ otherwise.
      def attribute_method?(attribute)
        method_defined?(attribute)
      end

      # Copy validators on inheritance.
      def inherited(base) # :nodoc:
        dup = _validators.dup
        base._validators = dup.each { |k, v| dup[k] = v.dup }
        super
      end

      private
        @@predicates_for_validation_contexts = {}

        def predicate_for_validation_context(context)
          context = context.is_a?(Array) ? context.sort : Array(context)

          @@predicates_for_validation_contexts[context] ||= -> (model) do
            if model.validation_context.is_a?(Array)
              model.validation_context.any? { |model_context| context.include?(model_context) }
            else
              context.include?(model.validation_context)
            end
          end
        end
    end

    # Clean the +Errors+ object if instance is duped.
    def initialize_dup(other) # :nodoc:
      @errors = nil
      super
    end

    # Returns the +Errors+ object that holds all information about attribute
    # error messages.
    def errors
      @errors ||= Errors.new(self)
    end

    # Runs all the specified validations and returns +true+ if no errors were
    # added otherwise +false+.
    def valid?(context = nil)
      current_context = validation_context
      context_for_validation.context = context
      errors.clear
      run_validations!
    ensure
      context_for_validation.context = current_context
    end

    alias_method :validate, :valid?

    def freeze
      errors
      context_for_validation

      super
    end

    # Performs the opposite of <tt>valid?</tt>. Returns +true+ if errors were
    # added, +false+ otherwise.
    def invalid?(context = nil)
      !valid?(context)
    end

    # Runs all the validations within the specified context. Returns +true+ if
    # no errors are found, raises +ValidationError+ otherwise.
    def validate!(context = nil)
      valid?(context) || raise_validation_error
    end

    # Hook method defining how an attribute value should be retrieved.
    def read_attribute_for_validation(attribute)
      send(attribute) if respond_to?(attribute, true)
    end

    # Returns the context when running validations.
    def validation_context
      context_for_validation.context
    end

  private
    def context_for_validation
      @context_for_validation ||= ValidationContext.new
    end

    def init_internals
      super
      @errors = nil
      @context_for_validation = nil
    end

    def run_validations!
      _run_validate_callbacks
      errors.empty?
    end

    def raise_validation_error # :doc:
      raise(ValidationError.new(self))
    end
  end

  # = Active \Model \ValidationError
  #
  # Raised by <tt>validate!</tt> when the model is invalid.
  class ValidationError < StandardError
    attr_reader :model

    def initialize(model)
      @model = model
      errors = @model.errors.full_messages.join(", ")
      super(I18n.t(:"#{@model.class.i18n_scope}.errors.messages.model_invalid", errors: errors, default: :"errors.messages.model_invalid"))
    end
  end

  class ValidationContext # :nodoc:
    attr_accessor :context
  end

  module Validations
    # ---- validations/helper_methods.rb
    module HelperMethods # :nodoc:
      private
        def _merge_attributes(attr_names)
          options = ValidationsSupport.symbolize_keys(ValidationsSupport.extract_options!(attr_names))
          attr_names.flatten!
          options[:attributes] = attr_names
          options
        end
    end

    # ---- validations/resolve_value.rb
    module ResolveValue # :nodoc:
      def resolve_value(record, value)
        case value
        when Proc
          if value.arity == 0
            value.call
          else
            value.call(record)
          end
        when Symbol
          record.send(value)
        else
          if value.respond_to?(:call)
            value.call(record)
          else
            value
          end
        end
      end
    end

    # ---- validations/absence.rb
    class AbsenceValidator < EachValidator # :nodoc:
      def validate_each(record, attr_name, value)
        record.errors.add(attr_name, :present, **options) if ValidationsSupport.present?(value)
      end
    end

    module HelperMethods
      def validates_absence_of(*attr_names)
        validates_with AbsenceValidator, _merge_attributes(attr_names)
      end
    end

    # ---- validations/acceptance.rb
    class AcceptanceValidator < EachValidator # :nodoc:
      def initialize(options)
        super({ allow_nil: true, accept: ["1", true] }.merge!(options))
        setup!(options[:class])
      end

      def validate_each(record, attribute, value)
        unless acceptable_option?(value)
          record.errors.add(attribute, :accepted, **options.except(:accept, :allow_nil))
        end
      end

      private
        def setup!(klass)
          define_attributes = LazilyDefineAttributes.new(attributes)
          klass.include(define_attributes) unless klass.included_modules.include?(define_attributes)
        end

        def acceptable_option?(value)
          Array(options[:accept]).include?(value)
        end

        class LazilyDefineAttributes < Module # :nodoc:
          def initialize(attributes)
            @attributes = attributes.map(&:to_s)
          end

          def included(klass)
            @lock = Mutex.new
            mod = self

            define_method(:respond_to_missing?) do |method_name, include_private = false|
              mod.define_on(klass)
              super(method_name, include_private) || mod.matches?(method_name)
            end

            define_method(:method_missing) do |method_name, *args, &block|
              mod.define_on(klass)
              if mod.matches?(method_name)
                send(method_name, *args, &block)
              else
                super(method_name, *args, &block)
              end
            end
          end

          def matches?(method_name)
            attr_name = method_name.to_s.chomp("=")
            attributes.any? { |name| name == attr_name }
          end

          def define_on(klass)
            @lock&.synchronize do
              return unless @lock

              attr_readers = attributes.reject { |name| klass.attribute_method?(name) }
              attr_writers = attributes.reject { |name| klass.attribute_method?("#{name}=") }

              attr_reader(*attr_readers)
              attr_writer(*attr_writers)

              remove_method :respond_to_missing?
              remove_method :method_missing

              @lock = nil
            end
          end

          def ==(other)
            self.class == other.class && attributes == other.attributes
          end

          protected
            attr_reader :attributes
        end
    end

    module HelperMethods
      def validates_acceptance_of(*attr_names)
        validates_with AcceptanceValidator, _merge_attributes(attr_names)
      end
    end

    # ---- validations/callbacks.rb
    module Callbacks
      # `included do ... end`, run as Concern#append_features runs it:
      #
      #   include ActiveSupport::Callbacks
      #   define_callbacks :validation,
      #                    skip_after_callbacks_if_terminated: true,
      #                    scope: [:kind, :name]
      def self.append_features(base)
        return false if base < self
        super
        base.extend ClassMethods
        base.class_eval do
          include ActiveSupport::Callbacks
          define_callbacks :validation,
                           skip_after_callbacks_if_terminated: true,
                           scope: [:kind, :name]
        end
      end

      module ClassMethods
        def before_validation(*args, &block)
          options = ValidationsSupport.extract_options!(args)

          set_options_for_callback(options)

          set_callback(:validation, :before, *args, options, &block)
        end

        def after_validation(*args, &block)
          options = ValidationsSupport.extract_options!(args)
          options = options.dup
          options[:prepend] = true

          set_options_for_callback(options)

          set_callback(:validation, :after, *args, options, &block)
        end

        private
          def set_options_for_callback(options)
            if options.key?(:on)
              options[:on] = Array(options[:on])
              options[:if] = [
                ->(o) {
                  options[:on].intersect?(Array(o.validation_context))
                },
                *options[:if]
              ]
            end

            if options.key?(:except_on)
              options[:except_on] = Array(options[:except_on])
              options[:unless] = [
                ->(o) {
                  options[:except_on].intersect?(Array(o.validation_context))
                },
                *options[:unless]
              ]
            end
          end
      end

    private
      # Override run_validations! to include callbacks.
      def run_validations!
        _run_validation_callbacks { super }
      end
    end

    # ---- validations/clusivity.rb
    module Clusivity # :nodoc:
      include ResolveValue

      ERROR_MESSAGE = "An object with the method #include? or a proc, lambda or symbol is required, " \
                      "and must be supplied as the :in (or :within) option of the configuration hash"

      def check_validity!
        unless delimiter.respond_to?(:include?) || delimiter.respond_to?(:call) || delimiter.respond_to?(:to_sym)
          raise ArgumentError, ERROR_MESSAGE
        end
      end

    private
      def include?(record, value)
        members = resolve_value(record, delimiter)

        if value.is_a?(Array)
          value.all? { |v| members.public_send(inclusion_method(members), v) }
        else
          members.public_send(inclusion_method(members), value)
        end
      end

      def delimiter
        @delimiter ||= options[:in] || options[:within]
      end

      # After Ruby 2.2, <tt>Range#include?</tt> on non-number-or-time-ish ranges checks all
      # possible values in the range for equality, which is slower but more accurate.
      # <tt>Range#cover?</tt> uses the previous logic of comparing a value with the range
      # endpoints, which is fast but is only accurate on Numeric, Time, Date,
      # or DateTime ranges.
      def inclusion_method(enumerable)
        if enumerable.is_a? Range
          case enumerable.begin || enumerable.end
          when Numeric, Time, DateTime, Date
            :cover?
          else
            :include?
          end
        else
          :include?
        end
      end
    end

    # ---- validations/comparability.rb
    module Comparability # :nodoc:
      COMPARE_CHECKS = { greater_than: :>, greater_than_or_equal_to: :>=,
        equal_to: :==, less_than: :<, less_than_or_equal_to: :<=,
        other_than: :!= }.freeze

      def error_options(value, option_value)
        options.except(*COMPARE_CHECKS.keys).merge!(
          count: option_value,
          value: value
        )
      end
    end

    # ---- validations/comparison.rb
    class ComparisonValidator < EachValidator # :nodoc:
      include Comparability
      include ResolveValue

      def check_validity!
        unless options.keys.intersect?(COMPARE_CHECKS.keys)
          raise ArgumentError, "Expected one of :greater_than, :greater_than_or_equal_to, "\
          ":equal_to, :less_than, :less_than_or_equal_to, or :other_than option to be supplied."
        end
      end

      def validate_each(record, attr_name, value)
        options.slice(*COMPARE_CHECKS.keys).each do |option, raw_option_value|
          option_value = resolve_value(record, raw_option_value)

          if value.nil? || ValidationsSupport.blank?(value)
            return record.errors.add(attr_name, :blank, **error_options(value, option_value))
          end

          unless value.public_send(COMPARE_CHECKS[option], option_value)
            record.errors.add(attr_name, option, **error_options(value, option_value))
          end
        rescue ArgumentError => e
          record.errors.add(attr_name, e.message)
        end
      end
    end

    module HelperMethods
      def validates_comparison_of(*attr_names)
        validates_with ComparisonValidator, _merge_attributes(attr_names)
      end
    end

    # ---- validations/confirmation.rb
    class ConfirmationValidator < EachValidator # :nodoc:
      def initialize(options)
        super({ case_sensitive: true }.merge!(options))
        setup!(options[:class])
      end

      def validate_each(record, attribute, value)
        unless (confirmed = record.public_send("#{attribute}_confirmation")).nil?
          unless confirmation_value_equal?(value, confirmed)
            human_attribute_name = record.class.human_attribute_name(attribute)
            record.errors.add(:"#{attribute}_confirmation", :confirmation, **options.except(:case_sensitive).merge!(attribute: human_attribute_name))
          end
        end
      end

      private
        def setup!(klass)
          klass.attr_reader(*attributes.filter_map do |attribute|
            :"#{attribute}_confirmation" unless klass.method_defined?(:"#{attribute}_confirmation")
          end)

          klass.attr_writer(*attributes.filter_map do |attribute|
            :"#{attribute}_confirmation" unless klass.method_defined?(:"#{attribute}_confirmation=")
          end)
        end

        def confirmation_value_equal?(value, confirmed)
          if !options[:case_sensitive] && value.is_a?(String)
            value.casecmp(confirmed) == 0
          else
            value == confirmed
          end
        end
    end

    module HelperMethods
      def validates_confirmation_of(*attr_names)
        validates_with ConfirmationValidator, _merge_attributes(attr_names)
      end
    end

    # ---- validations/exclusion.rb
    class ExclusionValidator < EachValidator # :nodoc:
      include Clusivity

      def validate_each(record, attribute, value)
        if include?(record, value)
          record.errors.add(attribute, :exclusion, **options.except(:in, :within).merge!(value: value))
        end
      end
    end

    module HelperMethods
      def validates_exclusion_of(*attr_names)
        validates_with ExclusionValidator, _merge_attributes(attr_names)
      end
    end

    # ---- validations/format.rb
    class FormatValidator < EachValidator # :nodoc:
      include ResolveValue

      def validate_each(record, attribute, value)
        if options[:with]
          regexp = resolve_value(record, options[:with])
          record_error(record, attribute, :with, value) unless regexp.match?(value.to_s)
        elsif options[:without]
          regexp = resolve_value(record, options[:without])
          record_error(record, attribute, :without, value) if regexp.match?(value.to_s)
        end
      end

      def check_validity!
        unless options.include?(:with) ^ options.include?(:without)  # ^ == xor, or "exclusive or"
          raise ArgumentError, "Either :with or :without must be supplied (but not both)"
        end

        check_options_validity :with
        check_options_validity :without
      end

      private
        def record_error(record, attribute, name, value)
          record.errors.add(attribute, :invalid, **options.except(name).merge!(value: value))
        end

        def check_options_validity(name)
          if option = options[name]
            if option.is_a?(Regexp)
              if options[:multiline] != true && regexp_using_multiline_anchors?(option)
                raise ArgumentError, "The provided regular expression is using multiline anchors (^ or $), " \
                "which may present a security risk. Did you mean to use \\A and \\z, or forgot to add the " \
                ":multiline => true option?"
              end
            elsif !option.respond_to?(:call)
              raise ArgumentError, "A regular expression or a proc or lambda must be supplied as :#{name}"
            end
          end
        end

        def regexp_using_multiline_anchors?(regexp)
          source = regexp.source
          source.start_with?("^") || (source.end_with?("$") && !source.end_with?("\\$"))
        end
    end

    module HelperMethods
      def validates_format_of(*attr_names)
        validates_with FormatValidator, _merge_attributes(attr_names)
      end
    end

    # ---- validations/inclusion.rb
    class InclusionValidator < EachValidator # :nodoc:
      include Clusivity

      def validate_each(record, attribute, value)
        unless include?(record, value)
          record.errors.add(attribute, :inclusion, **options.except(:in, :within).merge!(value: value))
        end
      end
    end

    module HelperMethods
      def validates_inclusion_of(*attr_names)
        validates_with InclusionValidator, _merge_attributes(attr_names)
      end
    end

    # ---- validations/length.rb
    class LengthValidator < EachValidator # :nodoc:
      include ResolveValue

      MESSAGES  = { is: :wrong_length, minimum: :too_short, maximum: :too_long }.freeze
      CHECKS    = { is: :==, minimum: :>=, maximum: :<= }.freeze

      RESERVED_OPTIONS = [:minimum, :maximum, :within, :is, :too_short, :too_long].freeze

      def initialize(options)
        if range = (options.delete(:in) || options.delete(:within))
          raise ArgumentError, ":in and :within must be a Range" unless range.is_a?(Range)
          options[:minimum] = range.min if range.begin
          options[:maximum] = (range.exclude_end? ? range.end - 1 : range.end) if range.end
        end

        if options[:allow_blank] == false && options[:minimum].nil? && options[:is].nil?
          options[:minimum] = 1
        end

        super
      end

      def check_validity!
        keys = CHECKS.keys & options.keys

        if keys.empty?
          raise ArgumentError, "Range unspecified. Specify the :in, :within, :maximum, :minimum, or :is option."
        end

        keys.each do |key|
          value = options[key]

          unless (value.is_a?(Integer) && value >= 0) ||
                  value == Float::INFINITY || value == -Float::INFINITY ||
                  value.is_a?(Symbol) || value.is_a?(Proc)
            raise ArgumentError, ":#{key} must be a non-negative Integer, Infinity, Symbol, or Proc"
          end
        end
      end

      def validate_each(record, attribute, value)
        value_length = value.respond_to?(:length) ? value.length : value.to_s.length
        errors_options = options.except(*RESERVED_OPTIONS)

        CHECKS.each do |key, validity_check|
          next unless check_value = options[key]

          check_value = resolve_value(record, check_value)

          if !value.nil? || skip_nil_check?(key)
            next if value_length.public_send(validity_check, check_value)
          end

          errors_options[:count] = check_value

          default_message = options[MESSAGES[key]]
          errors_options[:message] ||= default_message if default_message

          record.errors.add(attribute, MESSAGES[key], **errors_options)
        end
      end

      private
        def skip_nil_check?(key)
          key == :maximum && options[:allow_nil].nil? && options[:allow_blank].nil?
        end
    end

    module HelperMethods
      def validates_length_of(*attr_names)
        validates_with LengthValidator, _merge_attributes(attr_names)
      end

      alias_method :validates_size_of, :validates_length_of
    end

    # ---- validations/numericality.rb
    class NumericalityValidator < EachValidator # :nodoc:
      include Comparability
      include ResolveValue

      # `in?` (core_ext/object/inclusion.rb): `range.include?(value)`.
      RANGE_CHECKS = { in: :in? }.freeze
      NUMBER_CHECKS = { odd: :odd?, even: :even? }.freeze

      RESERVED_OPTIONS = COMPARE_CHECKS.keys + NUMBER_CHECKS.keys + RANGE_CHECKS.keys + [:only_integer, :only_numeric]

      INTEGER_REGEX = /\A[+-]?\d+\z/

      HEXADECIMAL_REGEX = /\A[+-]?0[xX]/

      def check_validity!
        options.slice(*COMPARE_CHECKS.keys).each do |option, value|
          unless value.is_a?(Numeric) || value.is_a?(Proc) || value.is_a?(Symbol)
            raise ArgumentError, ":#{option} must be a number, a symbol or a proc"
          end
        end

        options.slice(*RANGE_CHECKS.keys).each do |option, value|
          unless value.is_a?(Range) || value.is_a?(Proc) || value.is_a?(Symbol)
            raise ArgumentError, ":#{option} must be a range, a symbol or a proc"
          end
        end
      end

      def validate_each(record, attr_name, value, precision: Float::DIG, scale: nil)
        unless is_number?(value, precision, scale)
          record.errors.add(attr_name, :not_a_number, **filtered_options(value))
          return
        end

        if allow_only_integer?(record) && !is_integer?(value)
          record.errors.add(attr_name, :not_an_integer, **filtered_options(value))
          return
        end

        value = parse_as_number(value, precision, scale)

        options.slice(*RESERVED_OPTIONS).each do |option, option_value|
          if NUMBER_CHECKS.include?(option)
            unless value.to_i.public_send(NUMBER_CHECKS[option])
              record.errors.add(attr_name, option, **filtered_options(value))
            end
          elsif RANGE_CHECKS.include?(option)
            option_value = resolve_value(record, option_value)
            unless option_value.is_a?(Range)
              raise ArgumentError, ":#{option} must return a range"
            end

            unless option_value.include?(value)
              record.errors.add(attr_name, option, **filtered_options(value).merge!(count: option_value))
            end
          elsif COMPARE_CHECKS.include?(option)
            option_value = option_as_number(record, option_value, precision, scale)

            unless value.public_send(COMPARE_CHECKS[option], option_value)
              record.errors.add(attr_name, option, **filtered_options(value).merge!(count: option_value))
            end
          end
        end
      end

    private
      def option_as_number(record, option_value, precision, scale)
        parse_as_number(resolve_value(record, option_value), precision, scale)
      end

      def parse_as_number(raw_value, precision, scale)
        if raw_value.is_a?(Float)
          parse_float(raw_value, precision, scale)
        elsif raw_value.is_a?(BigDecimal)
          round(raw_value, scale)
        elsif raw_value.is_a?(Numeric)
          raw_value
        elsif is_integer?(raw_value)
          raw_value.to_i
        elsif !is_hexadecimal_literal?(raw_value)
          parse_float(Kernel.Float(raw_value), precision, scale)
        end
      end

      def parse_float(raw_value, precision, scale)
        round(raw_value, scale).to_d(precision)
      end

      def round(raw_value, scale)
        scale ? raw_value.round(scale) : raw_value
      end

      def is_number?(raw_value, precision, scale)
        if options[:only_numeric] && !raw_value.is_a?(Numeric)
          return false
        end

        !parse_as_number(raw_value, precision, scale).nil?
      rescue ArgumentError, TypeError
        false
      end

      def is_integer?(raw_value)
        INTEGER_REGEX.match?(raw_value.to_s)
      end

      def is_hexadecimal_literal?(raw_value)
        HEXADECIMAL_REGEX.match?(raw_value.to_s)
      end

      def filtered_options(value)
        filtered = options.except(*RESERVED_OPTIONS)
        filtered[:value] = value
        filtered
      end

      def allow_only_integer?(record)
        resolve_value(record, options[:only_integer])
      end

      def prepare_value_for_validation(value, record, attr_name)
        return value if record_attribute_changed_in_place?(record, attr_name)

        came_from_user = :"#{attr_name}_came_from_user?"

        if record.respond_to?(came_from_user)
          if record.public_send(came_from_user)
            raw_value = record.public_send(:"#{attr_name}_before_type_cast")
          elsif record.respond_to?(:read_attribute)
            raw_value = record.read_attribute(attr_name)
          end
        else
          before_type_cast = :"#{attr_name}_before_type_cast"
          if record.respond_to?(before_type_cast)
            raw_value = record.public_send(before_type_cast)
          end
        end
        raw_value || value
      end

      def record_attribute_changed_in_place?(record, attr_name)
        record.respond_to?(:attribute_changed_in_place?) &&
          record.attribute_changed_in_place?(attr_name.to_s)
      end
    end

    module HelperMethods
      def validates_numericality_of(*attr_names)
        validates_with NumericalityValidator, _merge_attributes(attr_names)
      end
    end

    # ---- validations/presence.rb
    class PresenceValidator < EachValidator # :nodoc:
      def validate_each(record, attr_name, value)
        record.errors.add(attr_name, :blank, **options) if ValidationsSupport.blank?(value)
      end
    end

    module HelperMethods
      def validates_presence_of(*attr_names)
        validates_with PresenceValidator, _merge_attributes(attr_names)
      end
    end

    # ---- validations/validates.rb
    module ClassMethods
      # This method is a shortcut to all default validators and any custom
      # validator classes ending in 'Validator'.
      def validates(*attributes)
        defaults = ValidationsSupport.extract_options!(attributes).dup
        # `defaults.slice!(*keys)` (core_ext/hash/slice.rb): `defaults`
        # keeps the default keys, the rest are the validations.
        default_keys = _validates_default_keys
        validations = defaults.slice(*(defaults.keys - default_keys))
        sliced = defaults.slice(*default_keys)
        sliced.default = defaults.default
        defaults.replace(sliced)

        raise ArgumentError, "You need to supply at least one attribute" if attributes.empty?
        raise ArgumentError, "You need to supply at least one validation" if validations.empty?

        defaults[:attributes] = attributes

        validations.each do |key, options|
          key = "#{ActiveSupport::Inflector.camelize(key.to_s)}Validator"

          begin
            validator = const_get(key)
          rescue NameError
            raise ArgumentError, "Unknown validator: '#{key}'"
          end

          next unless options

          validates_with(validator, _merge_validates_options(defaults, _parse_validates_options(options)))
        end
      end

      # This method is used to define validations that cannot be corrected by end
      # users and are considered exceptional.
      def validates!(*attributes)
        options = ValidationsSupport.extract_options!(attributes)
        options[:strict] = true
        validates(*(attributes << options))
      end

    private
      # When creating custom validators, it might be useful to be able to specify
      # additional default keys. This can be done by overwriting this method.
      def _validates_default_keys
        [:if, :unless, :on, :allow_blank, :allow_nil, :strict, :except_on]
      end

      def _merge_validates_options(defaults, validator_options)
        defaults.merge(validator_options) do |key, default_val, validator_val|
          if key == :if || key == :unless || key == :on
            Array(default_val) + Array(validator_val)
          else
            validator_val
          end
        end
      end

      def _parse_validates_options(options)
        case options
        when TrueClass
          {}
        when Hash
          options
        when Range, Array
          { in: options }
        else
          { with: options }
        end
      end
    end

    # ---- validations/with.rb
    class WithValidator < EachValidator # :nodoc:
      def validate_each(record, attr, val)
        method_name = options[:with]

        if record.method(method_name).arity == 0
          record.send method_name
        else
          record.send method_name, attr
        end
      end
    end

    module ClassMethods
      # Passes the record off to the class or classes specified and allows them
      # to add errors based on more complex conditions.
      def validates_with(*args, &block)
        options = ValidationsSupport.extract_options!(args)
        options[:class] = self

        args.each do |klass|
          validator = klass.new(options.dup, &block)

          if validator.respond_to?(:attributes) && !validator.attributes.empty?
            validator.attributes.each do |attribute|
              _validators[attribute.to_sym] << validator
            end
          else
            _validators[nil] << validator
          end

          validate(validator, options)
        end
      end
    end

    # Passes the record off to the class or classes specified and allows them
    # to add errors based on more complex conditions.
    def validates_with(*args, &block)
      options = ValidationsSupport.extract_options!(args)
      options[:class] = self.class

      args.each do |klass|
        validator = klass.new(options.dup, &block)
        validator.validate(self)
      end
    end
  end
end
