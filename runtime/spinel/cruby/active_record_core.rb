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
# - `reload` reads `lock:` only (a locking SELECT of the key, then the
#   re-read through the runtime's `_adapter_reload`).
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
#
# And Core#init_with / #init_with_attributes, the class-side
# `attributes_builder` (model_schema.rb) with the attribute set it builds
# from database values (activemodel attribute_set/builder.rb's Builder
# and LazyAttributeSet), Dirty's `clear_changes_information` and
# Core#strict_loading!. Shopify Core's DisconnectedFixtureSet.new_record
# calls `klass.allocate.init_with("attributes" => values)`, and Core's
# ActiveRecordAttributesCoercion turns the Hash into that set first.
# Differences:
# - `init_with_attributes` builds the record from the set's database
#   values through the model's compiled `instantiate(row)` (from_row,
#   mark_persisted!, the find and initialize hooks) and takes that
#   record's state, since the runtime's records keep their columns in
#   compiled slots, not in an attribute set. A hook that captures the
#   record's identity sees the record it ran on.
# - `init_with` takes an attribute set; a coder whose "attributes" is a
#   Hash (Rails stores it as @attributes and fails on the first read)
#   raises ArgumentError, and so does a YAML coder's "concise_attributes".
# - `strict_loading!` records the flag and mode as Rails does, but the
#   compiled association readers do not raise StrictLoadingViolationError.

module ActiveModel
  class AttributeSet # :nodoc:
    class Builder # :nodoc:
      attr_reader :types, :default_attributes

      def initialize(types, default_attributes = {})
        @types = types
        @default_attributes = default_attributes
      end

      def build_from_database(values = {}, additional_types = {})
        LazyAttributeSet.new(values, types, additional_types, default_attributes)
      end
    end
  end

  class LazyAttributeSet < AttributeSet # :nodoc:
    def initialize(values, types, additional_types, default_attributes, attributes = {})
      @attributes = attributes
      @values = values
      @types = types
      @additional_types = additional_types
      @default_attributes = default_attributes
      @casted_values = {}
    end

    def [](name)
      @attributes[name] || default_attribute(name)
    end

    def key?(name)
      (values.key?(name) || types.key?(name) || @attributes.key?(name)) && self[name].initialized?
    end

    def keys
      keys = values.keys | types.keys | @attributes.keys
      keys.keep_if { |name| self[name].initialized? }
    end

    def fetch_value(name, &block)
      if attr = @attributes[name]
        return attr.value(&block)
      end

      @casted_values.fetch(name) do
        value_present = true
        value = values.fetch(name) { value_present = false }

        if value_present
          type = additional_types.fetch(name, types[name])
          @casted_values[name] = type.deserialize(value)
        else
          attr = default_attribute(name, value_present, value)
          attr.value(&block)
        end
      end
    end

    # AttributeSet#values_before_type_cast, over the keys the set has.
    def values_before_type_cast
      keys.to_h { |name| [name, self[name].value_before_type_cast] }
    end

    private
      attr_reader :values, :types, :additional_types, :default_attributes

      def default_attribute(
        name,
        value_present = true,
        value = values.fetch(name) { value_present = false }
      )
        type = additional_types.fetch(name, types[name])

        if value_present
          @attributes[name] = Attribute.from_database(name, value, type, @casted_values[name])
        elsif types.key?(name)
          if attr = default_attributes[name]
            @attributes[name] = attr.dup
          else
            @attributes[name] = Attribute.uninitialized(name, type)
          end
        else
          Attribute.null(name)
        end
      end
  end

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

  module LegacyYamlAdapter # :nodoc:
    def self.convert(coder)
      return coder unless defined?(Psych::Coder) && coder.is_a?(Psych::Coder)

      case coder["active_record_yaml_version"]
      when 1, 2 then coder
      else
        raise("Active Record doesn't know how to load YAML with this format.")
      end
    end
  end

  module Core
    def initialize(_attributes = nil)
      init_internals
      initialize_internals_callback
    end

    # The instance reader of core.rb's `cattr_accessor :logger`
    # (ActiveRecord::Base.logger above).
    def logger
      ::ActiveRecord::Base.logger
    end

    # Returns +true+ if the record is read only.
    def readonly?
      @readonly == true
    end

    # Prevents records from being written to the database.
    def readonly!
      @readonly = true
    end

    # The has_many reflection whose `dependent: :destroy` is destroying
    # this record, nil otherwise (Rails' AutosaveAssociation).
    def destroyed_by_association
      @destroyed_by_association
    end

    def destroyed_by_association=(reflection)
      @destroyed_by_association = reflection
    end

    # Initialize an empty model object from +coder+.
    def init_with(coder, &block)
      coder = LegacyYamlAdapter.convert(coder)
      unless coder["attributes"]
        raise ArgumentError, "roundhouse: #{self.class}#init_with: a YAML coder's concise_attributes are not ported"
      end
      init_with_attributes(coder["attributes"], coder["new_record"], &block)
    end

    # Initialize an empty model object from +attributes+, an attribute set
    # of database values (`attributes_builder.build_from_database`).
    def init_with_attributes(attributes, new_record = false) # :nodoc:
      unless attributes.is_a?(ActiveModel::LazyAttributeSet)
        raise ArgumentError, "roundhouse: #{self.class}#init_with_attributes takes the attribute set " \
          "`attributes_builder.build_from_database` builds, not #{attributes.class}"
      end
      record = self.class.instantiate(attributes.values_before_type_cast)
      record.instance_variables.each { |ivar| instance_variable_set(ivar, record.instance_variable_get(ivar)) }
      @persisted = false if new_record

      yield self if block_given?

      self
    end

    # Rails' `initialize_dup`: `dup` answers a NEW record with the
    # attributes copied and the primary key and the timestamps cleared
    # (`@attributes.reset(@primary_key)`, `clear_timestamp_attributes`),
    # not destroyed, with no loaded associations, errors or saved
    # changes of the original, and the initialize callbacks run.
    def initialize_dup(other)
      super
      @persisted = false
      @new_record = nil
      @destroyed = false
      @readonly = false
      @errors = _fresh_errors
      @error_details = []
      @saved_changes = nil
      @id_previously_changed = false
      @__last_saved_attributes = nil
      @__forced_changes = nil
      @_new_record_before_last_commit = false
      @_trigger_update_callback = false
      @_trigger_destroy_callback = false
      @_committed_already_called = false
      @destroyed_by_association = nil
      instance_variables.each do |ivar|
        instance_variable_set(ivar, false) if ivar.end_with?("_loaded") && instance_variable_get(ivar) == true
      end
      # A composite key (`primary_key = [:shop_id, :id]`) clears every
      # column, as Rails' `@primary_key.each { |key| @attributes.reset(key) }`.
      Array(self.class.primary_key).each { |key| self[key] = nil }
      columns = self.class.schema_columns
      [:created_at, :created_on, :updated_at, :updated_on].each do |column|
        self[column] = nil if columns.include?(column)
      end
      after_initialize
    end

    # Clears all dirty data: current changes and previous changes.
    def clear_changes_information
      @__last_saved_attributes = _attribute_snapshot
      @saved_changes = nil
      @id_previously_changed = false
      nil
    end

    # Sets the record to strict_loading mode.
    def strict_loading!(value = true, mode: :all)
      unless [:all, :n_plus_one_only].include?(mode)
        raise ArgumentError, "The :mode option must be one of [:all, :n_plus_one_only] but #{mode.inspect} was provided."
      end

      @strict_loading_mode = mode
      @strict_loading = value
    end

    attr_reader :strict_loading_mode

    # Returns +true+ if the record is in strict_loading mode.
    def strict_loading?
      @strict_loading
    end

    # Returns +true+ if the record uses strict_loading with +:n_plus_one_only+ mode enabled.
    def strict_loading_n_plus_one_only?
      @strict_loading_mode == :n_plus_one_only
    end

    # Returns +true+ if the record uses strict_loading with +:all+ mode enabled.
    def strict_loading_all?
      @strict_loading_mode == :all
    end

    private
      def init_internals
        _runtime_internals
      end

      def initialize_internals_callback
      end
  end

  module Persistence
    # Reloads the record from the database (persistence.rb `reload(options = nil)`).
    # `lock:` first takes the row lock a `lock(value).find(id)` takes, as
    # `lock!` and Core's `reload(lock: true)` ask for; no other option is read.
    def reload(options = nil)
      lock = options && options[:lock]
      if lock && persisted?
        self.class.unscoped.lock(lock).where(self.class.primary_key => id).ids
      end
      _adapter_reload
      # ActiveRecord::AttributeMethods::Dirty#reload: the row just read is
      # the baseline, and the last save's changes are forgotten.
      clear_changes_information
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

    # Rails' `touch(*names, time: nil)`: with names or a time, those
    # attributes (and the update timestamps) are written `time`.
    def touch(*names, time: nil)
      _raise_readonly_record_error if readonly?
      return _runtime_touch if names.empty? && time.nil?
      _runtime_touch_named(names, time)
    end

    def touch_written
      _raise_readonly_record_error if readonly?
      _runtime_touch_written
    end

    private
      def _raise_readonly_record_error
        raise ReadOnlyRecord, "#{self.class} is marked as readonly"
      end
  end

  module Reflection
    # reflection.rb `compute_type`: a class name written without a
    # leading `::` binds relative to the owner's namespace, innermost
    # first (`class_name: "Address"` on Shipping::Label is
    # Shipping::Label::Address, Shipping::Address, then ::Address); a
    # candidate counts only when it is that very constant, not one the
    # lookup inherited from Object.
    def self.compute_type(owner, type_name)
      type_name = type_name.to_s
      return Object.const_get(type_name.delete_prefix("::")) if type_name.start_with?("::")
      candidates = []
      owner.to_s.scan(/::|$/) { candidates.unshift "#{$`}::#{type_name}" } unless owner.to_s.empty?
      candidates << type_name
      candidates.each do |candidate|
        constant = begin
          Object.const_get(candidate)
        rescue NameError => e
          raise unless e.name.to_s == candidate || candidate.split("::").include?(e.name.to_s)
          next
        end
        return constant if candidate == constant.to_s
      end
      raise NameError.new("uninitialized constant #{candidates.first}", candidates.first)
    end

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

      def initialize(name, target, foreign_key, options, active_record, unsupported: [], instance_dependent_scope: false, scoped: false, key_pairs: nil)
        @name = name
        @key_pairs = key_pairs
        @target = target
        @foreign_key = foreign_key
        @options = options.nil? ? nil : normalize_options(options)
        @active_record = active_record
        @unsupported = unsupported
        @instance_dependent_scope = instance_dependent_scope
        @scoped = scoped || instance_dependent_scope
      end

      # A composite key's `[foreign key side column, referenced side column]`
      # pairs as the compiler resolved them (Core's `yugabyte_overrides`
      # replace the plain options), the writable pair first; nil for a
      # single-column key. The target's column for a has_one / has_many,
      # the owner's for a belongs_to.
      def _roundhouse_key_pairs
        @key_pairs
      end

      # reflection.rb's `query_foreign_key`: the owner's columns the association
      # is read through, every part of a composite key (a belongs_to's foreign
      # key columns, a has_one / has_many's owner-side columns), else the
      # one foreign key (belongs_to) or primary key. Query layers select
      # these so a partially loaded owner can still find its target.
      def query_foreign_key
        if @key_pairs
          return macro == :belongs_to ? @key_pairs.map(&:first) : @key_pairs.map(&:last)
        end
        macro == :belongs_to ? foreign_key : active_record_primary_key
      end

      # Whether the declaration has a scope lambda (Rails' `scope`, which
      # an automatic inverse refuses); the lambda itself is not carried.
      def _roundhouse_scoped?
        @scoped
      end

      # reflection.rb's `inverse_name`: the `inverse_of:` option when the
      # declaration names one (`false` opts out), else the automatic
      # inverse Rails finds on the target by the owner's name.
      def inverse_name
        unless defined?(@inverse_name)
          @inverse_name = options.fetch(:inverse_of) { automatic_inverse_of }
        end
        @inverse_name
      end
      private :inverse_name

      def has_inverse?
        inverse_name
      end

      # reflection.rb's `inverse_of`: the target class's reflection named
      # by `inverse_name`, nil without one.
      def inverse_of
        return nil unless inverse_name
        @inverse_of ||= klass.reflect_on_association(inverse_name)
      end

      # reflection.rb `check_eager_loadable!`: a scope that takes the
      # owner (`->(owner) { where(...) }`) cannot be joined or eager
      # loaded, as there is no owner to hand it.
      def check_eager_loadable!
        return unless @instance_dependent_scope
        raise ArgumentError, "The association scope '#{@name}' is instance dependent (the scope " \
          "block takes an argument). Eager loading instance dependent scopes is not supported."
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
        @klass = nil
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

      # reflection.rb's `foreign_key`: the `:foreign_key` option as the
      # declaration gave it once it is known (a class macro's association
      # may pass an Array of columns, Rails' composite foreign key,
      # while the compiled association reads one column), else the
      # column the compiler resolved.
      def foreign_key
        if _roundhouse_options_known? && (declared = @options[:foreign_key])
          return _roundhouse_key_names(declared)
        end
        if @foreign_key.nil?
          raise NotImplementedError, "roundhouse: #{@active_record}.reflect_on_association(:#{@name}).foreign_key: " \
            "the compiled #{macro} association has no foreign key"
        end
        @foreign_key
      end

      # A declared `class_name:` (`"::Country"` written under a namespace
      # that has its own `Country`) wins over the name the compiler
      # derived from the association's name.
      def klass
        @klass ||= begin
          declared = _roundhouse_options_known? ? @options[:class_name] : nil
          Reflection.compute_type(@active_record&.name, declared.nil? ? @target : declared)
        end
      end

      # reflection.rb's `active_record_primary_key`: the owner's key the
      # association is joined from: the `:primary_key` option, else the
      # owner's query constraints, else its primary key (a composite
      # primary key shaped `[<tenant>, "id"]` infers "id").
      def active_record_primary_key
        @active_record_primary_key ||= begin
          custom = options[:primary_key]
          if custom
            custom.is_a?(Array) ? custom.map { |key| -key.to_s }.freeze : -custom.to_s
          elsif active_record.has_query_constraints? || options[:query_constraints]
            active_record.query_constraints_list
          elsif active_record.composite_primary_key?
            key = _roundhouse_primary_key_of(active_record)
            key.include?("id") ? "id" : key.freeze
          else
            _roundhouse_primary_key_of(active_record).freeze
          end
        end
      end

      # reflection.rb's `type`: the polymorphic type column of an `as:`
      # association (`foreign_type` option, else `"<as>_type"`), nil
      # otherwise.
      def type
        @type ||= (-(options[:foreign_type]&.to_s || "#{options[:as]}_type") if options[:as])
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

      # Rails' AssociationReflection#through_reflection: nil, as only a
      # ThroughReflection reaches through another association (Core's
      # DisconnectedFixtureSet.collection_fixture_names branches on it).
      def through_reflection
        nil
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

        VALID_AUTOMATIC_INVERSE_MACROS = [:has_many, :has_one, :belongs_to].freeze
        INVALID_AUTOMATIC_INVERSE_OPTIONS = [:through, :foreign_key].freeze

        # reflection.rb's `automatic_inverse_of`: the target's association
        # named after the owner (`Order` -> `:order`; the plural too when
        # the owner class says it inverts those), when it takes this
        # association's foreign key and neither side forbids it.
        def automatic_inverse_of
          return nil unless can_find_inverse_of_automatically?(self)
          inverse_name = ActiveSupport::Inflector.underscore(options[:as] || active_record.name.demodulize).to_sym
          reflection = klass.reflect_on_association(inverse_name)
          if reflection.nil? && active_record.respond_to?(:automatically_invert_plural_associations) &&
              active_record.automatically_invert_plural_associations
            reflection = klass.reflect_on_association(ActiveSupport::Inflector.pluralize(inverse_name))
          end
          inverse_name if valid_inverse_reflection?(reflection)
        end

        def valid_inverse_reflection?(reflection)
          !reflection.nil? &&
            !reflection.equal?(self) &&
            foreign_key == reflection.foreign_key &&
            klass <= reflection.active_record &&
            can_find_inverse_of_automatically?(reflection, true)
        end

        def can_find_inverse_of_automatically?(reflection, inverse_reflection = false)
          reflection.options[:inverse_of] != false &&
            VALID_AUTOMATIC_INVERSE_MACROS.include?(reflection.macro) &&
            INVALID_AUTOMATIC_INVERSE_OPTIONS.none? { |opt| reflection.options[opt] } &&
            scope_allows_automatic_inverse_of?(reflection, inverse_reflection)
        end

        def scope_allows_automatic_inverse_of?(reflection, inverse_reflection)
          if inverse_reflection
            !reflection._roundhouse_scoped?
          else
            !reflection._roundhouse_scoped? ||
              (reflection.klass.respond_to?(:automatic_scope_inversing) && reflection.klass.automatic_scope_inversing)
          end
        end

        # A `:foreign_key` / `:primary_key` option as reflection.rb
        # normalizes it: a frozen String, or an Array of them.
        def _roundhouse_key_names(key)
          key.is_a?(Array) ? key.map { |name| -name.to_s }.freeze : -key.to_s
        end

        # reflection.rb's `primary_key(klass)`.
        def _roundhouse_primary_key_of(klass)
          key = klass.primary_key
          raise UnknownPrimaryKey.new(klass) if key.nil?
          key
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

      def association_class
        ActiveRecord::Associations::HasManyAssociation
      end

      def collection?
        true
      end
    end

    class HasOneReflection < AssociationReflection
      def macro
        :has_one
      end

      def association_class
        ActiveRecord::Associations::HasOneAssociation
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

      def initialize(name, foreign_key, class_name, foreign_type, primary_key, options = nil, active_record = nil, unsupported: [], scoped: false, key_pairs: nil)
        super(name, class_name, foreign_key, options, active_record, unsupported: unsupported, scoped: scoped, key_pairs: key_pairs)
        @foreign_type = foreign_type
        @primary_key = primary_key
      end

      def macro
        :belongs_to
      end

      # reflection.rb's `association_class`.
      def association_class
        polymorphic? ? ActiveRecord::Associations::BelongsToPolymorphicAssociation : ActiveRecord::Associations::BelongsToAssociation
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

      # reflection.rb's `association_primary_key`: the `:primary_key`
      # option once the declaration's options are known (an Array for a
      # composite key), else the key the compiler resolved, else the
      # target's primary key.
      def association_primary_key
        if _roundhouse_options_known? && (declared = @options[:primary_key])
          return _roundhouse_key_names(declared)
        end
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
      def inverse_of = @delegate_reflection.inverse_of
      def _roundhouse_scoped? = @delegate_reflection._roundhouse_scoped?
      def macro = @delegate_reflection.macro
      def active_record = @delegate_reflection.active_record
      def class_name = @delegate_reflection.class_name
      def klass = @delegate_reflection.klass
      def active_record_primary_key = @delegate_reflection.active_record_primary_key
      def type = @delegate_reflection.type
      def collection? = @delegate_reflection.collection?
      def has_one? = @delegate_reflection.has_one?
      def belongs_to? = false
      def polymorphic? = @delegate_reflection.polymorphic?
      def validate? = @delegate_reflection.validate?
      def through_reflection? = true
      def check_eager_loadable! = @delegate_reflection.check_eager_loadable!

      def autosave=(autosave)
        @delegate_reflection.autosave = autosave
      end

      # A class macro's `has_many :name, through:` declares the wrapped
      # has_many's options (AssociationReflection#_roundhouse_declared).
      def _roundhouse_declared(options)
        @delegate_reflection._roundhouse_declared(options)
        self
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

  module Associations
    # Rails' Association#reflection (`attr_reader :reflection`): the
    # reflection of the association the object stands for, here the one
    # the owner's class answers for its name (`reflect_on_association`,
    # emit::ruby::library::apply_association_reflections). Core's
    # Customers::CustomerActiveRecord.has_many_association? asks
    # `association.reflection.through_reflection?`.
    class Association
      def reflection
        klass = @owner.class
        unless klass.respond_to?(:reflect_on_association)
          raise NotImplementedError, "roundhouse: #{klass.name}#association(:#{@name}).reflection: " \
            "#{klass.name} answers no reflections (reflect_on_association is emitted where the app calls it)"
        end
        reflection = klass.reflect_on_association(@name)
        if reflection.nil?
          raise NotImplementedError, "roundhouse: #{klass.name}#association(:#{@name}).reflection: " \
            "#{klass.name}.reflect_on_association(:#{@name}) answers no reflection"
        end
        reflection
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
    [:save_after_validation, :destroy, :touch, :touch_written].each do |name|
      own = instance_method(name)
      own = own.super_method until own.owner == self
      define_method(:"_runtime_#{name}", own)
      private :"_runtime_#{name}"
      remove_method name
    end

    # model_schema.rb's `attributes_builder`: the attribute set builder
    # for the class's attribute types. Rails passes the defaults of the
    # attributes that are not columns, and the primary key's; the runtime
    # keeps no default attribute set, so a name the values leave out reads
    # as uninitialized.
    def self.attributes_builder # :nodoc:
      @attributes_builder ||= ActiveModel::AttributeSet::Builder.new(attribute_types, {})
    end

    # The hydration sentinel (`HYDRATE_ATTRS`, frozen so a write through
    # the shared object raises) answers `delete` as the empty Hash a
    # bare `{}` default would. A model's own `initialize(*attrs)` takes
    # its option out of the first argument (Core's
    # ThemeJsonTemplateUpdateDeferredAction:
    # `@theme = attrs[0]&.delete(:theme)`), and hydration builds the
    # record through `new` where Rails' `instantiate` skips `initialize`:
    # deleting from an empty Hash changes nothing, but a frozen one
    # raised FrozenError. Every other write still raises. Rebuilt here,
    # not in base.rb, since a frozen object takes no singleton methods.
    hydrate_attrs = Hash.new
    hydrate_attrs.extend(Module.new do
      def delete(key)
        block_given? ? yield(key) : nil
      end
    end)
    remove_const(:HYDRATE_ATTRS)
    const_set(:HYDRATE_ATTRS, hydrate_attrs.freeze)

    class << self
      # core.rb's `cattr_accessor :logger, instance_writer: false`: one
      # logger every model class reads off ActiveRecord::Base, which
      # railties' "active_record.logger" initializer fills with
      # `self.logger ||= ::Rails.logger`. The emitted boot runs no railtie
      # initializers, so the first read stands in for that one, as the
      # LogSubscriber's does: it takes Rails.logger unless a logger was
      # assigned (nil included).
      def logger
        base = ::ActiveRecord::Base
        unless base.instance_variable_defined?(:@__roundhouse_logger)
          base.logger = ::Rails.logger if defined?(::Rails) && ::Rails.respond_to?(:logger)
        end
        base.instance_variable_get(:@__roundhouse_logger)
      end

      def logger=(logger)
        ::ActiveRecord::Base.instance_variable_set(:@__roundhouse_logger, logger)
      end

      # inheritance.rb's `new`: a record built with the inheritance
      # column's value (`Condition.new(attribute_name: "title")`, which
      # `create!` and an association's `build` reach) is an instance of
      # the class `find_sti_class` names for it, as a loaded row is.
      # Differences: a scope's `scope_for_create` and the column defaults
      # (Rails' two fallbacks when the attributes name no class) are not
      # consulted.
      # Written over `*args`: a bare `new` (the compiled hydration's) must
      # reach `initialize` with no argument, not a nil one.
      def new(*args, &block)
        subclass = subclass_from_attributes(args.first)
        if subclass && !subclass.equal?(self)
          subclass.new(*args, &block)
        else
          super
        end
      end
      ruby2_keywords :new

      # inheritance.rb's `subclass_from_attributes`.
      def subclass_from_attributes(attributes) # :nodoc:
        attributes = attributes.to_h if attributes.respond_to?(:permitted?)
        return nil unless attributes.is_a?(Hash)
        column = inheritance_column.to_s
        return nil if column.empty?
        type_name = attributes[column] || attributes[column.to_sym]
        return nil if type_name.nil? || (type_name.respond_to?(:empty?) && type_name.empty?)
        return nil unless schema_columns.include?(column.to_sym)
        find_sti_class(type_name)
      end
      private :subclass_from_attributes

      # model_schema.rb's `quoted_table_name` and `quoted_primary_key`:
      # the table and the primary key column as the connection quotes
      # identifiers (a table name may be schema-qualified).
      def quoted_table_name
        connection.quote_table_name(table_name)
      end

      def quoted_primary_key
        connection.quote_column_name(primary_key)
      end

      # core.rb: `class_attribute :belongs_to_required_by_default,
      # instance_accessor: false`, true under `load_defaults` >= 5.0
      # (Core's 8.1). Reading walks to the nearest class that assigned
      # one. The compiled `belongs_to` declarations already applied a
      # class body's or an `extended` hook's write
      # (lower::model_to_library::macro_replay); a class macro that calls
      # `belongs_to` at run time (Core's belongs_to_with_query_constraints)
      # reads it here.
      def belongs_to_required_by_default
        klass = self
        while klass
          if klass.instance_variable_defined?(:@belongs_to_required_by_default)
            return klass.instance_variable_get(:@belongs_to_required_by_default)
          end
          klass = klass.superclass
        end
        true
      end

      # locking/optimistic.rb `class_attribute :lock_optimistically, default:
      # true` and `locking_column` (default "lock_version"): the class's own
      # value once assigned, else its superclass's.
      def lock_optimistically
        return @lock_optimistically if @__lock_optimistically_assigned
        return true if equal?(ActiveRecord::Base)
        superclass.lock_optimistically
      end

      def lock_optimistically=(value)
        @__lock_optimistically_assigned = true
        @lock_optimistically = value
      end

      def locking_column
        return @locking_column if @__locking_column_assigned
        return "lock_version" if equal?(ActiveRecord::Base)
        superclass.locking_column
      end

      def locking_column=(value)
        @__locking_column_assigned = true
        @locking_column = value.to_s
      end

      # Optimistic#locking_enabled?: `lock_optimistically && columns_hash
      # .key?(locking_column)`, so a table without the column is unlocked.
      def locking_enabled?
        lock_optimistically && !abstract_class? && schema_columns.include?(locking_column.to_sym) ? true : false
      end

      def belongs_to_required_by_default=(value)
        @belongs_to_required_by_default = value
      end

      def belongs_to_required_by_default?
        !!belongs_to_required_by_default
      end
    end

    def locking_enabled?
      self.class.locking_enabled?
    end

    include ActiveModel::AttributeAssignment
    include Core
    include Persistence
    include AttributeAssignment
  end
end
