# frozen_string_literal: true

# ActiveRecord::Base's constructor, `reload` and mass assignment as
# methods of modules Base includes, on the PostgreSQL CRuby tree, from the
# activemodel and activerecord native Core runs (Shopify/rails
# 5813559f4aed, 8.2.0.alpha): ActiveRecord::Core#initialize and its
# private `init_internals` (core.rb), Persistence#reload (persistence.rb),
# ActiveModel::AttributeAssignment with ForbiddenAttributesProtection
# (attribute_assignment.rb, forbidden_attributes_protection.rb),
# ActiveRecord::AttributeAssignment#_assign_attributes, and
# ActiveModel::UnknownAttributeError (errors.rb). A module a gem includes
# into Base later sits above these and supers into them, as in Rails:
# shopify-money 4.1.1's MoneyColumn::ActiveRecordHooks overrides
# `init_internals`, `_assign_attributes` and `reload`.
#
# Differences, at the edges:
# - `init_internals` is the runtime base's slot setup (the body of
#   runtime/ruby/active_record/base.rb's `initialize`, kept here as the
#   private `_runtime_internals`), not Rails' @readonly/@strict_loading
#   flags.
# - Core#initialize ignores its attributes: the model's compiled
#   `initialize` calls `super()`, writes its column defaults and then
#   calls `assign_attributes` with them
#   (lower::model_to_library::assign_attributes), so `init_internals`
#   runs before any attribute write, as in Rails. Records loaded from the
#   database are built through `new`, so an override of `initialize` runs
#   for them where Rails' `instantiate` skips it;
#   lower::model_to_library::macro_replay refuses a Base include's
#   override of it (`PARTIAL_BASE_HOOKS`).
# - `reload` takes no options (Rails' `lock:`); it re-reads the row
#   through the runtime's `_adapter_reload`.
# - Multiparameter keys (`"written_on(1i)"`) raise NotImplementedError.
#
# Also Core#readonly! / #readonly? and the ReadOnlyRecord that
# Persistence raises for a readonly record's save, destroy and touch
# (core.rb, persistence.rb): after the before_save / before_destroy
# callbacks, as Rails raises inside the callback run. And the slice of
# the association reflections app code reads (reflection.rb), which a
# model answers from `reflect_on_association` for each association it
# compiled (emit::ruby::library::apply_association_reflections) and for
# those a class macro declares (lower::model_to_library::macro_associations).

module ActiveModel
  # Raised when forbidden attributes are used for mass assignment.
  class ForbiddenAttributesError < StandardError
  end

  # Raised when unknown attributes are supplied via mass assignment.
  class UnknownAttributeError < NoMethodError
    attr_reader :record, :attribute

    def initialize(record, attribute)
      @record = record
      @attribute = attribute
      super("unknown attribute '#{attribute}' for #{@record.class}.")
    end
  end

  module ForbiddenAttributesProtection # :nodoc:
    private
      def sanitize_for_mass_assignment(attributes)
        if attributes.respond_to?(:permitted?)
          raise ActiveModel::ForbiddenAttributesError if !attributes.permitted?
          attributes.to_h
        else
          attributes
        end
      end
      alias :sanitize_forbidden_attributes :sanitize_for_mass_assignment
  end

  module AttributeAssignment
    include ActiveModel::ForbiddenAttributesProtection

    # Allows you to set all the attributes by passing in a hash of
    # attributes with keys matching the attribute names.
    def assign_attributes(new_attributes)
      unless new_attributes.respond_to?(:each_pair)
        raise ArgumentError, "When assigning attributes, you must pass a hash as an argument, #{new_attributes.class} passed."
      end
      return if new_attributes.empty?

      _assign_attributes(sanitize_for_mass_assignment(new_attributes))
    end

    alias attributes= assign_attributes

    # Invoked when `assign_attributes` is passed an unknown attribute name.
    def attribute_writer_missing(name, value)
      raise UnknownAttributeError.new(self, name)
    end

    private
      def _assign_attributes(attributes)
        attributes.each_pair do |k, v|
          _assign_attribute(k, v)
        end
      end

      def _assign_attribute(k, v)
        setter = :"#{k}="
        public_send(setter, v)
      rescue NoMethodError
        if respond_to?(setter)
          raise
        else
          attribute_writer_missing(k.to_s, v)
        end
      end
  end
end

module ActiveRecord
  UnknownAttributeError = ActiveModel::UnknownAttributeError

  module Core
    def initialize(_attributes = nil)
      init_internals
      initialize_internals_callback
    end

    # Returns +true+ if the record is read only.
    def readonly?
      @readonly == true
    end

    # Prevents records from being written to the database.
    def readonly!
      @readonly = true
    end

    private
      def init_internals
        _runtime_internals
      end

      def initialize_internals_callback
      end
  end

  module Persistence
    # Reloads the record from the database.
    def reload
      _adapter_reload
      self
    end

    def save_after_validation
      if readonly?
        before_save
        _raise_readonly_record_error
      end
      _runtime_save_after_validation
    end

    def destroy
      if readonly?
        before_destroy
        _raise_readonly_record_error
      end
      _runtime_destroy
    end

    def touch
      _raise_readonly_record_error if readonly?
      _runtime_touch
    end

    private
      def _raise_readonly_record_error
        raise ReadOnlyRecord, "#{self.class} is marked as readonly"
      end
  end

  module Reflection
    # The reflection `reflect_on_association` answers for an association
    # the model compiled (reflection.rb's AssociationReflection and its
    # macro subclasses). The generated `reflect_on_association` builds one
    # per association, once per declaring class, and answers the same
    # object after (emit::ruby::library::apply_association_reflections),
    # so a write to it is seen by every later reader, as in Rails:
    # `base.reflect_on_association(:metafields).options[:autosave] = false`.
    #
    # The name, keys and target class come from the compiled association.
    # `options` is the declaration's keyword hash, normalized as
    # MacroReflection#normalize_options does (`counter_cache`), with
    # belongs_to's `required:` turned into `optional:` as the builder does.
    # An option whose value the compiler does not carry (anything but a
    # literal: a lambda, a constant, a `**splat`, a block's extension)
    # makes `options` raise NotImplementedError naming it, rather than
    # answer a hash without it. An association a class macro declares
    # takes the options the macro's `belongs_to` passes when it runs
    # (`_roundhouse_declared`, lower::model_to_library::macro_associations).
    class AssociationReflection
      attr_reader :name, :active_record

      def initialize(name, target, foreign_key, options, active_record, unsupported: [])
        @name = name
        @target = target
        @foreign_key = foreign_key
        @options = options.nil? ? nil : normalize_options(options)
        @active_record = active_record
        @unsupported = unsupported
      end

      def options
        if @options.nil?
          raise NotImplementedError, "roundhouse: #{@active_record}.reflect_on_association(:#{@name}).options: " \
            "the options are known once the class macro declaring the association runs"
        end
        unless @unsupported.empty?
          raise NotImplementedError, "roundhouse: #{@active_record}.reflect_on_association(:#{@name}).options: " \
            "#{@unsupported.join(", ")} not carried (the compiler copies literal option values only)"
        end
        @options
      end

      # The options a class macro's `belongs_to(name, *args, **options)`
      # declares the association with.
      def _roundhouse_declared(options)
        @options = normalize_options(options)
        @unsupported = []
        @class_name = nil
        self
      end

      # Rails' `options[:class_name]` or the name's class; the class the
      # compiler resolved the association to while the options are not
      # known.
      def class_name
        @class_name ||= if _roundhouse_options_known?
          -(@options[:class_name] || derive_class_name).to_s
        else
          -@target.to_s
        end
      end

      def foreign_key
        if @foreign_key.nil?
          raise NotImplementedError, "roundhouse: #{@active_record}.reflect_on_association(:#{@name}).foreign_key: " \
            "the compiled #{macro} association has no foreign key"
        end
        @foreign_key
      end

      def klass
        @klass ||= Object.const_get(@target)
      end

      def autosave=(autosave)
        options[:autosave] = autosave
      end

      def collection?
        false
      end

      def belongs_to?
        false
      end

      def has_one?
        false
      end

      def through_reflection?
        false
      end

      def polymorphic?
        options[:polymorphic]
      end

      def validate?
        !options[:validate].nil? ? options[:validate] : (options[:autosave] == true || collection?)
      end

      private
        def _roundhouse_options_known?
          !@options.nil? && @unsupported.empty?
        end

        def derive_class_name
          class_name = name.to_s
          class_name = class_name.singularize if collection?
          class_name.camelize
        end

        def normalize_options(options)
          counter_cache = options.delete(:counter_cache)

          if counter_cache
            active = true

            case counter_cache
            when String, Symbol
              column = -counter_cache.to_s
            when Hash
              active = counter_cache.fetch(:active, true)
              column = counter_cache[:column]&.to_s
            end

            options[:counter_cache] = { active: active, column: column }
          end

          options
        end
    end

    class HasManyReflection < AssociationReflection
      def macro
        :has_many
      end

      def collection?
        true
      end
    end

    class HasOneReflection < AssociationReflection
      def macro
        :has_one
      end

      def has_one?
        true
      end
    end

    class HasAndBelongsToManyReflection < AssociationReflection
      def macro
        :has_and_belongs_to_many
      end

      def collection?
        true
      end
    end

    # A `belongs_to` association's reflection: its name, foreign key
    # (and type), the target class and primary key. Without options (a
    # class macro's association before the macro runs), `polymorphic?`
    # answers from the foreign type.
    class BelongsToReflection < AssociationReflection
      attr_reader :foreign_type

      def initialize(name, foreign_key, class_name, foreign_type, primary_key, options = nil, active_record = nil, unsupported: [])
        super(name, class_name, foreign_key, options, active_record, unsupported: unsupported)
        @foreign_type = foreign_type
        @primary_key = primary_key
      end

      def macro
        :belongs_to
      end

      def belongs_to?
        true
      end

      # Only a polymorphic declaration has a foreign type.
      def polymorphic?
        _roundhouse_options_known? ? options[:polymorphic] : (true unless @foreign_type.nil?)
      end

      def klass
        raise ArgumentError, "Polymorphic associations do not support computing the class." if polymorphic?
        super
      end

      def association_primary_key
        @primary_key || klass.primary_key.to_s
      end

      private
        def normalize_options(options)
          options = super
          options[:optional] = !options.delete(:required) if options.key?(:required)
          options
        end
    end

    # A `has_many :through` association's reflection: the has_many it
    # wraps answers its name, options and class; the key is the source
    # association's, on the class the through association reaches.
    class ThroughReflection
      attr_reader :delegate_reflection

      def initialize(delegate_reflection)
        @delegate_reflection = delegate_reflection
      end

      def name = @delegate_reflection.name
      def options = @delegate_reflection.options
      def macro = @delegate_reflection.macro
      def active_record = @delegate_reflection.active_record
      def class_name = @delegate_reflection.class_name
      def klass = @delegate_reflection.klass
      def collection? = @delegate_reflection.collection?
      def has_one? = @delegate_reflection.has_one?
      def belongs_to? = false
      def polymorphic? = @delegate_reflection.polymorphic?
      def validate? = @delegate_reflection.validate?
      def through_reflection? = true

      def autosave=(autosave)
        @delegate_reflection.autosave = autosave
      end

      def through_reflection
        active_record.reflect_on_association(options[:through])
      end

      def source_reflection_names
        options[:source] ? [options[:source]] : [name.to_s.singularize, name].collect(&:to_sym).uniq
      end

      def source_reflection
        source_reflection_names.each do |source|
          reflection = through_reflection.klass.reflect_on_association(source)
          return reflection if reflection
        end
        nil
      end

      def foreign_key
        source_reflection.foreign_key
      end

      def foreign_type
        source_reflection.foreign_type
      end
    end
  end

  module AttributeAssignment
    private
      def _assign_attributes(attributes)
        multi_parameter_attributes = nested_parameter_attributes = nil

        attributes.each do |k, v|
          key = k.to_s

          if key.include?("(")
            (multi_parameter_attributes ||= {})[key] = v
          elsif v.is_a?(Hash)
            (nested_parameter_attributes ||= {})[key] = v
          else
            _assign_attribute(key, v)
          end
        end

        assign_nested_parameter_attributes(nested_parameter_attributes) if nested_parameter_attributes
        assign_multiparameter_attributes(multi_parameter_attributes) if multi_parameter_attributes
      end

      # Assign any deferred nested attributes after the base attributes have been set.
      def assign_nested_parameter_attributes(pairs)
        pairs.each { |k, v| _assign_attribute(k, v) }
      end

      def assign_multiparameter_attributes(pairs)
        raise NotImplementedError, "#{self.class.name}: multiparameter attribute assignment " \
          "(#{pairs.keys.join(", ")}) is not ported"
      end
  end

  class Base
    alias_method :_runtime_internals, :initialize
    private :_runtime_internals
    # Ruby warns on removing `initialize` whatever $VERBOSE says, unless
    # it is nil; Core#initialize answers in its place.
    verbose, $VERBOSE = $VERBOSE, nil
    begin
      remove_method :initialize
    ensure
      $VERBOSE = verbose
    end
    remove_method :reload
    # Base's own writes, which Persistence's readonly check calls. The
    # Transactions module (active_record_transactions.rb) may already be
    # prepended, and `alias_method` would name its method.
    [:save_after_validation, :destroy, :touch].each do |name|
      own = instance_method(name)
      own = own.super_method until own.owner == self
      define_method(:"_runtime_#{name}", own)
      private :"_runtime_#{name}"
      remove_method name
    end

    include ActiveModel::AttributeAssignment
    include Core
    include Persistence
    include AttributeAssignment
  end
end
