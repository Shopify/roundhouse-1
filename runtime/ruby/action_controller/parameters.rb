# ActionController::Parameters, ported from actionpack 8.2.0.alpha
# (Shopify/rails 5813559f4aed, the copy Core pins:
# actionpack/lib/action_controller/metal/strong_parameters.rb), with the
# errors that file defines beside it (UnfilteredParameters,
# InvalidParameterKey, ExpectedParameterMissing, and
# UnpermittedParameters' constructor and `params`).
#
# A request's params do not reach the emitted program through this
# class: `lower::params_permit` lowers `permit` / `require` on a
# controller's `params` to String-keyed hashes. This is the class app
# code names directly. Core builds `ActionController::Parameters.new(hash)`
# outside a request (BackgroundQueue's parameter validation, order
# updates, GraphQL cursors), tests `is_a?(ActionController::Parameters)`,
# and lists the class in VersionedSerializer coders, whose `constantize`
# runs at class-body time. So the constant has to be the real class.
#
# Method for method after the Rails source. The differences are at the
# edges:
# - The store is a String-keyed Hash, not a HashWithIndifferentAccess
#   (only the CRuby tree carries one). Every key a caller passes goes
#   through `convert_key`, so a Symbol reads as its name, as the
#   indifferent hash converts it. A nested Hash is copied with its
#   Symbol keys as names on the way in, which is the indifferent hash's
#   `convert_value`. Reads, `inspect`, equality and the `to_h` family
#   therefore answer as Rails does. One case differs: a top-level key
#   that is neither a String nor a Symbol, written through `[]=` /
#   `merge` / `transform_keys`, is stored as its `to_s`, where Rails
#   keeps the object.
# - `to_h` / `to_unsafe_h` answer a HashWithIndifferentAccess where the
#   tree defines one (the CRuby tree's core_ext port), as Rails does.
#   On a tree without one they answer the same String-keyed Hash.
# - The class settings (`permit_all_parameters`,
#   `action_on_unpermitted_parameters`, `always_permitted_parameters`)
#   live on Parameters, and its subclasses share them. In Rails the
#   first two are class_attributes, which a subclass could override.
#   When unset, `action_on_unpermitted_parameters` answers what Rails'
#   railtie (action_controller.parameters_config) sets: `:log` in a
#   local (development or test) environment, else false.
# - `:log` instruments `unpermitted_parameters.action_controller` where
#   the tree carries ActiveSupport::Notifications. Action Controller's
#   log subscriber, which prints that event, is not ported.
# - Rails' protected readers (`parameters`, `permitted=`,
#   `nested_attributes?`, `permit_filters`) are public here.
# - Not ported, each raising NotImplementedError by name: the YAML load
#   tags Rails registers at load (`hook_into_yaml_loading`; the dump and
#   revive hooks `encode_with` / `init_with` are ported, and Psych's
#   `!ruby/object:` form needs no tag), `deep_transform_keys(!)`,
#   `deep_transform_values(!)` and `deep_merge(!)`.
#
# Ruby calls some of these methods with no call site in the tree:
# Psych's dump and load call `encode_with` and `init_with`, and a
# `case ... in {key:}` pattern calls `deconstruct_keys`. The text shake
# (emit::ruby::shake) keeps a def only when its name appears elsewhere,
# so the names `encode_with`, `init_with` and `deconstruct_keys` in this
# comment count as their uses.
require_relative "../rails"
require_relative "parameter_missing"

module ActionController
  # Raised when a Parameters instance that is not permitted is converted
  # to a hash (`to_h`, `to_hash`).
  class UnfilteredParameters < ArgumentError
    def initialize
      super("unable to convert unpermitted parameters to hash")
    end
  end

  # Raised when Parameters is built from a hash whose keys are not all
  # Strings or Symbols.
  class InvalidParameterKey < ArgumentError
  end

  # Raised from `expect!` when an expected parameter is missing or of an
  # incompatible type.
  class ExpectedParameterMissing < ParameterMissing
  end

  # parameter_missing.rb declares the class for every Ruby-family tree;
  # strong_parameters.rb gives it its constructor and message.
  class UnpermittedParameters < StandardError
    def initialize(params)
      @params = params
      plural = params.size > 1 ? "s" : ""
      names = params.map { |e| ":#{e}" }.join(", ")
      super("found unpermitted parameter#{plural}: #{names}")
    end

    def params
      @params
    end
  end

  class Parameters
    # class_attribute :permit_all_parameters, default: false
    def self.permit_all_parameters
      @permit_all_parameters == true
    end

    def self.permit_all_parameters=(value)
      @permit_all_parameters = value
    end

    # class_attribute :action_on_unpermitted_parameters, which the
    # railtie sets when the app does not configure it.
    def self.action_on_unpermitted_parameters
      return @action_on_unpermitted_parameters if @action_on_unpermitted_parameters_set == true
      Rails.env.local? ? :log : false
    end

    def self.action_on_unpermitted_parameters=(value)
      @action_on_unpermitted_parameters_set = true
      @action_on_unpermitted_parameters = value
    end

    # cattr_accessor :always_permitted_parameters, default: %w( controller action )
    def self.always_permitted_parameters
      list = @always_permitted_parameters
      return list unless list.nil?
      list = ["controller", "action"]
      @always_permitted_parameters = list
      list
    end

    def self.always_permitted_parameters=(value)
      @always_permitted_parameters = value
    end

    def self.nested_attribute?(key, value)
      key.is_a?(String) && /\A-?\d+\z/.match?(key) && (value.is_a?(Hash) || value.is_a?(Parameters))
    end

    # The store's key for a key a caller passes: a Symbol reads as its
    # name, as the indifferent hash converts it.
    def self.convert_key(key)
      key.to_s
    end

    # The indifferent hash's `convert_value`: a nested Hash is copied
    # with its Symbol keys as names (all the way down), an Array is
    # copied with its elements converted, and anything else (a
    # Parameters included) is stored as it is.
    def self.convert_value(value)
      if value.is_a?(Hash)
        copy = {}
        value.each_pair { |k, v| copy[k.is_a?(Symbol) ? k.to_s : k] = Parameters.convert_value(v) }
        copy
      elsif value.is_a?(Array)
        value.map { |e| Parameters.convert_value(e) }
      else
        value
      end
    end

    # A HashWithIndifferentAccess over `hash` where the tree defines one.
    def self.indifferent(hash)
      if defined?(::ActiveSupport::HashWithIndifferentAccess)
        ::ActiveSupport::HashWithIndifferentAccess.new(hash)
      else
        hash
      end
    end

    def initialize(parameters = {}, logging_context = {})
      source = parameters.is_a?(Hash) ? Parameters.checked(parameters) : Parameters.hash_like(parameters)
      store = {}
      source.each_pair { |key, value| store[Parameters.convert_key(key)] = Parameters.convert_value(value) }
      @parameters = store
      @logging_context = logging_context
      @permitted = Parameters.permit_all_parameters
    end

    # `parameters.each_key` checking each key, as Rails' constructor does.
    def self.checked(parameters)
      parameters.each_key { |key| Parameters.check_key(key) }
      parameters
    end

    def self.check_key(key)
      return if key.is_a?(String) || key.is_a?(Symbol)
      raise InvalidParameterKey, "all keys must be Strings or Symbols, got: #{key.class}"
    end

    # Rails' constructor over something other than a Hash: its keys
    # checked through `each_key`, then its `with_indifferent_access`.
    # Each raises Ruby's NoMethodError where the object lacks it (a
    # Parameters answers `each_key` but not `with_indifferent_access`).
    def self.hash_like(parameters)
      duck = parameters #: untyped
      duck.each_key { |key| Parameters.check_key(key) }
      duck.with_indifferent_access
    end

    # Rails' protected reader, for `==` and the filters.
    def parameters
      @parameters
    end

    def permitted=(value)
      @permitted = value
    end

    def ==(other)
      if other.is_a?(Parameters)
        permitted? == other.permitted? && parameters == other.parameters
      elsif other.respond_to?(:permitted?)
        duck = other #: untyped
        permitted? == duck.permitted? && parameters == duck.parameters
      else
        equal?(other)
      end
    end

    def eql?(other)
      other.is_a?(Parameters) && self.class == other.class &&
        permitted? == other.permitted? && parameters.eql?(other.parameters)
    end

    def hash
      [self.class, @parameters, @permitted].hash
    end

    def deconstruct_keys(keys)
      params = keys.nil? ? self : slice(*keys)
      out = {}
      params.keys.each { |key| out[key.to_sym] = params[key] }
      out
    end

    # Delegated to the store, as Rails delegates them to @parameters.
    def keys
      @parameters.keys
    end

    def empty?
      @parameters.empty?
    end

    def include?(key)
      @parameters.key?(Parameters.convert_key(key))
    end

    def has_key?(key)
      include?(key)
    end

    def key?(key)
      include?(key)
    end

    def member?(key)
      include?(key)
    end

    def exclude?(key)
      !include?(key)
    end

    def each_key(&block)
      @parameters.each_key(&block)
      self
    end

    def to_s
      @parameters.to_s
    end

    def as_json(options = nil)
      Parameters.indifferent(@parameters).as_json(options)
    end

    def to_h(&block)
      raise UnfilteredParameters unless permitted?
      convert_parameters_to_hashes(@parameters, false, &block)
    end

    def to_hash
      to_h.to_hash
    end

    def to_query(*args)
      hash = to_h #: untyped
      hash.to_query(*args)
    end

    def to_param(*args)
      to_query(*args)
    end

    def to_unsafe_h
      convert_parameters_to_hashes(@parameters, true)
    end

    def to_unsafe_hash
      to_unsafe_h
    end

    def each_pair
      @parameters.keys.each do |key|
        yield [key, convert_hashes_to_parameters(key, @parameters[key])]
      end
      self
    end

    def each(&block)
      each_pair(&block)
    end

    def each_value
      @parameters.keys.each do |key|
        yield convert_hashes_to_parameters(key, @parameters[key])
      end
      self
    end

    def values
      @parameters.keys.map { |key| convert_hashes_to_parameters(key, @parameters[key]) }
    end

    def permitted?
      @permitted == true
    end

    def permit!
      @parameters.keys.each { |key| Parameters.permit_nested!(self[key]) }
      @permitted = true
      self
    end

    # `Array.wrap(value).flatten.each { |v| v.permit! if v.respond_to?(:permit!) }`
    def self.permit_nested!(value)
      if value.is_a?(Parameters)
        value.permit!
      elsif value.is_a?(Array)
        value.each { |v| Parameters.permit_nested!(v) }
      end
      value
    end

    def require(key)
      return key.map { |k| require(k) } if key.is_a?(Array)
      value = self[key]
      if Parameters.present?(value) || value == false
        value
      else
        raise ParameterMissing.new(key)
      end
    end

    def required(key)
      require(key)
    end

    def permit(*filters)
      permit_filters(filters, Parameters.action_on_unpermitted_parameters, false)
    end

    def expect(*filters)
      params = permit_filters(filters, nil, true)
      keys = filters.flatten.flat_map { |f| f.is_a?(Hash) ? f.keys : [f] }
      values = keys.map { |k| params.require(k) }
      values.size == 1 ? values.first : values
    end

    def expect!(*filters)
      expect(*filters)
    rescue ParameterMissing => e
      raise ExpectedParameterMissing.new(e.param)
    end

    def [](key)
      name = Parameters.convert_key(key)
      convert_hashes_to_parameters(name, @parameters[name])
    end

    def []=(key, value)
      @parameters[Parameters.convert_key(key)] = Parameters.convert_value(value)
    end

    def fetch(key, *args)
      name = Parameters.convert_key(key)
      return convert_value_to_parameters(@parameters[name]) if @parameters.key?(name)
      return convert_value_to_parameters(yield(key)) if block_given?
      raise ParameterMissing.new(key) if args.empty?
      convert_value_to_parameters(args[0])
    end

    def fetch_values(*keys)
      keys.map do |key|
        name = Parameters.convert_key(key)
        if @parameters.key?(name)
          convert_value_to_parameters(@parameters[name])
        elsif block_given?
          convert_value_to_parameters(yield(key))
        else
          raise ParameterMissing.new(key)
        end
      end
    end

    def dig(*keys)
      raise ArgumentError, "wrong number of arguments (given 0, expected 1+)" if keys.empty?
      first = Parameters.convert_key(keys.first)
      value = convert_hashes_to_parameters(first, @parameters[first])
      rest = keys.drop(1)
      return value if rest.empty? || value.nil?
      return value.dig(*rest) if value.is_a?(Parameters)
      return value.dig(*rest) if value.is_a?(Array)
      return value.dig(*rest) if value.is_a?(Hash)
      raise TypeError, "#{value.class} does not have #dig method"
    end

    def slice(*keys)
      new_instance_with_inherited_permitted_status(store_slice(keys))
    end

    def slice!(*keys)
      @parameters = store_slice(keys)
      self
    end

    def except(*keys)
      names = keys.map { |k| Parameters.convert_key(k) }
      kept = {}
      @parameters.each_pair { |k, v| kept[k] = v unless names.include?(k) }
      new_instance_with_inherited_permitted_status(kept)
    end

    def without(*keys)
      except(*keys)
    end

    def extract!(*keys)
      taken = {}
      keys.each do |k|
        name = Parameters.convert_key(k)
        taken[name] = @parameters.delete(name) if @parameters.key?(name)
      end
      new_instance_with_inherited_permitted_status(taken)
    end

    def transform_values
      changed = {}
      @parameters.keys.each { |k| changed[k] = yield(convert_value_to_parameters(@parameters[k])) }
      new_instance_with_inherited_permitted_status(changed)
    end

    def transform_values!
      @parameters.keys.each { |k| @parameters[k] = Parameters.convert_value(yield(convert_value_to_parameters(@parameters[k]))) }
      self
    end

    def transform_keys
      changed = {}
      @parameters.each_pair { |k, v| changed[Parameters.convert_key(yield(k))] = v }
      new_instance_with_inherited_permitted_status(changed)
    end

    def transform_keys!
      changed = {}
      @parameters.each_pair { |k, v| changed[Parameters.convert_key(yield(k))] = v }
      @parameters = changed
      self
    end

    def deep_transform_keys
      raise NotImplementedError, "ActionController::Parameters#deep_transform_keys is not ported"
    end

    def deep_transform_keys!
      raise NotImplementedError, "ActionController::Parameters#deep_transform_keys! is not ported"
    end

    def deep_transform_values
      raise NotImplementedError, "ActionController::Parameters#deep_transform_values is not ported"
    end

    def deep_transform_values!
      raise NotImplementedError, "ActionController::Parameters#deep_transform_values! is not ported"
    end

    def deep_merge(_other)
      raise NotImplementedError, "ActionController::Parameters#deep_merge is not ported"
    end

    def deep_merge!(_other)
      raise NotImplementedError, "ActionController::Parameters#deep_merge! is not ported"
    end

    def delete(key)
      name = Parameters.convert_key(key)
      return convert_value_to_parameters(@parameters.delete(name)) if @parameters.key?(name)
      return convert_value_to_parameters(yield(name)) if block_given?
      nil
    end

    def select
      kept = {}
      @parameters.each_pair { |k, v| kept[k] = v if yield(k, v) }
      new_instance_with_inherited_permitted_status(kept)
    end

    def select!
      kept = {}
      @parameters.each_pair { |k, v| kept[k] = v if yield(k, v) }
      @parameters = kept
      self
    end

    def keep_if(&block)
      select!(&block)
    end

    def reject
      kept = {}
      @parameters.each_pair { |k, v| kept[k] = v unless yield(k, v) }
      new_instance_with_inherited_permitted_status(kept)
    end

    def reject!
      kept = {}
      @parameters.each_pair { |k, v| kept[k] = v unless yield(k, v) }
      @parameters = kept
      self
    end

    def delete_if(&block)
      reject!(&block)
    end

    def compact
      new_instance_with_inherited_permitted_status(@parameters.compact)
    end

    def compact!
      before = @parameters.size
      @parameters = @parameters.compact
      before == @parameters.size ? nil : self
    end

    def compact_blank
      kept = {}
      @parameters.each_pair { |k, v| kept[k] = v unless Parameters.blank?(v) }
      new_instance_with_inherited_permitted_status(kept)
    end

    def compact_blank!
      kept = {}
      @parameters.each_pair { |k, v| kept[k] = v unless Parameters.blank?(v) }
      @parameters = kept
      self
    end

    def has_value?(value)
      values.include?(convert_value_to_parameters(value))
    end

    def value?(value)
      has_value?(value)
    end

    def values_at(*keys)
      keys.map { |k| convert_value_to_parameters(@parameters[Parameters.convert_key(k)]) }
    end

    def merge(*other_hashes, &block)
      merged = @parameters.dup
      other_hashes.each { |other| Parameters.update_store(merged, Parameters.hash_of(other), block) }
      new_instance_with_inherited_permitted_status(merged)
    end

    def merge!(*other_hashes, &block)
      other_hashes.each { |other| Parameters.update_store(@parameters, Parameters.hash_of(other), block) }
      self
    end

    def reverse_merge(other_hash)
      merged = {}
      Parameters.update_store(merged, Parameters.hash_of(other_hash), nil)
      @parameters.each_pair { |k, v| merged[k] = v }
      new_instance_with_inherited_permitted_status(merged)
    end

    def with_defaults(other_hash)
      reverse_merge(other_hash)
    end

    def reverse_merge!(other_hash)
      Parameters.hash_of(other_hash).each_pair do |k, v|
        name = Parameters.convert_key(k)
        @parameters[name] = Parameters.convert_value(v) unless @parameters.key?(name)
      end
      self
    end

    def with_defaults!(other_hash)
      reverse_merge!(other_hash)
    end

    def stringify_keys
      dup
    end

    def inspect
      "#<#{self.class} #{@parameters} permitted: #{@permitted}>"
    end

    def self.hook_into_yaml_loading
      raise NotImplementedError, "ActionController::Parameters.hook_into_yaml_loading (the YAML load tags) is not ported"
    end

    def init_with(coder)
      tag = coder.tag #: String
      map = coder.map #: Hash[String, ::Object]
      if tag == "!ruby/hash:ActionController::Parameters"
        @parameters = Parameters.store_of(map)
        @permitted = false
      elsif tag == "!ruby/hash-with-ivars:ActionController::Parameters"
        @parameters = Parameters.store_of(map["elements"])
        ivars = map["ivars"] #: Hash[Symbol, ::Object]
        @permitted = ivars[:@permitted] == true
      elsif tag == "!ruby/object:ActionController::Parameters"
        @parameters = Parameters.store_of(map["parameters"])
        @permitted = map["permitted"] == true
      end
      @logging_context = {}
    end

    def encode_with(coder)
      coder.map = { "parameters" => Parameters.indifferent(@parameters), "permitted" => @permitted }
    end

    def dup
      copy = self.class.new({}, @logging_context)
      copy.replace_store(@parameters.dup)
      copy.permitted = @permitted
      copy
    end

    def deep_dup
      copy = self.class.new(Parameters.deep_dup_value(@parameters), @logging_context)
      copy.permitted = @permitted
      copy
    end

    def extract_value(key, delimiter: "_")
      value = @parameters[Parameters.convert_key(key)]
      return nil if value.nil?
      return value.split(delimiter, -1) if value.is_a?(String)
      raise NoMethodError.new("undefined method 'split' for an instance of #{value.class}")
    end

    def replace_store(store)
      @parameters = store
    end

    def nested_attributes?
      @parameters.any? { |k, v| Parameters.nested_attribute?(k, v) }
    end

    # Rails' `each_nested_attribute { |v| v.permit_filters(...) }`, as
    # `permit_array_of_hashes` calls it.
    def permit_nested_attributes(filter, on_unpermitted, explicit_arrays)
      hash = self.class.new
      @parameters.keys.each do |key|
        value = self[key]
        next unless Parameters.nested_attribute?(key, value)
        next unless value.is_a?(Parameters)
        hash[key] = value.permit_filters(Parameters.wrap(filter), on_unpermitted, explicit_arrays)
      end
      hash
    end

    def permit_filters(filters, on_unpermitted, explicit_arrays)
      params = self.class.new
      filters.flatten.each do |filter|
        if filter.is_a?(Symbol) || filter.is_a?(String)
          permitted_scalar_filter(params, filter)
        elsif filter.is_a?(Hash)
          hash_filter(params, filter, on_unpermitted, explicit_arrays)
        end
      end
      unpermitted_parameters!(params, on_unpermitted: on_unpermitted)
      params.permit!
    end

    # The store of a revived coder map (an indifferent hash, or a Hash).
    def self.store_of(map)
      store = {}
      if map.is_a?(Hash)
        map.each_pair { |k, v| store[Parameters.convert_key(k)] = Parameters.convert_value(v) }
      end
      store
    end

    # A Parameters, a Hash or an object answering `to_h`, as the Hash
    # `merge` / `reverse_merge` read (`other.to_h`).
    def self.hash_of(other)
      return other.to_h if other.is_a?(Parameters)
      return other if other.is_a?(Hash)
      duck = other #: untyped
      duck.to_h
    end

    # The indifferent hash's `update`: each key converted, each value
    # converted, a block resolving a key both sides hold.
    def self.update_store(store, other, block)
      other.each_pair do |k, v|
        name = Parameters.convert_key(k)
        if !block.nil? && store.key?(name)
          store[name] = Parameters.convert_value(block.call(name, store[name], v))
        else
          store[name] = Parameters.convert_value(v)
        end
      end
      store
    end

    # Object#deep_dup over the store (core_ext/object/deep_dup.rb).
    def self.deep_dup_value(value)
      if value.is_a?(Hash)
        copy = {}
        value.each_pair { |k, v| copy[Parameters.deep_dup_value(k)] = Parameters.deep_dup_value(v) }
        copy
      elsif value.is_a?(Array)
        value.map { |e| Parameters.deep_dup_value(e) }
      elsif value.is_a?(Parameters)
        value.deep_dup
      elsif value.is_a?(Method) || value.is_a?(UnboundMethod)
        value
      else
        value.dup
      end
    end

    # Object#blank? / #present? (core_ext/object/blank.rb).
    def self.blank?(value)
      return true if value.nil? || value == false
      return /\A[[:space:]]*\z/.match?(value) if value.is_a?(String)
      return value.empty? if value.is_a?(Array)
      return value.empty? if value.is_a?(Hash)
      return value.empty? if value.is_a?(Parameters)
      return value.empty? if value.is_a?(Symbol)
      return false if value == true || value.is_a?(Numeric) || value.is_a?(Time)
      return false unless value.respond_to?(:empty?)
      duck = value #: untyped
      duck.empty? ? true : false
    end

    def self.present?(value)
      !Parameters.blank?(value)
    end

    # PERMITTED_SCALAR_TYPES: String, Symbol, NilClass, Numeric,
    # TrueClass, FalseClass, Date, Time, StringIO, IO,
    # ActionDispatch::Http::UploadedFile, Rack::Test::UploadedFile.
    def self.permitted_scalar?(value)
      return true if value.nil? || value == true || value == false
      return true if value.is_a?(String) || value.is_a?(Symbol) || value.is_a?(Numeric)
      return true if value.is_a?(Time) || value.is_a?(IO)
      return true if defined?(::Date) && value.is_a?(::Date)
      return true if defined?(::StringIO) && value.is_a?(::StringIO)
      return true if defined?(::ActionDispatch::Http::UploadedFile) && value.is_a?(::ActionDispatch::Http::UploadedFile)
      return true if defined?(::Rack::Test::UploadedFile) && value.is_a?(::Rack::Test::UploadedFile)
      false
    end

    # Array.wrap (core_ext/array/wrap.rb).
    def self.wrap(object)
      if object.nil?
        []
      elsif object.is_a?(Array)
        object
      else
        [object]
      end
    end

    private

    def store_slice(keys)
      kept = {}
      keys.each do |k|
        name = Parameters.convert_key(k)
        kept[name] = @parameters[name] if @parameters.key?(name)
      end
      kept
    end

    def new_instance_with_inherited_permitted_status(hash)
      copy = self.class.new(hash, @logging_context)
      copy.permitted = @permitted
      copy
    end

    def convert_parameters_to_hashes(value, unsafe, &block)
      if value.is_a?(Array)
        value.map { |v| convert_parameters_to_hashes(v, unsafe) }
      elsif value.is_a?(Hash)
        transformed = {}
        value.each_pair { |k, v| transformed[k] = convert_parameters_to_hashes(v, unsafe) }
        transformed = transformed.to_h(&block) unless block.nil?
        Parameters.indifferent(transformed)
      elsif value.is_a?(Parameters)
        unsafe ? value.to_unsafe_h : value.to_h
      else
        value
      end
    end

    def convert_hashes_to_parameters(key, value)
      converted = convert_value_to_parameters(value)
      @parameters[key] = converted unless converted.equal?(value)
      converted
    end

    # A Hash becomes a Parameters; an Array has its elements converted,
    # and is answered as it is once nothing in it changes (Rails tracks
    # its converted arrays for the same effect).
    def convert_value_to_parameters(value)
      if value.is_a?(Array)
        converted = value.map { |e| convert_value_to_parameters(e) }
        i = 0
        while i < converted.size
          return converted unless converted[i].equal?(value[i])
          i += 1
        end
        value
      elsif value.is_a?(Hash)
        self.class.new(value, @logging_context)
      else
        value
      end
    end

    def specify_numeric_keys?(filter)
      return false unless filter.is_a?(Hash)
      filter.keys.any? { |key| /\A-?\d+\z/.match?(key.to_s) }
    end

    def array_filter?(filter)
      filter.is_a?(Array) && filter.size == 1 && filter.first.is_a?(Array)
    end

    # Rails 8.2 takes the action as a keyword defaulting to the class's
    # setting: typed_parameters' `permit_types` calls it with `params` alone.
    def unpermitted_parameters!(params, on_unpermitted: Parameters.action_on_unpermitted_parameters)
      return if on_unpermitted.nil? || on_unpermitted == false
      unpermitted = unpermitted_keys(params)
      return if unpermitted.empty?
      if on_unpermitted == :log
        if defined?(::ActiveSupport::Notifications)
          notifications = ::ActiveSupport::Notifications #: untyped
          notifications.instrument("unpermitted_parameters.action_controller", { keys: unpermitted, context: @logging_context })
        end
      elsif on_unpermitted == :raise
        raise UnpermittedParameters.new(unpermitted)
      end
    end

    def unpermitted_keys(params)
      keys - params.keys - Parameters.always_permitted_parameters
    end

    def permitted_scalar_filter(params, permitted_key)
      name = permitted_key.to_s
      if has_key?(name) && Parameters.permitted_scalar?(self[name])
        params[name] = self[name]
      end
      @parameters.keys.each do |key|
        match = /\(\d+[if]?\)\z/.match(key)
        next if match.nil?
        next unless match.pre_match == name
        params[key] = self[key] if Parameters.permitted_scalar?(self[key])
      end
    end

    def non_scalar?(value)
      value.is_a?(Array) || value.is_a?(Parameters)
    end

    def hash_filter(params, filter, on_unpermitted, explicit_arrays)
      wanted = {}
      filter.each_pair { |k, v| wanted[Parameters.convert_key(k)] = Parameters.convert_value(v) }
      sliced = slice(*wanted.keys)
      sliced.keys.each do |key|
        value = sliced[key]
        next unless value
        next unless has_key?(key)
        result = permit_value(value, wanted[key], on_unpermitted, explicit_arrays)
        params[key] = result unless result.nil?
      end
    end

    def permit_value(value, filter, on_unpermitted, explicit_arrays)
      if filter == []
        permit_array_of_scalars(value)
      elsif filter == {}
        permit_hash(value, filter, on_unpermitted, explicit_arrays)
      elsif filter.is_a?(Array) && array_filter?(filter)
        permit_array_of_hashes(value, filter.first, on_unpermitted, explicit_arrays)
      elsif explicit_arrays
        permit_hash(value, filter, on_unpermitted, explicit_arrays)
      elsif non_scalar?(value)
        permit_hash_or_array(value, filter, on_unpermitted, explicit_arrays)
      end
    end

    def permit_array_of_scalars(value)
      return nil unless value.is_a?(Array)
      value.all? { |element| Parameters.permitted_scalar?(element) } ? value : nil
    end

    # Rails' `each_array_element(value, filter) { |e| e.permit_filters(...) }`.
    def permit_array_of_hashes(value, filter, on_unpermitted, explicit_arrays)
      if value.is_a?(Array)
        permitted = []
        value.each do |element|
          next unless element.is_a?(Parameters)
          permitted << element.permit_filters(Parameters.wrap(filter), on_unpermitted, explicit_arrays)
        end
        permitted
      elsif value.is_a?(Parameters)
        if value.nested_attributes? && !specify_numeric_keys?(filter)
          value.permit_nested_attributes(filter, on_unpermitted, explicit_arrays)
        end
      end
    end

    def permit_hash(value, filter, on_unpermitted, explicit_arrays)
      return nil unless value.is_a?(Parameters)
      if filter == {}
        permit_any_in_parameters(value)
      else
        value.permit_filters(Parameters.wrap(filter), on_unpermitted, explicit_arrays)
      end
    end

    def permit_hash_or_array(value, filter, on_unpermitted, explicit_arrays)
      permit_array_of_hashes(value, filter, on_unpermitted, explicit_arrays) ||
        permit_hash(value, filter, on_unpermitted, explicit_arrays)
    end

    def permit_any_in_parameters(params)
      sanitized = self.class.new
      params.keys.each do |key|
        value = params[key]
        if Parameters.permitted_scalar?(value)
          sanitized[key] = value
        elsif value.is_a?(Array)
          sanitized[key] = permit_any_in_array(value)
        elsif value.is_a?(Parameters)
          sanitized[key] = permit_any_in_parameters(value)
        end
      end
      sanitized
    end

    def permit_any_in_array(array)
      sanitized = []
      array.each do |element|
        if Parameters.permitted_scalar?(element)
          sanitized << element
        elsif element.is_a?(Array)
          sanitized << permit_any_in_array(element)
        elsif element.is_a?(Parameters)
          sanitized << permit_any_in_parameters(element)
        end
      end
      sanitized
    end
  end
end
