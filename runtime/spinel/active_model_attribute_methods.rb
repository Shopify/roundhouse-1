# frozen_string_literal: true

# ActiveModel::AttributeMethods, with the ActiveSupport::CodeGenerator it
# defines its methods through, ported from the activemodel and
# activesupport native Core runs (Shopify/rails 5813559f4aed, 8.2.0.alpha:
# activemodel/lib/active_model/attribute_methods.rb,
# activesupport/lib/active_support/code_generator.rb) for the PostgreSQL
# CRuby tree, carried where app code (a gem compiled as source among it)
# names the module. frozen_record 0.27.4's FrozenRecord::Base includes it,
# declares `attribute_method_suffix '?'` and defines each YAML key's
# reader and predicate with `define_attribute_methods` as its records
# load (`undefine_attribute_methods` as they unload). Each method follows
# the native one.
#
# Differences, at the edges:
# - ActiveSupport::Concern is not used: the `included` block is written
#   out as Concern#append_features, and its two class_attributes
#   (`attribute_aliases`, `attribute_method_patterns`, both
#   `instance_writer: false`) as the class reader, writer and predicate
#   and the instance reader and predicate they define: a class-level
#   value a subclass inherits until it assigns its own.
# - `ActiveSupport::Ractors.make_shareable` is `Ractor.make_shareable`,
#   which is what it is on a Ruby that has `Ractor.make_shareable`.

require "concurrent/map"

# ---- activesupport/lib/active_support/code_generator.rb
module ActiveSupport
  class CodeGenerator # :nodoc:
    class MethodSet
      METHOD_CACHES = Hash.new { |h, k| h[k] = Module.new }

      def initialize(namespace)
        @cache = METHOD_CACHES[namespace]
        @sources = []
        @methods = {}
        @canonical_methods = {}
      end

      def define_cached_method(canonical_name, as: nil)
        canonical_name = canonical_name.to_sym
        as = (as || canonical_name).to_sym

        @methods.fetch(as) do
          unless @cache.method_defined?(canonical_name) || @canonical_methods[canonical_name]
            yield @sources
          end
          @canonical_methods[canonical_name] = true
          @methods[as] = canonical_name
        end
      end

      def apply(owner, path, line)
        unless @sources.empty?
          @cache.module_eval("# frozen_string_literal: true\n" + @sources.join(";"), path, line)
        end
        @canonical_methods.clear
        @methods.each do |as, canonical_name|
          owner.define_method(as, @cache.instance_method(canonical_name))
        end
      end
    end

    class << self
      def batch(owner, path, line)
        if owner.is_a?(CodeGenerator)
          yield owner
        else
          instance = new(owner, path, line)
          result = yield instance
          instance.execute
          result
        end
      end
    end

    def initialize(owner, path, line)
      @owner = owner
      @path = path
      @line = line
      @namespaces = Hash.new { |h, k| h[k] = MethodSet.new(k) }
      @sources = []
    end

    def class_eval
      yield @sources
    end

    def define_cached_method(canonical_name, namespace:, as: nil, &block)
      @namespaces[namespace].define_cached_method(canonical_name, as: as, &block)
    end

    def execute
      @namespaces.each_value do |method_set|
        method_set.apply(@owner, @path, @line - 1)
      end

      unless @sources.empty?
        @owner.class_eval("# frozen_string_literal: true\n" + @sources.join(";"), @path, @line - 1)
      end
    end
  end
end

# ---- activemodel/lib/active_model/attribute_methods.rb
module ActiveModel
  # Raised when an attribute is not defined.
  class MissingAttributeError < NoMethodError
  end

  module AttributeMethods
    NAME_COMPILABLE_REGEXP = /\A[a-zA-Z_]\w*[!?=]?\z/
    CALL_COMPILABLE_REGEXP = /\A[a-zA-Z_]\w*[!?]?\z/
    EMPTY_HASH = Hash.new([].freeze).freeze # :nodoc:

    # `included do ... end`, run as Concern#append_features runs it:
    # once, and not again for a class that already has the module.
    #
    #   class_attribute :attribute_aliases, instance_writer: false, default: {}.freeze
    #   @aliases_by_attribute_name = EMPTY_HASH
    #   class_attribute :attribute_method_patterns, instance_writer: false, default: ClassMethods::AttributeMethodPatternSet.new([ ClassMethods::AttributeMethodPattern::BASE ])
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
      base.instance_variable_set(:@__class_attr_attribute_aliases, {}.freeze)
      base.instance_variable_set(:@aliases_by_attribute_name, EMPTY_HASH)
      base.instance_variable_set(
        :@__class_attr_attribute_method_patterns,
        ClassMethods::AttributeMethodPatternSet.new([ ClassMethods::AttributeMethodPattern::BASE ])
      )
    end

    # The class_attributes' instance readers and predicates.
    def attribute_aliases
      defined?(@attribute_aliases) ? @attribute_aliases : self.class.attribute_aliases
    end

    def attribute_aliases?
      !!attribute_aliases
    end

    def attribute_method_patterns
      defined?(@attribute_method_patterns) ? @attribute_method_patterns : self.class.attribute_method_patterns
    end

    def attribute_method_patterns?
      !!attribute_method_patterns
    end

    module ClassMethods
      attr_reader :aliases_by_attribute_name # :nodoc:

      # The class_attribute readers: the value the nearest class that
      # assigned one holds.
      def attribute_aliases
        owner = self
        owner = owner.superclass until owner.instance_variable_defined?(:@__class_attr_attribute_aliases)
        owner.instance_variable_get(:@__class_attr_attribute_aliases)
      end

      def attribute_aliases=(value)
        @__class_attr_attribute_aliases = value
      end

      def attribute_aliases?
        !!attribute_aliases
      end

      def attribute_method_patterns
        owner = self
        owner = owner.superclass until owner.instance_variable_defined?(:@__class_attr_attribute_method_patterns)
        owner.instance_variable_get(:@__class_attr_attribute_method_patterns)
      end

      def attribute_method_patterns=(value)
        @__class_attr_attribute_method_patterns = value
      end

      def attribute_method_patterns?
        !!attribute_method_patterns
      end

      # Declares a method available for all attributes with the given prefix.
      def attribute_method_prefix(*prefixes, parameters: nil)
        prefixes.map! { |prefix| AttributeMethodPattern.new(prefix: prefix, parameters: parameters) }
        self.attribute_method_patterns = (attribute_method_patterns + prefixes).freeze
        undefine_attribute_methods
      end

      # Declares a method available for all attributes with the given suffix.
      def attribute_method_suffix(*suffixes, parameters: nil)
        suffixes.map! { |suffix| AttributeMethodPattern.new(suffix: suffix, parameters: parameters) }
        self.attribute_method_patterns = (attribute_method_patterns + suffixes).freeze
        undefine_attribute_methods
      end

      # Declares a method available for all attributes with the given prefix
      # and suffix.
      def attribute_method_affix(*affixes)
        affixes.map! { |affix| AttributeMethodPattern.new(**affix) }
        self.attribute_method_patterns = (attribute_method_patterns + affixes).freeze
        undefine_attribute_methods
      end

      # Allows you to make aliases for attributes.
      def alias_attribute(new_name, old_name)
        old_name = -old_name.to_s
        new_name = -new_name.to_s
        self.attribute_aliases = attribute_aliases.merge(new_name => old_name).freeze
        record_alias_by_attribute_name(old_name, new_name)
        eagerly_generate_alias_attribute_methods(new_name, old_name)
      end

      def eagerly_generate_alias_attribute_methods(new_name, old_name) # :nodoc:
        ActiveSupport::CodeGenerator.batch(generated_attribute_methods, __FILE__, __LINE__) do |code_generator|
          generate_alias_attribute_methods(code_generator, new_name, old_name)
        end
      end

      def generate_alias_attribute_methods(code_generator, new_name, old_name) # :nodoc:
        ActiveSupport::CodeGenerator.batch(code_generator, __FILE__, __LINE__) do |owner|
          attribute_method_patterns.each do |pattern|
            alias_attribute_method_definition(code_generator, pattern, new_name, old_name)
          end
        end
      end

      def alias_attribute_method_definition(code_generator, pattern, new_name, old_name) # :nodoc:
        method_name = pattern.method_name(new_name).to_s
        target_name = pattern.method_name(old_name).to_s
        parameters = pattern.parameters
        mangled_name = build_mangled_name(target_name)

        call_args = []
        call_args << parameters if parameters

        define_call(code_generator, target_name, mangled_name, parameters, call_args, namespace: :alias_attribute, as: method_name)
      end

      # Is +new_name+ an alias?
      def attribute_alias?(new_name)
        attribute_aliases.key? new_name.to_s
      end

      # Returns the original name for the alias +name+
      def attribute_alias(name)
        attribute_aliases[name.to_s]
      end

      # Declares the attributes that should be prefixed and suffixed by
      # ActiveModel::AttributeMethods.
      def define_attribute_methods(*attr_names)
        ActiveSupport::CodeGenerator.batch(generated_attribute_methods, __FILE__, __LINE__) do |owner|
          attr_names.flatten.each do |attr_name|
            define_attribute_method(attr_name, _owner: owner)
            aliases_by_attribute_name[attr_name.to_s].each do |aliased_name|
              generate_alias_attribute_methods owner, aliased_name, attr_name
            end
          end
        end
      end

      # Declares an attribute that should be prefixed and suffixed by
      # ActiveModel::AttributeMethods.
      def define_attribute_method(attr_name, _owner: generated_attribute_methods, as: attr_name)
        ActiveSupport::CodeGenerator.batch(_owner, __FILE__, __LINE__) do |owner|
          attribute_method_patterns.each do |pattern|
            define_attribute_method_pattern(pattern, attr_name, owner: owner, as: as)
          end
        end
      end

      def define_attribute_method_pattern(pattern, attr_name, owner:, as:, override: false) # :nodoc:
        public_method_name = pattern.method_name(as)

        # If defining a regular attribute method, we don't override methods that are explicitly
        # defined in parent classes.
        if instance_method_already_implemented?(public_method_name)
          # However, for `alias_attribute`, we always define the method.
          # We check for override second because `instance_method_already_implemented?`
          # also check for dangerous methods.
          return unless override
        end

        generate_method = pattern.define_method_proxy_target
        if respond_to?(generate_method, true)
          send(generate_method, attr_name.to_s, owner: owner, as: as)
        else
          canonical_method_name = if as == attr_name
            public_method_name
          else
            pattern.method_name(attr_name)
          end
          define_proxy_call(
            owner,
            canonical_method_name,
            pattern.proxy_target,
            pattern.parameters,
            attr_name.to_s,
            namespace: :active_model_proxy,
            as: public_method_name
          )
        end
      end

      # Removes all the previously dynamically defined methods from the class, including alias attribute methods.
      def undefine_attribute_methods
        @generated_attribute_methods&.module_eval do
          undef_method(*instance_methods)
        end
      end

      private
        def inherited(base) # :nodoc:
          super
          base.class_eval do
            @aliases_by_attribute_name = EMPTY_HASH
            @generated_attribute_methods = nil
          end
        end

        def resolve_attribute_name(name)
          attribute_aliases.fetch(super, &:itself)
        end

        def generated_attribute_methods
          @generated_attribute_methods ||= Module.new.tap { |mod| include mod }
        end

        def instance_method_already_implemented?(method_name)
          @generated_attribute_methods&.method_defined?(method_name)
        end

        # Define a method `name` in `mod` that dispatches to `send`
        # using the given `extra` args. This falls back on `send`
        # if the called name cannot be compiled.
        def define_proxy_call(code_generator, name, proxy_target, parameters, *call_args, namespace:, as: name)
          mangled_name = build_mangled_name(name)

          call_args.map!(&:inspect)
          call_args << parameters if parameters

          # We have to use a different namespace for every target method, because
          # if someone defines an attribute that look like an attribute method we could clash, e.g.
          #   attribute :title_was
          #   attribute :title
          namespace = :"#{namespace}_#{proxy_target}"

          define_call(code_generator, proxy_target, mangled_name, parameters, call_args, namespace: namespace, as: as)
        end

        def build_mangled_name(name)
          mangled_name = name

          unless NAME_COMPILABLE_REGEXP.match?(name)
            mangled_name = :"__temp__#{name.unpack1("h*")}"
          end

          mangled_name
        end

        def define_call(code_generator, target_name, mangled_name, parameters, call_args, namespace:, as:)
          code_generator.define_cached_method(mangled_name, as: as, namespace: namespace) do |batch|
            body = if CALL_COMPILABLE_REGEXP.match?(target_name)
              "self.#{target_name}(#{call_args.join(", ")})"
            else
              call_args.unshift(":'#{target_name}'")
              "send(#{call_args.join(", ")})"
            end

            batch <<
              "def #{mangled_name}(#{parameters || ''})" <<
              body <<
              "end"
          end
        end

        def record_alias_by_attribute_name(old_name, new_name)
          @aliases_by_attribute_name = Ractor.make_shareable(aliases_by_attribute_name.merge(
            old_name => (aliases_by_attribute_name[old_name] | [new_name])
          ))
        end

        class AttributeMethodPattern # :nodoc:
          attr_reader :prefix, :suffix, :define_method_proxy_target, :proxy_target, :parameters

          AttributeMethod = Struct.new(:proxy_target, :attr_name)

          def initialize(prefix: "", suffix: "", parameters: nil)
            @prefix = -prefix
            @suffix = -suffix
            @parameters = parameters.nil? ? "..." : (parameters.is_a?(String) ? -parameters : parameters)
            @prefix_length = @prefix.length
            @affix_length = @prefix.length + @suffix.length
            @unaffixed = @prefix.empty? && @suffix.empty?
            @proxy_target = "#{@prefix}attribute#{@suffix}".freeze
            @define_method_proxy_target = :"define_method_#{@proxy_target}"
            @method_name = "#{prefix}%s#{suffix}".freeze
            freeze
          end

          def matched_attribute_name(method_name)
            return method_name if @unaffixed
            return if method_name.length < @affix_length
            return unless method_name.end_with?(@suffix) && method_name.start_with?(@prefix)
            method_name[@prefix_length, method_name.length - @affix_length]
          end

          def method_for_attr(attr_name)
            AttributeMethod.new(@proxy_target, attr_name)
          end

          def method_name(attr_name)
            @method_name % attr_name
          end

          BASE = new
        end

        class AttributeMethodPatternSet # :nodoc:
          include Enumerable

          def initialize(patterns)
            @patterns = patterns.freeze
            @affixed = patterns.reject { |pattern| pattern.prefix.empty? && pattern.suffix.empty? }.freeze
            @prefixes = @affixed.map(&:prefix).reject(&:empty?).uniq.freeze
            @suffixes = @affixed.map(&:suffix).reject(&:empty?).uniq.freeze
            freeze
          end

          def each(&block)
            @patterns.each(&block)
          end

          def +(other)
            AttributeMethodPatternSet.new(@patterns + other)
          end

          def ==(other)
            other.is_a?(AttributeMethodPatternSet) && patterns == other.patterns
          end

          def matching(method_name)
            return if @affixed.empty?

            @affixed if method_name.end_with?(*@suffixes) || method_name.start_with?(*@prefixes)
          end

          protected
            attr_reader :patterns
        end
    end

    # Allows access to the object attributes, which are held in the hash
    # returned by <tt>attributes</tt>, as though they were first-class
    # methods.
    def method_missing(method, ...)
      if respond_to_without_attributes?(method, true)
        super
      else
        match = matched_attribute_method(method.name)
        match ? attribute_missing(match, ...) : super
      end
    end

    # +attribute_missing+ is like +method_missing+, but for attributes. When
    # +method_missing+ is called we check to see if there is a matching
    # attribute method. If so, we tell +attribute_missing+ to dispatch the
    # attribute.
    def attribute_missing(match, ...)
      __send__(match.proxy_target, match.attr_name, ...)
    end

    # A +Person+ instance with a +name+ attribute can ask
    # <tt>person.respond_to?(:name)</tt>, <tt>person.respond_to?(:name=)</tt>,
    # and <tt>person.respond_to?(:name?)</tt> which will all return +true+.
    define_method(:respond_to_without_attributes?, Kernel.instance_method(:respond_to?))
    def respond_to?(method, include_private_methods = false)
      if super
        true
      elsif !include_private_methods && super(method, true)
        # If we're here then we haven't found among non-private methods
        # but found among all methods. Which means that the given method is private.
        false
      else
        !matched_attribute_method(method.to_s).nil?
      end
    end

    private
      def attribute_method?(attr_name)
        respond_to_without_attributes?(:attributes) && attributes.include?(attr_name)
      end

      # Returns a struct representing the matching attribute method.
      # The struct's attributes are prefix, base and suffix.
      def matched_attribute_method(method_name)
        pattern = ClassMethods::AttributeMethodPattern::BASE
        attr_name = pattern.matched_attribute_name(method_name)
        return pattern.method_for_attr(attr_name) if attr_name && attribute_method?(attr_name)

        affixed = self.class.attribute_method_patterns.matching(method_name)
        return unless affixed

        index = 0
        len = affixed.length
        while index < len
          pattern = affixed[index]
          attr_name = pattern.matched_attribute_name(method_name)
          if attr_name && attribute_method?(attr_name)
            return pattern.method_for_attr(attr_name)
          end
          index += 1
        end
        nil
      end

      def missing_attribute(attr_name, stack)
        raise ActiveModel::MissingAttributeError, "missing attribute '#{attr_name}' for #{self.class}", stack
      end

      def _read_attribute(attr)
        __send__(attr)
      end

      module AttrNames # :nodoc:
        DEF_SAFE_NAME = /\A[a-zA-Z_]\w*\z/

        # We want to generate the methods via module_eval rather than
        # define_method, because define_method is slower on dispatch.
        def self.define_attribute_accessor_method(attr_name, writer: false)
          method_name = "#{attr_name}#{'=' if writer}"
          if attr_name.ascii_only? && DEF_SAFE_NAME.match?(attr_name)
            yield method_name, "'#{attr_name}'"
          else
            safe_name = attr_name.unpack1("h*")
            const_name = "ATTR_#{safe_name}"
            const_set(const_name, attr_name) unless const_defined?(const_name)
            temp_method_name = "__temp__#{safe_name}#{'=' if writer}"
            attr_name_expr = "::ActiveModel::AttributeMethods::AttrNames::#{const_name}"
            yield temp_method_name, attr_name_expr
          end
        end
      end
  end
end
