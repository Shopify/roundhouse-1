# frozen_string_literal: true

# `errors.attribute_names`, `to_hash`, `as_json`, `messages`, `details`
# and `merge!`, ported from activemodel 8.2.0.alpha (Shopify/rails 5813559f,
# the version Core pins: lib/active_model/errors.rb, `#attribute_names`
# is `@errors.map(&:attribute).uniq.freeze`, `#to_hash(full_messages =
# false)` is `group_by_attribute.transform_values { |errors|
# errors.map(&message_method) }`, `#as_json(options = nil)` is
# `to_hash(options && options[:full_messages])`, `#messages` is that
# Hash with an empty default, frozen, `#details` is
# `group_by_attribute.transform_values { |errors| errors.map(&:details)
# }` with that default, frozen, `#merge!(other)` imports each of
# `other.errors` as a NestedError unless `other` is this collection;
# `#add`'s `normalize_arguments` symbolizes each attribute) for the
# CRuby and JRuby trees, with an Error per recorded message (`objects`)
# for what `merge!` answers. Rails' collection includes Enumerable over
# `each`, which yields each Error: `errors.map(&:attribute)`,
# `errors.first`, `errors.group_by(&:attribute)` and the like, and
# `errors.errors` / `objects`, are written over
# `ActiveModel::ErrorAttributes.objects(errors)`
# (`expr::errors_enumerable`); a blockless count or emptiness query
# answers the same over the messages and stays on the accumulator.
#
# The emitted record's `errors` is the shared runtime's accumulator, an
# Array of full messages: the validates lowering bakes "Title can't be
# blank" and `errors.add(:title, :blank)` grounds to the same push
# (`lower::errors_add`, `lower::model_to_library::validations`). Rails
# keeps an Error per message, which knows its attribute and its short
# message ("can't be blank"); the Array does not. Where the app reads
# one of these, the lowering's pushes are written
# `ActiveModel::ErrorAttributes.add(errors, :attr, message, "Title ")`
# (`emit::ruby::expr::error_attribute_push`), which appends the message
# as `errors << message` did and records on that Array the attribute and
# the text the lowering put before the short message (Rails' full
# message is `"%{attribute} %{message}"`, the bare message on :base), and
# each read is written `ActiveModel::ErrorAttributes.<read>(errors, ...)`.
#
# The records are matched back to the messages still in the Array when
# read, the very String pushed first and an equal one otherwise, so a
# `clear`, a `delete`, the autosave dedupe or a fresh accumulator
# (`valid?` starts one) drops their attributes as Rails' collection would.
#
# Differences, at the edges:
# - A message pushed without an attribute (a hand-written `errors << "x"`,
#   an `errors.add` the lowering left as residue) answers none from
#   `attribute_names`, where Rails' would name its attribute; `to_hash`,
#   `as_json` and `messages` raise naming it rather than key it wrongly.
# - A message the lowering could not split (no prefix recorded) answers
#   `to_hash(true)`, which keys full messages, and raises from the short
#   reads.
# - A receiver that is not the accumulator (an Array the app built)
#   answers the attributes recorded on it, usually none, from
#   `attribute_names`; any other object answers the read itself.
# - An Error built here is not an ActiveModel::Error and keeps no
#   record (`base` raises, and equality leaves it out). Its type and
#   options are known only where the push recorded them (the validates
#   lowering's checks but uniqueness, a numericality bound and
#   validates_associated; `errors.add` with a type, a String message or
#   `message:` alone); otherwise `raw_type`, `options`, `details`,
#   equality and `merge!` into Rails' collection raise naming the
#   message. Its options are Rails' but the callback and `message:` ones,
#   which `details` drops too.
# - On the PostgreSQL tree a record over the ActiveModel::Validations
#   port has Rails' ActiveModel::Errors, which answers each read itself;
#   `merge!` between it and the accumulator goes through the records.
module ActiveModel
  module ErrorAttributes
    PAIRS = :@__roundhouse_error_attributes
    # A recorded message whose short message is not known.
    UNSPLIT = Object.new.freeze
    # A recorded message whose type or options are not known.
    UNKNOWN = Object.new.freeze
    # The type of a message added as text (`errors.add(:title, "is
    # bad")`): Rails' Error keeps the text as its type.
    SHORT = Object.new.freeze
    EMPTY = [].freeze

    # `errors << message`, recording `attribute` for it, its short
    # message (what follows `prefix`, when given and the message starts
    # with it), and the Error's type and options where the lowering knows
    # them (`expr::ErrorDetail`; SHORT: the short message is the type).
    # Returns the accumulator, as `<<` does.
    def self.add(errors, attribute, message, prefix = nil, type = UNKNOWN, options = UNKNOWN)
      errors << message
      short = prefix && message.is_a?(String) && message.start_with?(prefix) ? message[prefix.length..] : UNSPLIT
      type = short.equal?(UNSPLIT) ? UNKNOWN : short if type.equal?(SHORT)
      pairs_of(errors) << [attribute.to_sym, message, short, type, options]
      errors
    end

    # `add` for an attribute held in a local: its message is the bare one
    # on `base` and `"#{ActiveSupport.humanize(attribute.to_s)} #{short}"`
    # otherwise (`lower::errors_add`).
    def self.add_humanized(errors, attribute, message, type = UNKNOWN, options = UNKNOWN)
      name = attribute.to_s
      add(errors, attribute, message, name == "base" ? "" : "#{ActiveSupport.humanize(name)} ", type, options)
    end

    # The attributes of the messages in `errors`, in order, each once,
    # frozen: Rails' `@errors.map(&:attribute).uniq.freeze`.
    def self.attribute_names(errors)
      # A record over the ActiveModel::Validations port
      # (runtime/active_model_validations.rb) holds Rails' collection,
      # which answers this itself.
      return errors.attribute_names if defined?(::ActiveModel::Errors) && errors.is_a?(::ActiveModel::Errors)
      names = []
      each_record(errors) { |_message, pair| names << pair[0] if pair }
      names.uniq.freeze
    end

    # Rails' `to_hash(full_messages = false)`: the messages by attribute,
    # in the order each attribute first appears, short or full.
    def self.to_hash(errors, full_messages = false)
      return (full_messages ? errors.to_hash(full_messages) : errors.to_hash) unless errors.is_a?(Array)
      hash = {}
      each_record(errors) do |message, pair|
        unless pair
          raise "roundhouse: ActiveModel::Errors#to_hash: #{message.inspect} was added without its attribute " \
                "(a push the errors lowering did not record), so it cannot be keyed"
        end
        text = full_messages ? message : pair[2]
        if text.equal?(UNSPLIT)
          raise "roundhouse: ActiveModel::Errors#to_hash: #{message.inspect} on #{pair[0].inspect} was added " \
                "without its short message (only the full message is known)"
        end
        (hash[pair[0]] ||= []) << text
      end
      hash
    end

    # Rails' `as_json(options = nil)`: `to_hash(options && options[:full_messages])`.
    def self.as_json(errors, options = nil)
      return (options.nil? ? errors.as_json : errors.as_json(options)) unless errors.is_a?(Array)
      to_hash(errors, options && options[:full_messages])
    end

    # Rails' `messages`: `to_hash` with an empty, frozen default, frozen.
    def self.messages(errors)
      return errors.messages unless errors.is_a?(Array)
      hash = to_hash(errors)
      hash.default = EMPTY
      hash.freeze
    end

    # Rails' `details`: each attribute's errors as `{error: type}` merged
    # with their options but the callback and `message:` ones, in the
    # order each attribute first appears, with an empty, frozen default,
    # frozen.
    def self.details(errors)
      return errors.details unless errors.is_a?(Array)
      hash = {}
      each_record(errors) do |message, record|
        error = Error.new(message, record)
        (hash[error.attribute] ||= []) << error.details
      end
      hash.default = EMPTY
      hash.freeze
    end

    # Rails' `merge!(other)`: `other`'s errors appended, each with its
    # attribute, short message, type and options (an ActiveModel::Errors
    # holds NestedErrors over them, whose full message is this record's
    # `"%{attribute} %{message}"`). Answers `other`'s errors, as Rails'
    # `other.errors.each` does.
    def self.merge!(errors, other)
      unless errors.is_a?(Array)
        return errors.merge!(other) unless other.is_a?(Array)
        # A port record's collection (runtime/active_model_validations.rb)
        # taking the accumulator's errors: each added with the type and
        # options its push recorded and its short message, raising naming
        # one whose were not recorded.
        each_record(other) do |message, record|
          raise unkeyed("merge!", message) unless record
          raise unsplit("merge!", message, record) if record[2].equal?(UNSPLIT)
          error = Error.new(message, record)
          errors.add(error.attribute, error.raw_type, **error.options, message: error.message)
        end
        return objects(other)
      end
      return objects(errors) if errors.equal?(other)
      if other.is_a?(Array)
        each_record(other) do |message, record|
          errors << message
          pairs_of(errors) << record.dup if record
        end
      else
        other.errors.each do |error|
          name = error.attribute.to_s
          short = error.message
          prefix = name == "base" ? "" : "#{ActiveSupport.humanize(name)} "
          add(errors, error.attribute, "#{prefix}#{short}", prefix, error.raw_type, error.options)
        end
      end
      objects(other)
    end

    # Rails' `where(attribute, type = nil, **options)`: the Errors on
    # `attribute` (of `type`, with `options`), in order.
    def self.where(errors, attribute, type = nil, **options)
      return errors.where(attribute, type, **options) unless errors.is_a?(Array)
      attribute, type = normalize_arguments("where", attribute, type)
      objects(errors).select { |error| error.match?(attribute, type, **options) }
    end

    # Rails' `include?(attribute)`, `key?` and `has_key?`: whether an
    # error is on `attribute` (ActiveSupport's `exclude?` reaches it as
    # `!include?`, `lower::exclude_predicate`).
    def self.key?(errors, attribute)
      return errors.key?(attribute) unless errors.is_a?(Array)
      attribute = attribute.to_sym
      objects(errors).any? { |error| error.match?(attribute) }
    end

    # Rails' `of_kind?(attribute, type = :invalid)`: an error of a
    # Symbol type on `attribute`, or a message added as text.
    def self.of_kind?(errors, attribute, type = :invalid)
      return errors.of_kind?(attribute, type) unless errors.is_a?(Array)
      attribute, type = normalize_arguments("of_kind?", attribute, type)
      type.is_a?(Symbol) ? !where(errors, attribute, type).empty? : messages_for(errors, attribute).include?(type)
    end

    # Rails' `added?(attribute, type = :invalid, options = {})`: an error
    # of a Symbol type with exactly these options (but the callback and
    # `message:` ones), or a message added as text.
    def self.added?(errors, attribute, type = :invalid, options = {})
      return errors.added?(attribute, type, options) unless errors.is_a?(Array)
      attribute, type = normalize_arguments("added?", attribute, type)
      if type.is_a?(Symbol)
        objects(errors).any? { |error| error.strict_match?(attribute, type, **options) }
      else
        messages_for(errors, attribute).include?(type)
      end
    end

    # Rails' `messages_for(attribute, type = nil)`: the short messages.
    def self.messages_for(errors, attribute, type = nil)
      return errors.messages_for(attribute, type) unless errors.is_a?(Array)
      where(errors, attribute, type).map(&:message)
    end

    # Rails' `full_messages_for(attribute, type = nil)`, frozen.
    def self.full_messages_for(errors, attribute, type = nil)
      return errors.full_messages_for(attribute, type) unless errors.is_a?(Array)
      where(errors, attribute, type).map(&:full_message).freeze
    end

    # Rails' `full_message(attribute, message)`: the bare message on
    # :base, else `"%{attribute} %{message}"` with the attribute
    # humanized as the pushes humanize it (`add_humanized`).
    def self.full_message(errors, attribute, message)
      return errors.full_message(attribute, message) unless errors.is_a?(Array)
      return message if attribute == :base
      "#{ActiveSupport.humanize(attribute.to_s.tr(".", "_"))} #{message}"
    end

    # Rails' `delete(attribute, type = nil, **options)`: removes the
    # matching errors, answering their short messages, or nil for none.
    def self.delete(errors, attribute, type = nil, **options)
      return errors.delete(attribute, type, **options) unless errors.is_a?(Array)
      attribute, type = normalize_arguments("delete", attribute, type)
      matches = []
      each_record(errors) do |message, record|
        error = Error.new(message, record)
        matches << [message, record, error] if error.match?(attribute, type, **options)
      end
      pairs = pairs_of(errors)
      matches.each do |message, record, _error|
        errors.delete_at(errors.index { |m| m.equal?(message) })
        pairs.delete_at(pairs.index { |pair| pair.equal?(record) })
      end
      messages = matches.map { |_message, _record, error| error.message }
      messages.empty? ? nil : messages
    end

    # Rails' `import(error, override_options = {})`: `error` appended as a
    # NestedError, whose attribute and type the options may override,
    # with its raw type, options and short message, and this record's
    # full message for them. Answers the Errors.
    def self.import(errors, error, override_options = {})
      return errors.import(error, override_options) unless errors.is_a?(Array)
      override_options = override_options.dup
      [:attribute, :type].each do |key|
        override_options[key] = override_options[key].to_sym if override_options.key?(key)
      end
      attribute = override_options.fetch(:attribute) { error.attribute }
      raw_type, options = error.is_a?(Error) ? error.recorded_detail : [error.raw_type, error.options]
      short = error.message
      prefix = attribute == :base ? "" : "#{ActiveSupport.humanize(attribute.to_s.tr(".", "_"))} "
      add(errors, attribute, "#{prefix}#{short}", prefix, raw_type, options)
      if override_options.key?(:type)
        pairs_of(errors).last << override_options[:type]
      elsif error.is_a?(Error)
        type = error.recorded_type
        pairs_of(errors).last << type if type
      end
      objects(errors)
    end

    # Rails' `normalize_arguments`: the attribute as a Symbol. A Proc type
    # is called with the record, which the accumulator does not keep.
    def self.normalize_arguments(read, attribute, type)
      if type.respond_to?(:call)
        raise "roundhouse: ActiveModel::Errors##{read}: a type that responds to `call` is called with the record, " \
              "which the emitted record's errors do not keep"
      end
      [attribute.to_sym, type]
    end
    private_class_method :normalize_arguments

    # Rails' `objects` (`errors`): an Error for each message, in order.
    # A message pushed without its attribute is an Error whose readers
    # raise naming it.
    def self.objects(errors)
      return errors.objects unless errors.is_a?(Array)
      out = []
      each_record(errors) { |message, record| out << Error.new(message, record) }
      out
    end

    # One message of the accumulator, as Rails' ActiveModel::Error
    # answers it (activemodel's error.rb): its attribute, type
    # (`raw_type`, `:invalid` when nil), options, short and full message,
    # `details`, `match?` and `strict_match?`, and equality over them.
    # Its record (`base`) is not kept.
    class Error
      CALLBACKS_OPTIONS = [:if, :unless, :on, :allow_nil, :allow_blank, :strict].freeze
      MESSAGE_OPTIONS = [:message].freeze

      def initialize(full_message, record)
        @full = full_message
        @record = record
        return unless record
        @attribute = record[0]
        @type = record[3].equal?(UNKNOWN) ? UNKNOWN : (record[3] || :invalid)
        @options = record[4]
      end

      def attribute
        keyed![0]
      end

      def raw_type
        type = keyed![3]
        raise ErrorAttributes.unrecorded("type", @full, @record) if type.equal?(UNKNOWN)
        type
      end

      # A NestedError's type may differ from its raw type (`import`'s
      # `type:` override, kept sixth on the record).
      def type
        override = keyed![5]
        return override if override
        raw_type || :invalid
      end

      def options
        options = keyed![4]
        raise ErrorAttributes.unrecorded("options", @full, @record) if options.equal?(UNKNOWN)
        options
      end

      def message
        short = keyed![2]
        raise ErrorAttributes.unsplit("Error#message", @full, @record) if short.equal?(UNSPLIT)
        short
      end

      def full_message
        keyed!
        @full
      end

      def base
        raise "roundhouse: ActiveModel::Error#base: the emitted record's errors do not keep their record"
      end

      def details
        { error: raw_type }.merge(options.except(*CALLBACKS_OPTIONS, *MESSAGE_OPTIONS))
      end
      alias_method :detail, :details

      def match?(attribute, type = nil, **options)
        return false if self.attribute != attribute || (type && self.type != type)
        options.each { |key, value| return false if self.options[key] != value }
        true
      end

      def strict_match?(attribute, type, **options)
        return false unless match?(attribute, type)
        options.except(*CALLBACKS_OPTIONS, *MESSAGE_OPTIONS) == self.options.except(*CALLBACKS_OPTIONS, *MESSAGE_OPTIONS)
      end

      def ==(other)
        other.is_a?(Error) && attributes_for_hash == other.attributes_for_hash
      end
      alias_method :eql?, :==

      def hash
        attributes_for_hash.hash
      end

      # The raw type and options as recorded, unknown or not, for an
      # import of this Error to carry.
      def recorded_detail
        [keyed![3], @record[4]]
      end

      # The type override `import` recorded, if any.
      def recorded_type
        keyed![5]
      end

      protected

      def attributes_for_hash
        [attribute, raw_type, options.except(*CALLBACKS_OPTIONS)]
      end

      private

      def keyed!
        raise ErrorAttributes.unkeyed("ActiveModel::Error", @full) unless @record
        @record
      end

      def instance_variables_to_inspect
        [:@attribute, :@type, :@options].freeze
      end
    end

    def self.unkeyed(read, message)
      RuntimeError.new("roundhouse: #{read}: #{message.inspect} was added without its attribute " \
                       "(a push the errors lowering did not record)")
    end

    def self.unsplit(read, message, record)
      RuntimeError.new("roundhouse: #{read}: #{message.inspect} on #{record[0].inspect} was added " \
                       "without its short message (only the full message is known)")
    end

    def self.unrecorded(what, message, record)
      RuntimeError.new("roundhouse: ActiveModel::Error: the #{what} of #{message.inspect} on #{record[0].inspect} " \
                       "was not recorded by the push that added it")
    end

    # The records kept on an accumulator, created on first use.
    def self.pairs_of(errors)
      pairs = errors.instance_variable_get(PAIRS)
      unless pairs
        pairs = []
        errors.instance_variable_set(PAIRS, pairs)
      end
      pairs
    end
    private_class_method :pairs_of

    # Each message in `errors` with the record pushed for it, or nil:
    # the very String first, else an equal one, each record used once.
    def self.each_record(errors)
      pending = (errors.instance_variable_get(PAIRS) || []).dup
      errors.each do |message|
        at = pending.index { |pair| pair[1].equal?(message) } || pending.index { |pair| pair[1] == message }
        yield message, (at && pending.delete_at(at))
      end
    end
    private_class_method :each_record
  end
end
