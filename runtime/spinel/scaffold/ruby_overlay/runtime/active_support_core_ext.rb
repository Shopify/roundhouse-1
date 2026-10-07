# CRuby-only ActiveSupport core extensions (Object / String / Array).
#
# The reopened-builtin surface the lobsters corpus actually calls —
# ActiveSupport itself ships these as core_ext monkey-patches, so the
# CRuby overlay mirrors that shape. Reopening builtins is overlay-only
# (the shared transpiled runtime can't); strict targets get typed
# lowerings for these when lobsters comes up on them.
#
# Inflection here is the rule subset the corpus words exercise
# (story→stories, category→categories, comment→comments, …), not
# ActiveSupport's full irregular/uncountable tables.

class Object
  # `blank?`/`present?` (core_ext/object/blank.rb). The generic form
  # covers nil (!nil → true), false, numerics, and containers via
  # `empty?`. lobsters calls `present?` ~113 times.
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
  def html_safe?
    true
  end

  def html_safe
    self
  end

  # String#to_s on a subclass returns a plain String copy; safety must
  # survive the `.to_s` coercions the emit sprinkles on helper args.
  def to_s
    self
  end
end

class String
  # AS specializes String#blank? to treat whitespace-only as blank.
  def blank?
    empty? || match?(/\A[[:space:]]*\z/)
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

  # AS `without` / `excluding` (core_ext/enumerable.rb): the receiver
  # minus the named elements. `Relation` already answers both (they are
  # what `Room.opens.without(current)` needs); an Array did not, and a
  # test body holding a materialized association is exactly where the
  # Array form turns up — campfire's `rooms/refreshes_controller_test`
  # writes `@room.messages.without(@old_message)`.
  #
  # Rails compares with `==`; two loads of the same row are different
  # objects here, so this is IDENTITY on a model unless the model
  # defines `==`. Recorded rather than worked around: `Base` documents
  # why it defines no equality protocol.
  def without(*values)
    reject { |x| values.include?(x) }
  end

  alias_method :excluding, :without
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

# ActiveSupport::HashWithIndifferentAccess (Rails 8.1.4,
# active_support/hash_with_indifferent_access.rb): a Hash whose Symbol
# keys are stored as Strings, so `h[:a]` and `h["a"]` read the same
# entry, and whose nested Hashes (also inside Arrays) convert on the way
# in. Ported method for method; the AS core_ext helpers Rails' version
# leans on (`except!`, `symbolize_keys!`, `Hash#reverse_merge`) are
# written out in place. Not ported: `slice!`, `to_options!`,
# `extractable_options?`.
module ActiveSupport
  class HashWithIndifferentAccess < Hash
    def with_indifferent_access
      dup
    end

    def nested_under_indifferent_access
      self
    end

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
