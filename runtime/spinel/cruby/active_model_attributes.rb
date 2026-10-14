# frozen_string_literal: true

# ActiveModel::Attributes, ported from the activemodel native Core runs
# (Shopify/rails 5813559f4aed, 8.2.0.alpha: lib/active_model/attributes.rb
# and attribute_registration.rb) for the PostgreSQL CRuby tree, carried
# where app code (a gem compiled as source among it) names
# ActiveModel::Attributes. Core's tableless classes include it for the
# `attribute :name, :type, default:` declarations (the
# maintenance_tasks gem's Task, with the `attribute` calls its concerns'
# `gcs_csv_collection` make in a task subclass body; the shopify_ledger
# client's inputs; trust_battery_client's SignalProvenance).
#
# Each method follows the native one, over the ports this tree carries:
# ActiveModel::Type (runtime/active_model_type.rb) and ActiveModel::Attribute
# (runtime/active_model_attribute.rb), and ActiveModel::AttributeAssignment
# (runtime/active_record_core.rb), which Attributes includes.
#
# Differences, at the edges:
# - ActiveSupport::Concern is not used: the includes Attributes makes and
#   its ClassMethods are applied as Concern#append_features applies them,
#   once per class, with an includer that is a module (an app concern)
#   deferring to its own includers, as a Concern does.
# - ActiveModel::AttributeSet and AttributeMethods are not ported: the
#   instance keeps its attributes in a Hash of name => ActiveModel::Attribute,
#   and the reader and writer of each attribute are defined directly in a
#   module the class includes. `attribute_method_suffix` and its generated
#   predicates are refused where AttributeMethods is absent.
# - The attributes of a class are the ones declared on it and its
#   ancestors, in declaration order, an inherited one redeclared in place.

module ActiveModel
  # runtime/active_record_core.rb defines it; named here so a class that
  # includes it beside Attributes (Core's maintenance_tasks Task) finds the
  # tree carrying it with this port (analyze/runtime_reads.rs).
  module AttributeAssignment
  end

  module Attributes
    NO_DEFAULT_PROVIDED = Object.new.freeze # :nodoc:

    def self.append_features(base)
      unless base.is_a?(Class)
        base.extend(ActiveSupport::Concern) if defined?(ActiveSupport::Concern) && !base.instance_variable_defined?(:@_dependencies)
        return super unless base.instance_variable_defined?(:@_dependencies)
        base.instance_variable_get(:@_dependencies) << self
        return false
      end
      return false if base < self
      base.include(ActiveModel::AttributeAssignment)
      super
      base.extend(ClassMethods)
      # A class lowered with the lite validations surface (`errors` an
      # Array: the lowering replaced its `include ActiveModel::Validations`)
      # has no class-side `validates`; a concern's macro that declares one
      # when it runs (maintenance_tasks' `gcs_csv_collection`) reaches it
      # here.
      unless defined?(ActiveModel::Validations) && base < ActiveModel::Validations
        base.extend(LiteValidations::ClassMethods)
        base.prepend(LiteValidations)
      end
    end

    # `validates :name, presence: true` and `validates_presence_of` for a
    # class whose `validate` is the lowering's lite one (`errors` and
    # `error_details` Arrays; runtime/active_model_errors.rb's
    # ErrorAttributes records each message's attribute). Only presence is
    # run; any other rule, option or macro is refused by name.
    module LiteValidations # :nodoc:
      module ClassMethods # :nodoc:
        def validates(*attributes, **options)
          presence = options.delete(:presence)
          unless options.empty? && presence
            rest = options.keys
            rest.unshift(:presence) if presence && !(presence == true)
            raise NotImplementedError,
              "roundhouse: #{name}.validates #{attributes.map(&:inspect).join(", ")}: " \
              "#{(rest.empty? ? [:presence] : rest).map(&:inspect).join(", ")} is not ported for a class without ActiveModel::Validations " \
              "(only `presence: true`)"
          end
          raise ArgumentError, "You need to supply at least one attribute" if attributes.empty?
          raise ArgumentError, "roundhouse: #{name}.validates: presence options (#{presence.inspect}) are not ported" unless presence == true
          own_lite_validations << attributes.map(&:to_sym)
          nil
        end

        def validates_presence_of(*attributes, **options)
          raise NotImplementedError, "roundhouse: #{name}.validates_presence_of: #{options.keys.map(&:inspect).join(", ")} is not ported" unless options.empty?
          validates(*attributes, presence: true)
        end

        def validate(*methods, **options, &block)
          raise NotImplementedError, "roundhouse: #{name}.validate is not ported for a class without ActiveModel::Validations"
        end

        def _lite_validations # :nodoc:
          inherited = superclass.respond_to?(:_lite_validations) ? superclass._lite_validations : []
          inherited + own_lite_validations
        end

        private
          def own_lite_validations
            @own_lite_validations ||= []
          end
      end

      def validate(*)
        result = super
        self.class._lite_validations.each do |attributes|
          attributes.each do |attribute|
            next unless ActiveSupport.blank?(public_send(attribute))
            ActiveModel::ErrorAttributes.add_humanized(errors, attribute, "#{ActiveSupport.humanize(attribute.to_s)} can't be blank", :blank, {})
            error_details << ActiveRecord::ErrorDetail.new(attribute.to_s, "blank", "can't be blank") if respond_to?(:error_details)
          end
        end
        result
      end
    end

    module ClassMethods # :nodoc:
      # Defines a model attribute: its +name+, +cast_type+ (a Symbol the
      # type registry knows, a type object, or nil to keep an inherited
      # attribute's type, else a generic Value), and an optional
      # +default+ (a value, or a callable evaluated for each new object).
      # +options+ are the registry's (`precision:`, `limit:`, ...).
      def attribute(name, cast_type = nil, default: NO_DEFAULT_PROVIDED, **options)
        name = name.to_s
        type =
          case cast_type
          when nil
            raise ArgumentError, "roundhouse: attribute #{name.inspect}: type options (#{options.keys.join(", ")}) need a type" unless options.empty?
            inherited_type = _attribute_types[name]
            inherited_type || ActiveModel::Type::Value.new
          when Symbol then ActiveModel::Type.lookup(cast_type, **options)
          else
            raise ArgumentError, "roundhouse: attribute #{name.inspect}: options (#{options.keys.join(", ")}) given with a type object" unless options.empty?
            cast_type
          end
        # A redeclaration without a default keeps the one in effect.
        default = _attribute_definitions[name]&.last if default.equal?(NO_DEFAULT_PROVIDED) && _attribute_definitions.key?(name)
        own_attribute_definitions[name] = [type, default]
        define_attribute_accessors(name)
        name
      end

      # The names of the declared attributes.
      def attribute_names
        _attribute_types.keys
      end

      # The type of each declared attribute, by name.
      def attribute_types
        _attribute_types.dup
      end

      def type_for_attribute(attribute_name, &block)
        attribute_name = attribute_name.to_s
        if (type = _attribute_types[attribute_name])
          type
        elsif block
          yield attribute_name
        else
          ActiveModel::Type::Value.new
        end
      end

      def attribute_method_suffix(*)
        raise NotImplementedError, "roundhouse: #{name}.attribute_method_suffix needs ActiveModel::AttributeMethods, which is not ported with ActiveModel::Attributes"
      end
      alias_method :attribute_method_prefix, :attribute_method_suffix
      alias_method :attribute_method_affix, :attribute_method_suffix

      # The attributes' defaults, in declaration order: a new
      # ActiveModel::Attribute for each.
      def _default_attributes
        _attribute_definitions.to_h do |name, (type, default)|
          [name, _default_attribute(name, type, default)]
        end
      end

      def _attribute_definitions # :nodoc:
        inherited = superclass.respond_to?(:_attribute_definitions) ? superclass._attribute_definitions : {}
        inherited.merge(own_attribute_definitions)
      end

      def _attribute_types # :nodoc:
        _attribute_definitions.transform_values(&:first)
      end

      private
        def own_attribute_definitions
          @own_attribute_definitions ||= {}
        end

        def _default_attribute(name, type, default)
          if default.equal?(NO_DEFAULT_PROVIDED)
            ActiveModel::Attribute.with_cast_value(name, nil, type)
          else
            value = default.respond_to?(:call) ? default.call : default
            ActiveModel::Attribute.from_user(name, value, type)
          end
        end

        def generated_attribute_methods
          @generated_attribute_methods ||= Module.new.tap { |mod| include mod }
        end

        def define_attribute_accessors(name)
          generated_attribute_methods.module_eval do
            define_method(name) { attribute(name) }
            define_method(:"#{name}=") { |value| _write_attribute(name, value) }
          end
        end
    end

    def initialize(*)
      @attributes = self.class._default_attributes
      super
    end

    def initialize_dup(other) # :nodoc:
      super
      @attributes = @attributes.transform_values(&:dup)
    end

    def freeze
      @attributes = @attributes.clone.freeze unless frozen?
      super
    end

    # Returns a hash of all the attributes with their names as keys and the
    # values of the attributes as values.
    def attributes
      @attributes.transform_values(&:value)
    end

    # Returns an array of attribute names as strings.
    def attribute_names
      @attributes.keys
    end

    private
      def _write_attribute(attr_name, value)
        attr_name = attr_name.to_s
        @attributes[attr_name] = @attributes.fetch(attr_name).with_value_from_user(value)
        value
      end

      def attribute(attr_name)
        @attributes.fetch(attr_name.to_s).value
      end
  end
end
