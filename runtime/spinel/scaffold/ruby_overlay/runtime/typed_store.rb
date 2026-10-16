# typed_store gem subset (CRuby overlay) — YAML-serialized virtual
# attributes backed by a TEXT column. Lobsters' User declares
# `typed_store :settings do |s| s.string :totp_secret … end`; the Ruby
# emit path (`apply_typed_store_lowering`) synthesizes per-attribute
# readers/writers that route through this module, so the model file
# stays declarative and the serialization format lives in one place.
#
# The store column holds a YAML hash with string keys
# (`prefers_color_scheme: system\n…` in the benchmark DB). Reads far
# outnumber writes (the benchmark never writes settings), so `parse`
# keeps a one-entry cache keyed by the serialized string — /settings
# reads ~24 attributes off the same row without re-parsing per
# attribute.
require "yaml"
require "date"
begin
  require "bigdecimal"
rescue LoadError
end

module TypedStore
  # What a stored settings hash may hold beyond the safe scalars, as a
  # Rails app's `config.active_record.yaml_column_permitted_classes`
  # (Core's: Symbol, Time, DateTime, Date, BigDecimal); aliases are what
  # `YAML.dump` writes for one object held twice.
  PERMITTED_CLASSES = [Symbol, Time, DateTime, Date, (BigDecimal if defined?(BigDecimal))].compact.freeze

  # The one-entry parse memo, per thread (as thread_state.rb keeps the
  # shared one): two requests parsing different rows neither evict each
  # other nor hand each other the wrong hash.
  def self.parse(serialized)
    s = serialized.to_s
    cached = Thread.current[:typed_store_val]
    return cached if cached && s == Thread.current[:typed_store_key]
    h = s.empty? ? {} : YAML.safe_load(s, permitted_classes: PERMITTED_CLASSES, aliases: true)
    h = {} unless h.is_a?(Hash)
    Thread.current[:typed_store_key] = s
    Thread.current[:typed_store_val] = h
    h
  end

  # The stored value for `key`, or `default` when the hash carries no
  # entry (matching typed_store: an explicit stored nil stays nil).
  def self.read(serialized, key, default)
    h = parse(serialized)
    h.key?(key) ? h[key] : default
  end

# One half of a saved change of the store column, read at `key`: the
# snapshot Hash `saved_changes` pairs hold for a store with a view
# (`record._typed_store_changes`), or the stored text. A half the column
# did not have yet (the nil before a create) reads the declared default.
def self.pair_value(half, key, default)
  return default if half.nil?
  return(half.key?(key) ? half[key] : default) if half.is_a?(Hash)
  read(half, key, default)
end

# activerecord store.rb's `saved_change_to_<key>?`, `saved_change_to_<key>`
# and `<key>_before_last_save`, over the column's saved change `pair`
# (`saved_change_to_attribute(column)`, nil when the save left it alone).
def self.saved_key_changed?(pair, key, default)
  return false if pair.nil?
  pair_value(pair[0], key, default) != pair_value(pair[1], key, default)
end

def self.saved_key_change(pair, key, default)
  return nil unless saved_key_changed?(pair, key, default)
  [pair_value(pair[0], key, default), pair_value(pair[1], key, default)]
end

# A create leaves the column unchanged in the emitted tree (native writes
# the declared defaults into it), so the half before it is the default.
def self.key_before_last_save(pair, key, default, created)
  return(created ? default : nil) if pair.nil?
  pair_value(pair[0], key, default)
end

  # Returns the re-serialized store with `key` set — the caller
  # assigns it back to the column ivar.
  def self.write(serialized, key, value)
    h = parse(serialized).dup
    h[key] = value
    Thread.current[:typed_store_key] = nil
    YAML.dump(h)
  end

  # The store column's reader (`lower::typed_store`'s
  # `push_store_hash_methods`): the gem's TypedHash, a Hash with
  # indifferent access holding every declared attribute (its default
  # until written) and any other stored key. Reads and writes go through
  # the column, so `shop.settings[:plan_name] = "x"` is what the record
  # saves; the record's generated `__rh_<column>_keys`,
  # `__rh_<column>_default(key)` and `__rh_<column>_cast(key, value)`
  # carry the declarations.
  class Accessor
    include Enumerable

    def initialize(record, column)
      @record = record
      @ivar = :"@#{column}"
      @column = column
    end

    def [](key)
      k = key.to_s
      TypedStore.read(serialized, k, @record.__send__(:"__rh_#{@column}_default", k))
    end

    def []=(key, value)
      k = key.to_s
      cast = @record.__send__(:"__rh_#{@column}_cast", k, value)
      @record.instance_variable_set(@ivar, TypedStore.write(serialized, k, cast))
      value
    end

    def key?(key)
      k = key.to_s
      declared.include?(k) || TypedStore.parse(serialized).key?(k)
    end
    alias has_key? key?
    alias include? key?
    alias member? key?

    def fetch(key, *default)
      return self[key] if key?(key)
      return yield(key) if block_given?
      return default.first unless default.empty?
      raise KeyError, "key not found: #{key.to_s.inspect}"
    end

    def dig(key, *rest)
      value = self[key]
      rest.empty? || value.nil? ? value : value.dig(*rest)
    end

    def keys
      (declared + TypedStore.parse(serialized).keys).uniq
    end

    def to_h
      h = defined?(ActiveSupport::HashWithIndifferentAccess) ? ActiveSupport::HashWithIndifferentAccess.new : {}
      keys.each { |k| h[k] = self[k] }
      h
    end
    alias to_hash to_h

    def each(&block)
      return to_enum(:each) unless block
      to_h.each(&block)
      self
    end

    def ==(other)
      to_h == (other.is_a?(Accessor) ? other.to_h : other)
    end

    def inspect
      to_h.inspect
    end

    # The rest of the TypedHash's Hash surface (`values_at`, `slice`,
    # `symbolize_keys`, `except`, ... : Core's
    # `ShopBillingSetting::Extension` reads `self[:status].values_at(*keys)`)
    # answers over a copy, as `to_h` does. Hash's own `select`, `reject`
    # and `compact` answer a Hash, where the Enumerable ones above would
    # answer pairs.
    def select(&block) = to_h.select(&block)
    alias filter select

    def reject(&block) = to_h.reject(&block)

    def compact = to_h.compact

    def method_missing(name, *args, &block)
      h = to_h
      return super unless h.respond_to?(name)
      h.public_send(name, *args, &block)
    end

    def respond_to_missing?(name, include_private = false)
      to_h.respond_to?(name) || super
    end

    private

    def serialized
      @record.instance_variable_get(@ivar)
    end

    def declared
      @record.__send__(:"__rh_#{@column}_keys")
    end
  end

  # `<column>_was` on a store: the Hash the column held before the
  # pending change (`serialized`, `attribute_was`), with the declared
  # defaults filled in as the reader's view has them. A copy, as Rails'
  # dirty tracking hands out.
  def self.snapshot(record, column, serialized)
    Snapshot.new(record, column, serialized).to_h
  end

  class Snapshot < Accessor
    def initialize(record, column, serialized)
      super(record, column)
      @serialized = serialized
    end

    private

    def serialized
      @serialized
    end
  end
  private_constant :Snapshot
end

# The gem's class-side `typed_store(store_attribute, options = {}, &block)`
# (extension.rb, dsl.rb and field.rb of activerecord-typedstore 1.6), for
# a call the class body makes at load: an STI subclass's own, run there
# by a Concern's `included do`, or reached through an app macro (Core's
# `redactable_typed_store`). A model's own direct declaration is lowered
# statically instead (`lower::typed_store`); both keep the store in the
# column's instance variable through `TypedStore.read`/`write`. Each
# attribute's reader casts the stored value (or answers its default),
# its writer casts as the declared type does, and `<name>_changed?` /
# `<name>_was` compare with the column before the pending change. The
# accessors live in a module the call includes into the class, as
# `store_accessor` defines them, so a method the class defines itself
# wins and a module included before the call (an abstract interface's
# reader) does not. `typed_stores` maps each store to its declaration,
# inherited by subclasses. The coder (the column's serialization) is not
# applied: the store keeps the YAML of `TypedStore.write`, as a lowered
# declaration does.
module TypedStore
  class Field
    attr_reader :array, :blank, :name, :default, :null, :accessor, :type_sym

    TYPES = {
      boolean: "Boolean", integer: "Integer", string: "String", text: "String", float: "Float",
      date: "Date", time: "Time", datetime: "DateTime", decimal: "Decimal", any: "Value",
    }.freeze

    def initialize(name, type, options = {})
      @type_options = {}
      %i[scale limit precision].each { |k| @type_options[k] = options[k] if options.key?(k) }
      @type_sym = type
      @accessor = options.fetch(:accessor, true)
      @name = name
      @array = options.fetch(:array, false)
      @default = extract_default(options[:default]) if options.key?(:default)
      @null = options.fetch(:null, true)
      @blank = options.fetch(:blank, true)
    end

    # Active Record's type of that name, else Active Model's (`Time`).
    def type
      @type ||= begin
        name = TYPES.fetch(type_sym)
        owner = ActiveRecord::Type.const_defined?(name, false) ? ActiveRecord::Type : ActiveModel::Type
        owner.const_get(name, false).new(**@type_options)
      end
    end

    def has_default?
      instance_variable_defined?(:@default)
    end

    def cast(value)
      casted_value = type_cast(value)
      if !blank
        casted_value = default if casted_value.blank?
      elsif array && has_default?
        casted_value = default if value.nil?
      elsif !null
        casted_value = default if casted_value.nil?
      end
      casted_value
    end

    private

    def extract_default(value)
      return value if (type_sym == :string || type_sym == :text) && value.nil?
      type_cast(value)
    end

    def type_cast(value, arrayize: true)
      if array && (arrayize || value.is_a?(Array))
        return [] if arrayize && !value.is_a?(Array)
        return value.map { |v| type_cast(v, arrayize: false) }
      end
      if type_sym == :string || type_sym == :text
        return value.to_s unless value.blank? && (null || array)
      end
      type.cast(value)
    end
  end

  class DSL
    attr_reader :fields, :coder, :store_name

    def initialize(store_name, options)
      @coder = options[:coder]
      @store_name = store_name
      @prefix = affix(options[:prefix], "#{store_name}_") { |v| "#{v}_" }
      @suffix = affix(options[:suffix], "_#{store_name}") { |v| "_#{v}" }
      @accessors = if options[:accessors] == false
        {}
      elsif options[:accessors].is_a?(Array)
        options[:accessors].each_with_object({}) { |n, h| h[n] = accessor_key_for(n) }
      end
      @fields = {}
      yield self if block_given?
    end

    def accessors
      @accessors || @fields.values.select(&:accessor).each_with_object({}) { |f, h| h[f.name] = accessor_key_for(f.name) }
    end

    def keys
      @fields.keys
    end

    %i[string text integer float time datetime date boolean decimal any].each do |type|
      define_method(type) { |name, **options| @fields[name] = Field.new(name, type, options) }
    end
    alias_method :date_time, :datetime

    private

    def affix(value, store_named)
      case value
      when String, Symbol then yield(value)
      when true then store_named
      when false, nil then ""
      else raise ArgumentError, "Unexpected type for prefix option. Expected string, symbol, or boolean"
      end
    end

    def accessor_key_for(name)
      "#{@prefix}#{name}#{@suffix}"
    end
  end

  # The stored value of `field` in `serialized`, cast, or its default.
  def self.read_field(serialized, field)
    h = parse(serialized)
    key = field.name.to_s
    return field.default unless h.key?(key)
    field.cast(h[key])
  end

  module ClassMethods
    def typed_store(store_attribute, options = {}, &block)
      dsl = DSL.new(store_attribute, options, &block)
      @typed_stores = (typed_stores || {}).merge(store_attribute => dsl)
      @store_accessors = nil
      ivar = :"@#{store_attribute}"
      column = store_attribute.to_s
      accessors = @_typed_store_accessors ||= Module.new.tap { |m| include m }
      dsl.accessors.each do |name, key|
        field = dsl.fields.fetch(name)
        accessors.define_method(key) { TypedStore.read_field(instance_variable_get(ivar), field) }
        accessors.define_method(:"#{key}=") do |value|
          instance_variable_set(ivar, TypedStore.write(instance_variable_get(ivar), name.to_s, field.cast(value)))
          value
        end
        accessors.define_method(:"#{key}_was") { TypedStore.read_field(attribute_was(column), field) }
        accessors.define_method(:"#{key}_changed?") { __send__(:"#{key}_was") != __send__(key) }
      end
      nil
    end

    def typed_stores
      return @typed_stores if instance_variable_defined?(:@typed_stores) && @typed_stores
      superclass.respond_to?(:typed_stores) ? superclass.typed_stores : nil
    end

    def store_accessors
      @store_accessors ||= (typed_stores || {}).each_value.flat_map { |d| d.accessors.values }.map(&:to_s).to_set
    end
  end
end

ActiveRecord::Base.extend(TypedStore::ClassMethods) if defined?(ActiveRecord::Base)
