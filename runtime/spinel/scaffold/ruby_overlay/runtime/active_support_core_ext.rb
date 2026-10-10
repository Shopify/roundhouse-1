# CRuby-only ActiveSupport core extensions (Object / String / Array /
# Hash / Enumerable / Module).
#
# The reopened-builtin surface the lobsters corpus actually calls, and
# (at the end, verbatim from activesupport 8.2) the core_ext surface
# Shopify Core sends to builtins — ActiveSupport itself ships these as
# core_ext monkey-patches, so the CRuby overlay mirrors that shape.
# tests/core_ext_surface_runtime.rb holds the Core surface to native
# activesupport. Reopening builtins is overlay-only
# (the shared transpiled runtime can't); strict targets get typed
# lowerings for these when lobsters comes up on them.
#
# Inflection here is the rule subset the corpus words exercise
# (story→stories, category→categories, comment→comments, …), not
# ActiveSupport's full irregular/uncountable tables.

class Object
  # `blank?`/`present?` (core_ext/object/blank.rb). Rails' generic form,
  # with its NilClass / FalseClass / TrueClass answers folded into the
  # `!self` tail (Numeric and Time have no `empty?`, so they answer
  # false as Rails' overrides do; Array, Hash and Symbol answer by
  # `empty?`). lobsters calls `present?` ~113 times.
  def blank?
    respond_to?(:empty?) ? !!empty? : !self
  end

  def present?
    !blank?
  end

  # `presence` (core_ext/object/blank.rb): the receiver when present,
  # else nil — the `url.presence || fallback` idiom.
  def presence
    present? ? self : nil
  end

  # `to_param` (core_ext/object/to_param): the URL-segment form of a
  # value, `to_s` by default. Models with a custom `to_param` (lobsters'
  # Domain routes on its name) override this; generated route helpers
  # call it on every segment arg.
  def to_param
    to_s
  end

  # `deep_dup` (core_ext/object/deep_dup.rb) on a receiver inference
  # could not type; a typed Hash/Array is grounded to the same function
  # by `lower::symbolize_keys`.
  def deep_dup
    ActiveSupport.deep_dup(self)
  end
end

# `compact_blank` (core_ext/enumerable.rb + core_ext/hash.rb) — the
# blank family's collection form, and the one member of it `lower::blank`
# cannot always ground.
#
# That pass rewrites `xs.compact_blank` through the ELEMENT type and
# files a `blank_unlowered` diagnostic when the element is untyped,
# saying in as many words that "the CRuby overlay serves it at
# runtime". It did not: nothing here defined the method, so the ledger
# entry promised a fallback that raised NoMethodError. campfire's
# `sessions.pluck(:ip_address).compact_blank` (a concern body, where
# `self` is not yet typed) is the site that showed it.
#
# Enumerable, not Array: Rails defines it there, so a Hash keeps its
# own pair-shaped override below and every other collection gets the
# element form.
module Enumerable
  def compact_blank
    reject(&:blank?)
  end
end

class Hash
  # Rails rejects on the VALUE and keeps the key — `{ host: nil,
  # protocol: "https" }.compact_blank` is `{ protocol: "https" }`,
  # which is how campfire's `SetCurrentRequest` builds default_url_options.
  def compact_blank
    reject { |_k, v| v.blank? }
  end
end

# A string an app has explicitly marked html-safe (`raw(...)`,
# `.html_safe`) — the overlay's ActiveSupport::SafeBuffer equivalent.
# The only behavior it adds is `html_safe?` returning true, which the
# overlay's escape-aware `html_escape` honors (see
# action_view_safe_buffer.rb). Escaping is still decided positionally
# at emit time wherever the walker can see the producer; SafeString
# carries safety across the value boundaries it can't — a `raw()`
# label threaded through an app helper's parameter into `link_to`, or
# a model method that ends in `.html_safe` (lobsters' Hat#to_html_label).
class SafeString < String
  def initialize(value)
    super(value)
    @html_safe = true
  end

  def html_safe?
    @html_safe
  end

  def html_safe
    @html_safe = true
    self
  end

  # Rails' SafeBuffer marks itself unsafe when squish! invokes gsub! or
  # strip!, even when neither call changes the contents.
  def squish!
    super
    @html_safe = false
    self
  end

  # String#to_s on a subclass returns a plain String copy; safety must
  # survive the `.to_s` coercions the emit sprinkles on helper args.
  def to_s
    self
  end
end

class String
  # AS specializes String#blank? to treat whitespace-only as blank,
  # retrying with the regex in the string's own encoding where the
  # UTF-8 one cannot read it (UTF-16 and UTF-32), as Rails does.
  BLANK_RE = /\A[[:space:]]*\z/
  ENCODED_BLANKS = {}

  def blank?
    empty? ||
      begin
        BLANK_RE.match?(self)
      rescue Encoding::CompatibilityError
        (ENCODED_BLANKS[encoding] ||= Regexp.new(BLANK_RE.source.encode(encoding), BLANK_RE.options | Regexp::FIXEDENCODING)).match?(self)
      end
  end

  # Rails' destructive form returns the receiver even when unchanged.
  def squish!
    gsub!(/[[:space:]]+/, " ")
    strip!
    self
  end

  # `html_safe` promotes to the marked type; plain strings answer
  # `html_safe?` false so the escape-aware `html_escape` escapes them.
  # (Both were identity/true in the earlier positional-only world —
  # nothing consumed `html_safe?` then; the SafeString-aware
  # html_escape override is its first consumer.)
  def html_safe
    SafeString.new(self)
  end

  def html_safe?
    false
  end

  # `'story'.pluralize(count)` — singular when count == 1, else the
  # plural form. Every corpus call site passes a count; the inflection
  # applies to the string's tail, matching AS ("#{n} story".pluralize(n)
  # → "2 stories"). A trailing "s" is treated as already-plural
  # (idempotent, like AS's /s$/ → "s" rule).
  def pluralize(count = nil)
    return self if count == 1
    if match?(/([^aeiouy]|qu)y\z/i)
      sub(/y\z/i, "ies")
    elsif end_with?("s")
      self
    elsif match?(/(x|z|ch|sh)\z/i)
      "#{self}es"
    else
      "#{self}s"
    end
  end

  def singularize
    if match?(/([^aeiouy]|qu)ies\z/i)
      sub(/ies\z/i, "y")
    elsif match?(/(s|x|z|ch|sh)es\z/i)
      sub(/es\z/i, "")
    elsif end_with?("s") && !end_with?("ss")
      sub(/s\z/i, "")
    else
      self
    end
  end

  # AS `parameterize`: downcase, squash non-alphanumerics to the
  # separator, collapse runs, trim edges. (No transliteration pass —
  # the corpus titles are ASCII; non-ASCII chars drop like Rails'
  # post-transliterate "?" placeholders do under gsub.)
  def parameterize(separator: "-")
    # Default-separator fast path: literal regexes are compiled once and
    # cached per call-site, where the general path below rebuilds two
    # regexes from `Regexp.escape(separator)` on every call. `separator`
    # is "-" for essentially every caller (Story#title_as_url et al.),
    # so this halves both the time and the allocations of the hottest
    # parameterize sites while staying byte-identical.
    if separator == "-"
      return downcase
        .gsub(/[^a-z0-9\-_]+/, "-")
        .gsub(/-{2,}/, "-")
        .gsub(/\A-|-\z/, "")
    end
    downcase
      .gsub(/[^a-z0-9\-_]+/, separator)
      .gsub(/#{Regexp.escape(separator)}{2,}/, separator)
      .gsub(/\A#{Regexp.escape(separator)}|#{Regexp.escape(separator)}\z/, "")
  end
end

class Array
  # AS core_ext/array/wrap.rb (8.1.4). lower/enumerable_ext.rs folds an
  # `Array.wrap` whose argument has a closed type; an untyped argument
  # (`Shopify::Secrets.build_rotation`'s `secrets`) keeps the call.
  def self.wrap(object)
    if object.nil?
      []
    elsif object.respond_to?(:to_ary)
      object.to_ary || [object]
    else
      [object]
    end
  end

  # AS `to_sentence` with its three connector options, :en defaults:
  # "a", "a and b", "a, b, and c".
  def to_sentence(words_connector: ", ", two_words_connector: " and ", last_word_connector: ", and ")
    case length
    when 0 then ""
    when 1 then self[0].to_s
    when 2 then "#{self[0]}#{two_words_connector}#{self[1]}"
    else "#{self[0..-2].join(words_connector)}#{last_word_connector}#{self[-1]}"
    end
  end

  # AS's ordinal readers (core_ext/array/access.rb). Rails ships
  # `second` through `fifth` plus `forty_two`; the corpus reaches
  # `third` (campfire's `messages_controller_test` pages relative to
  # `@messages.third`), and the neighbours come with it because they
  # are the same one-liner and a list that stops at the first caller is
  # the shape that drifts.
  #
  # `forty_two` is deliberately NOT here: it is a joke method, and a
  # reader who meets it in this file should find it in Rails' docs
  # rather than in ours.
  def second
    self[1]
  end

  def third
    self[2]
  end

  def fourth
    self[3]
  end

  def fifth
    self[4]
  end

  # The rest of activesupport 8.2 core_ext/array/access.rb, verbatim
  # (`forty_two` aside, as above): `from`/`to` slices, the
  # from-the-end readers, and `including` / `excluding` / `without`.
  def from(position)
    self[position, length] || []
  end

  def to(position)
    if position >= 0
      take position + 1
    else
      self[0..position]
    end
  end

  def third_to_last
    self[-3]
  end

  def second_to_last
    self[-2]
  end

  def including(*elements)
    self + elements.flatten(1)
  end

  # `Relation` answers `excluding`/`without` itself (`Room.opens.
  # without(current)`); an Array holding a materialized association is
  # the form campfire's `rooms/refreshes_controller_test` writes
  # (`@room.messages.without(@old_message)`). Array#- compares by
  # `hash`/`eql?`, which the CRuby overlay's record equality
  # (`active_record_bang.rb`: same class and id) answers as Rails' does,
  # so another load of the same row is dropped too.
  def excluding(*elements)
    self - elements.flatten(1)
  end

  alias_method :without, :excluding
end

# `ActiveSupport::DelegationError` (Shopify/rails 5813559f4aed,
# activesupport 8.2.0.alpha, active_support/delegation.rb), verbatim:
# what the `method_missing` the CRuby tree writes for a
# `delegate_missing_to` without `allow_nil` raises on a nil target
# (`ingest::delegate::missing_delegators`).
module ActiveSupport
  # Error generated by `delegate` when a method is called on `nil` and `allow_nil`
  # option is not used.
  class DelegationError < NoMethodError
    class << self
      def nil_target(method_name, target) # :nodoc:
        new("#{method_name} delegated to #{target}, but #{target} is nil")
      end
    end
  end
end

# `Hash#deep_merge` / `deep_merge!` (core_ext/hash/deep_merge.rb over
# ActiveSupport::DeepMergeable), Rails 8.1.4: a nested Hash on both
# sides merges recursively, otherwise the block (when given) or the
# other side's value wins.
module ActiveSupport
  module DeepMergeable
    def deep_merge(other, &block)
      dup.deep_merge!(other, &block)
    end

    def deep_merge!(other, &block)
      merge!(other) do |key, this_val, other_val|
        if this_val.is_a?(DeepMergeable) && this_val.deep_merge?(other_val)
          this_val.deep_merge(other_val, &block)
        elsif block_given?
          block.call(key, this_val, other_val)
        else
          other_val
        end
      end
    end

    def deep_merge?(other)
      other.is_a?(self.class)
    end
  end
end

class Hash
  include ActiveSupport::DeepMergeable

  def deep_merge?(other)
    other.is_a?(Hash)
  end

  # core_ext/hash/indifferent_access.rb.
  def with_indifferent_access
    ActiveSupport::HashWithIndifferentAccess.new(self)
  end

  alias nested_under_indifferent_access with_indifferent_access
end

# core_ext/hash/keys.rb (Rails 8.1.4), which hash/indifferent_access
# loads natively: a gem compiled as source that requires it calls
# `params.stringify_keys` on whatever Hash it is given (activemerchant
# 1.137.0's `Billing::Response#initialize`), where no type lets
# `lower::symbolize_keys` ground the call; Core's SerializationOptions
# reads `stringify_keys` of a Hash entry in `methods:`. Ported method
# for method.
class Hash
  def stringify_keys
    transform_keys { |k| Symbol === k ? k.name : k.to_s }
  end

  def stringify_keys!
    transform_keys! { |k| Symbol === k ? k.name : k.to_s }
  end

  def symbolize_keys
    transform_keys { |key| key.to_sym rescue key }
  end
  alias_method :to_options, :symbolize_keys

  def symbolize_keys!
    transform_keys! { |key| key.to_sym rescue key }
  end
  alias_method :to_options!, :symbolize_keys!

  def assert_valid_keys(*valid_keys)
    valid_keys.flatten!
    each_key do |k|
      unless valid_keys.include?(k)
        raise ArgumentError.new("Unknown key: #{k.inspect}. Valid keys are: #{valid_keys.map(&:inspect).join(', ')}")
      end
    end
  end

  def deep_transform_keys(&block)
    _deep_transform_keys_in_object(self, &block)
  end

  def deep_transform_keys!(&block)
    _deep_transform_keys_in_object!(self, &block)
  end

  def deep_stringify_keys
    deep_transform_keys { |k| Symbol === k ? k.name : k.to_s }
  end

  def deep_stringify_keys!
    deep_transform_keys! { |k| Symbol === k ? k.name : k.to_s }
  end

  def deep_symbolize_keys
    deep_transform_keys { |key| key.to_sym rescue key }
  end

  def deep_symbolize_keys!
    deep_transform_keys! { |key| key.to_sym rescue key }
  end

  private
    def _deep_transform_keys_in_object(object, &block)
      case object
      when Hash
        object.each_with_object(self.class.new) do |(key, value), result|
          result[yield(key)] = _deep_transform_keys_in_object(value, &block)
        end
      when Array
        object.map { |e| _deep_transform_keys_in_object(e, &block) }
      else
        object
      end
    end

    def _deep_transform_keys_in_object!(object, &block)
      case object
      when Hash
        object.keys.each do |key|
          value = object.delete(key)
          object[yield(key)] = _deep_transform_keys_in_object!(value, &block)
        end
        object
      when Array
        object.map! { |e| _deep_transform_keys_in_object!(e, &block) }
      else
        object
      end
    end
end

# ActiveSupport::HashWithIndifferentAccess (Rails 8.1.4,
# active_support/hash_with_indifferent_access.rb): a Hash whose Symbol
# keys are stored as Strings, so `h[:a]` and `h["a"]` read the same
# entry, and whose nested Hashes (also inside Arrays) convert on the way
# in. Ported method for method; the AS core_ext helpers Rails' version
# leans on (`except!`, `symbolize_keys!`, `Hash#reverse_merge`) are
# written out in place. Not ported: `slice!`, `extractable_options?`.
module ActiveSupport
  class HashWithIndifferentAccess < Hash
    def with_indifferent_access
      dup
    end

    def nested_under_indifferent_access
      self
    end

    # Rails' own: the in-place symbolizing forms would break the
    # String-key invariant, and `to_options!` is the hash itself.
    undef :symbolize_keys!
    undef :deep_symbolize_keys!
    def to_options!; self end

    def initialize(constructor = nil)
      if constructor.nil?
        super()
      elsif constructor.respond_to?(:to_hash)
        super()
        update(constructor)

        hash = constructor.is_a?(Hash) ? constructor : constructor.to_hash
        self.default = hash.default unless hash.default.nil?
        self.default_proc = hash.default_proc if hash.default_proc
      else
        super(constructor)
      end
    end

    def self.[](*args)
      new.merge!(Hash[*args])
    end

    alias_method :regular_writer, :[]= unless method_defined?(:regular_writer)
    alias_method :regular_update, :update unless method_defined?(:regular_update)

    def []=(key, value)
      regular_writer(convert_key(key), convert_value(value, conversion: :assignment))
    end

    def store(key, value, convert_value: true)
      value = convert_value(value, conversion: :assignment) if convert_value
      regular_writer(convert_key(key), value)
    end

    def update(*other_hashes, &block)
      other_hashes.each do |other_hash|
        update_with_single_argument(other_hash, block)
      end
      self
    end

    alias_method :merge!, :update

    def key?(key)
      super(convert_key(key))
    end

    alias_method :include?, :key?
    alias_method :has_key?, :key?
    alias_method :member?, :key?

    def [](key)
      super(convert_key(key))
    end

    def assoc(key)
      super(convert_key(key))
    end

    def fetch(key, *extras)
      super(convert_key(key), *extras)
    end

    def dig(*args)
      args[0] = convert_key(args[0]) if args.size > 0
      super(*args)
    end

    def default(key = (no_key = true))
      if no_key
        super()
      else
        super(convert_key(key))
      end
    end

    def values_at(*keys)
      keys.map! { |key| convert_key(key) }
      super
    end

    def fetch_values(*indices, &block)
      indices.map! { |key| convert_key(key) }
      super
    end

    def dup
      copy_defaults(self.class.new(self))
    end

    def merge(*hashes, &block)
      dup.update(*hashes, &block)
    end

    def reverse_merge(other_hash)
      cast(other_hash).merge(self)
    end

    alias_method :with_defaults, :reverse_merge

    def reverse_merge!(other_hash)
      replace(reverse_merge(other_hash))
    end

    alias_method :with_defaults!, :reverse_merge!

    def replace(other_hash)
      super(cast(other_hash))
    end

    def delete(key)
      super(convert_key(key))
    end

    def except(*keys)
      copy = dup
      keys.each { |key| copy.delete(key) }
      copy
    end

    alias_method :without, :except

    def stringify_keys
      dup
    end

    def deep_stringify_keys
      dup
    end

    def symbolize_keys
      to_hash.transform_keys { |key| key.to_sym rescue key }
    end

    alias_method :to_options, :symbolize_keys

    def deep_symbolize_keys
      deep_symbolize(to_hash)
    end

    def select(*args, &block)
      return to_enum(:select) unless block_given?
      dup.tap { |hash| hash.select!(*args, &block) }
    end

    alias_method :filter, :select

    def reject(*args, &block)
      return to_enum(:reject) unless block_given?
      dup.tap { |hash| hash.reject!(*args, &block) }
    end

    def transform_values(&block)
      return to_enum(:transform_values) unless block_given?
      dup.tap { |hash| hash.transform_values!(&block) }
    end

    NOT_GIVEN = Object.new # :nodoc:

    def transform_keys(hash = NOT_GIVEN, &block)
      if NOT_GIVEN.equal?(hash)
        if block_given?
          self.class.new(super(&block))
        else
          to_enum(:transform_keys)
        end
      else
        self.class.new(super)
      end
    end

    def transform_keys!(hash = NOT_GIVEN, &block)
      if NOT_GIVEN.equal?(hash)
        if block_given?
          replace(copy_defaults(transform_keys(&block)))
        else
          return to_enum(:transform_keys!)
        end
      else
        replace(copy_defaults(transform_keys(hash, &block)))
      end
      self
    end

    def slice(*keys)
      keys.map! { |key| convert_key(key) }
      self.class.new(super)
    end

    def compact
      dup.tap(&:compact!)
    end

    def to_hash
      copy = Hash[self]
      copy.transform_values! { |v| convert_value_to_hash(v) }
      copy_defaults(copy)
    end

    def to_proc
      proc { |key| self[key] }
    end

    private
      def cast(other)
        self.class === other ? other : self.class.new(other)
      end

      def convert_key(key)
        Symbol === key ? key.name : key
      end

      def convert_value(value, conversion: nil)
        if value.is_a? Hash
          value.nested_under_indifferent_access
        elsif value.is_a?(Array)
          if conversion != :assignment || value.frozen?
            value = value.dup
          end
          value.map! { |e| convert_value(e, conversion: conversion) }
        else
          value
        end
      end

      def convert_value_to_hash(value)
        if value.is_a? Hash
          value.to_hash
        elsif value.is_a?(Array)
          value.map { |e| convert_value_to_hash(e) }
        else
          value
        end
      end

      # AS `deep_symbolize_keys`: keys of nested Hashes, also inside
      # Arrays, become Symbols where they respond to `to_sym`.
      def deep_symbolize(value)
        case value
        when Hash
          value.each_with_object({}) do |(key, v), out|
            out[(key.to_sym rescue key)] = deep_symbolize(v)
          end
        when Array
          value.map { |e| deep_symbolize(e) }
        else
          value
        end
      end

      def copy_defaults(target)
        if default_proc
          target.default_proc = default_proc.dup
        else
          target.default = default
        end
        target
      end

      def update_with_single_argument(other_hash, block)
        if other_hash.is_a? HashWithIndifferentAccess
          regular_update(other_hash, &block)
        else
          other_hash.to_hash.each_pair do |key, value|
            if block && key?(key)
              value = block.call(convert_key(key), self[key], value)
            end
            regular_writer(convert_key(key), convert_value(value))
          end
        end
      end
  end
end

HashWithIndifferentAccess = ActiveSupport::HashWithIndifferentAccess

# ---------------------------------------------------------------------
# activesupport 8.2 core_ext that Shopify Core calls on builtins
# (Shopify/rails 5813559f4aed, activesupport/lib/active_support/
# core_ext/), verbatim apart from comments. `require "rails/all"` loads
# every one of these before app code; the emitted Core tree reached
# `Array#exclude?` at boot (`Podding::Database::PodConnectionConfigProvider`
# under `DatabaseConfiguration.configure!`) with none of them defined.
# `lower::exclude_predicate` (and `in_predicate`, `including`) ground
# their sites to `include?` forms only while no app class defines the
# same name; Core defines `exclude?` and `in?` on its own classes, so
# its sites reach these methods as written. Counted in the unit-1000
# emit (r2), raw sends: `exclude?` 662, `in?` 592, `index_with` 507,
# `squish` ~800, `deep_dup` ~180, `starts_with?` 149, `in_groups_of` 41.
# ---------------------------------------------------------------------

# core_ext/enumerable.rb
module ActiveSupport
  module EnumerableCoreExt # :nodoc:
    module Constants
      private
        def const_missing(name)
          if name == :SoleItemExpectedError
            ::ActiveSupport::EnumerableCoreExt::SoleItemExpectedError
          else
            super
          end
        end
    end
  end
end

module Enumerable
  # Error generated by +sole+ when called on an enumerable that doesn't have
  # exactly one item.
  class SoleItemExpectedError < StandardError; end

  # HACK: For performance reasons, Enumerable shouldn't have any constants of its own.
  # So we move SoleItemExpectedError into ActiveSupport::EnumerableCoreExt.
  ActiveSupport::EnumerableCoreExt::SoleItemExpectedError = remove_const(:SoleItemExpectedError)
  singleton_class.prepend(ActiveSupport::EnumerableCoreExt::Constants)

  def minimum(key)
    map(&key).min
  end

  def maximum(key)
    map(&key).max
  end

  def index_by
    if block_given?
      result = {}
      each { |elem| result[yield(elem)] = elem }
      result
    else
      to_enum(:index_by) { size if respond_to?(:size) }
    end
  end

  def index_with(default = (no_default = true))
    if block_given?
      to_h { |elem| [elem, yield(elem)] }
    elsif no_default
      to_enum(:index_with) { size if respond_to?(:size) }
    else
      to_h { |elem| [elem, default] }
    end
  end

  def many?
    cnt = 0
    if block_given?
      any? do |*args|
        cnt += 1 if yield(*args)
        cnt > 1
      end
    else
      any? { (cnt += 1) > 1 }
    end
  end

  def including(*elements)
    to_a.including(*elements)
  end

  # The inverse of Enumerable#include?. Returns +true+ if the collection
  # does not include the object.
  def exclude?(object)
    !include?(object)
  end

  def excluding(*elements)
    elements.flatten!(1)
    reject { |element| elements.include?(element) }
  end
  alias :without :excluding

  def pluck(*keys)
    if keys.many?
      map { |element| keys.map { |key| element[key] } }
    else
      key = keys.first
      map { |element| element[key] }
    end
  end

  def pick(*keys)
    return if none?

    if keys.many?
      keys.map { |key| first[key] }
    else
      first[keys.first]
    end
  end

  def in_order_of(key, series, filter: true)
    if filter
      group_by(&key).values_at(*series).compact.flatten(1)
    else
      sort_by { |v| series.index(v.public_send(key)) || series.size }
    end
  end

  def sole
    result = nil
    found = false

    each do |*element|
      if found
        raise SoleItemExpectedError, "multiple items found"
      end

      result = element.size == 1 ? element[0] : element
      found = true
    end

    if found
      result
    else
      raise SoleItemExpectedError, "no item found"
    end
  end
end

class Hash
  def compact_blank!
    # use delete_if rather than reject! because it always returns self even if nothing changed
    delete_if { |_k, v| v.blank? }
  end
end

class Array
  def compact_blank!
    # use delete_if rather than reject! because it always returns self even if nothing changed
    delete_if(&:blank?)
  end
end

# core_ext/string/exclude.rb
class String
  # The inverse of String#include?. Returns true if the string
  # does not include the other string.
  def exclude?(string)
    !include?(string)
  end
end

# core_ext/object/inclusion.rb
class Object
  def in?(another_object)
    case another_object
    when Range
      another_object.cover?(self)
    else
      another_object.include?(self)
    end
  rescue NoMethodError
    raise ArgumentError.new("The parameter passed to #in? must respond to #include?")
  end

  def presence_in(another_object)
    in?(another_object) ? self : nil
  end
end

# core_ext/string/starts_ends_with.rb
class String
  alias :starts_with? :start_with?
  alias :ends_with? :end_with?
end

# core_ext/string/access.rb
class String
  def at(position)
    self[position]
  end

  def from(position)
    self[position, length]
  end

  def to(position)
    position += size if position < 0
    self[0, position + 1] || +""
  end

  def first(limit = 1)
    self[0, limit] || raise(ArgumentError, "negative limit")
  end

  def last(limit = 1)
    self[[length - limit, 0].max, limit] || raise(ArgumentError, "negative limit")
  end
end

# core_ext/string/filters.rb
class String
  def squish
    dup.squish!
  end

  def squish!
    gsub!(/([[:space:]]{2,}|[[[:space:]]&&[^ ]])/, " ")
    strip!
    self
  end

  def remove(*patterns)
    dup.remove!(*patterns)
  end

  def remove!(*patterns)
    patterns.each do |pattern|
      gsub! pattern, ""
    end

    self
  end

  def truncate(truncate_to, options = {})
    return dup unless length > truncate_to

    omission = options[:omission] || "..."
    length_with_room_for_omission = truncate_to - omission.length
    return omission.dup if length_with_room_for_omission <= 0
    stop = \
      if options[:separator]
        rindex(options[:separator], length_with_room_for_omission) || length_with_room_for_omission
      else
        length_with_room_for_omission
      end

    +"#{self[0, stop]}#{omission}"
  end

  def truncate_bytes(truncate_to, omission: "…")
    omission ||= ""

    case
    when bytesize <= truncate_to
      dup
    when omission.bytesize > truncate_to
      raise ArgumentError, "Omission #{omission.inspect} is #{omission.bytesize}, larger than the truncation length of #{truncate_to} bytes"
    when omission.bytesize == truncate_to
      omission.dup
    else
      self.class.new.force_encoding(encoding).tap do |cut|
        cut_at = truncate_to - omission.bytesize

        each_grapheme_cluster do |grapheme|
          if cut.bytesize + grapheme.bytesize <= cut_at
            cut << grapheme
          else
            break
          end
        end

        cut << omission
      end
    end
  end

  def truncate_words(words_count, options = {})
    sep = options[:separator] || /\s+/
    sep = Regexp.escape(sep.to_s) unless Regexp === sep
    if self =~ /\A((?>.+?#{sep}){#{words_count - 1}}.+?)#{sep}.*/m
      $1 + (options[:omission] || "...")
    else
      dup
    end
  end
end

# core_ext/string/inquiry.rb (ActiveSupport::StringInquirer is the
# PostgreSQL tree's runtime/active_support_string_inquirer.rb).
class String
  def inquiry
    ActiveSupport::StringInquirer.new(self)
  end
end

# core_ext/array/grouping.rb
class Array
  def in_groups_of(number, fill_with = nil, &block)
    if number.to_i <= 0
      raise ArgumentError,
        "Group size must be a positive integer, was #{number.inspect}"
    end

    if fill_with == false
      collection = self
    else
      # size % number gives how many extra we have;
      # subtracting from number gives how many to add;
      # modulo number ensures we don't add group of just fill.
      padding = (number - size % number) % number
      collection = dup.concat(Array.new(padding, fill_with))
    end

    if block_given?
      collection.each_slice(number, &block)
    else
      collection.each_slice(number).to_a
    end
  end

  def in_groups(number, fill_with = nil, &block)
    if number.to_i <= 0
      raise ArgumentError,
        "Number of groups must be a positive integer, was #{number.inspect}"
    end

    # size.div number gives minor group size;
    # size % number gives how many objects need extra accommodation;
    # each group hold either division or division + 1 items.
    number = number.to_i
    division = size.div number
    modulo = size % number

    # create a new array avoiding dup
    groups = []
    start = 0

    number.times do |index|
      length = division + (modulo > 0 && modulo > index ? 1 : 0)
      groups << last_group = slice(start, length)
      last_group << fill_with if fill_with != false &&
        modulo > 0 && length == division
      start += length
    end

    if block_given?
      groups.each(&block)
    else
      groups
    end
  end

  def split(value = nil, &block)
    arr = dup
    result = []
    if block_given?
      while (idx = arr.index(&block))
        result << arr.shift(idx)
        arr.shift
      end
    else
      while (idx = arr.index(value))
        result << arr.shift(idx)
        arr.shift
      end
    end
    result << arr
  end
end

# core_ext/array/extract.rb
class Array
  def extract!
    return to_enum(:extract!) { size } unless block_given?

    extracted_elements = []

    reject! do |element|
      extracted_elements << element if yield(element)
    end

    extracted_elements
  end
end

# core_ext/array/extract_options.rb
class Hash
  def extractable_options?
    instance_of?(Hash)
  end
end

class Array
  def extract_options!
    if last.is_a?(Hash) && last.extractable_options?
      pop
    else
      {}
    end
  end
end

# core_ext/hash/slice.rb
class Hash
  def slice!(*keys)
    omit = slice(*self.keys - keys)
    hash = slice(*keys)
    hash.default      = default
    hash.default_proc = default_proc if default_proc
    replace(hash)
    omit
  end

  def extract!(*keys)
    keys.each_with_object(self.class.new) { |key, result| result[key] = delete(key) if has_key?(key) }
  end
end

# core_ext/hash/except.rb (`Hash#except` itself is Ruby's).
class Hash
  def except!(*keys)
    keys.each { |key| delete(key) }
    self
  end
end

# core_ext/hash/reverse_merge.rb (HashWithIndifferentAccess above keeps
# its own).
class Hash
  def reverse_merge(other_hash)
    other_hash.merge(self)
  end
  alias_method :with_defaults, :reverse_merge

  def reverse_merge!(other_hash)
    replace(reverse_merge(other_hash))
  end
  alias_method :reverse_update, :reverse_merge!
  alias_method :with_defaults!, :reverse_merge!
end

# core_ext/hash/deep_transform_values.rb
class Hash
  def deep_transform_values(&block)
    _deep_transform_values_in_object(self, &block)
  end

  def deep_transform_values!(&block)
    _deep_transform_values_in_object!(self, &block)
  end

  private
    # Support methods for deep transforming nested hashes and arrays.
    def _deep_transform_values_in_object(object, &block)
      case object
      when Hash
        object.transform_values { |value| _deep_transform_values_in_object(value, &block) }
      when Array
        object.map { |e| _deep_transform_values_in_object(e, &block) }
      else
        yield(object)
      end
    end

    def _deep_transform_values_in_object!(object, &block)
      case object
      when Hash
        object.transform_values! { |value| _deep_transform_values_in_object!(value, &block) }
      when Array
        object.map! { |e| _deep_transform_values_in_object!(e, &block) }
      else
        yield(object)
      end
    end
end

# core_ext/object/duplicable.rb
class Object
  # Can you safely dup this object?
  #
  # False for method objects;
  # true otherwise.
  def duplicable?
    true
  end
end

methods_are_duplicable = begin
  Object.instance_method(:duplicable?).dup
  true
rescue TypeError
  false
end

unless methods_are_duplicable
  class Method
    # Methods are not duplicable:
    #
    #   method(:puts).duplicable? # => false
    #   method(:puts).dup         # => TypeError: allocator undefined for Method
    def duplicable?
      false
    end
  end

  class UnboundMethod
    # Unbound methods are not duplicable:
    #
    #   method(:puts).unbind.duplicable? # => false
    #   method(:puts).unbind.dup         # => TypeError: allocator undefined for UnboundMethod
    def duplicable?
      false
    end
  end
end

require "singleton"

module Singleton
  # Singleton instances are not duplicable:
  #
  #   Class.new.include(Singleton).instance.dup # TypeError (can't dup instance of singleton
  def duplicable?
    false
  end
end

# core_ext/object/deep_dup.rb
class Object
  def deep_dup
    duplicable? ? dup : self
  end
end

class Array
  def deep_dup
    map(&:deep_dup)
  end
end

class Hash
  def deep_dup
    hash = dup
    each_pair do |key, value|
      if ::String === key || ::Symbol === key
        hash[key] = value.deep_dup
      else
        hash.delete(key)
        hash[key.deep_dup] = value.deep_dup
      end
    end
    hash
  end
end

class Module
  def deep_dup
    if name.nil?
      super
    else
      self
    end
  end
end

# core_ext/object/instance_variables.rb
class Object
  def instance_values
    instance_variables.to_h do |ivar|
      [ivar[1..-1].freeze, instance_variable_get(ivar)]
    end
  end

  def instance_variable_names
    instance_variables.map(&:name)
  end
end
