# frozen_string_literal: true

# `errors.attribute_names`, `to_hash`, `as_json` and `messages`, ported
# from activemodel 8.2.0.alpha (Shopify/rails 5813559f, the version Core
# pins: lib/active_model/errors.rb, `#attribute_names` is
# `@errors.map(&:attribute).uniq.freeze`, `#to_hash(full_messages =
# false)` is `group_by_attribute.transform_values { |errors|
# errors.map(&message_method) }`, `#as_json(options = nil)` is
# `to_hash(options && options[:full_messages])` and `#messages` is that
# Hash with an empty default, frozen; `#add`'s `normalize_arguments`
# symbolizes each attribute) for the CRuby and JRuby trees.
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
# - On the PostgreSQL tree a record over the ActiveModel::Validations
#   port has Rails' ActiveModel::Errors, which answers each read itself.
module ActiveModel
  module ErrorAttributes
    PAIRS = :@__roundhouse_error_attributes
    # A recorded message whose short message is not known.
    UNSPLIT = Object.new.freeze
    EMPTY = [].freeze

    # `errors << message`, recording `attribute` for it, and its short
    # message: what follows `prefix`, when given and the message starts
    # with it. Returns the accumulator, as `<<` does.
    def self.add(errors, attribute, message, prefix = nil)
      errors << message
      pairs = errors.instance_variable_get(PAIRS)
      unless pairs
        pairs = []
        errors.instance_variable_set(PAIRS, pairs)
      end
      short = prefix && message.is_a?(String) && message.start_with?(prefix) ? message[prefix.length..] : UNSPLIT
      pairs << [attribute.to_sym, message, short]
      errors
    end

    # `add` for an attribute held in a local: its message is the bare one
    # on `base` and `"#{ActiveSupport.humanize(attribute.to_s)} #{short}"`
    # otherwise (`lower::errors_add`).
    def self.add_humanized(errors, attribute, message)
      name = attribute.to_s
      add(errors, attribute, message, name == "base" ? "" : "#{ActiveSupport.humanize(name)} ")
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
