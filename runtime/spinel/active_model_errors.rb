# frozen_string_literal: true

# `errors.attribute_names`, ported from activemodel 8.2.0.alpha
# (Shopify/rails 5813559f, the version Core pins: lib/active_model/
# errors.rb `#attribute_names`, `@errors.map(&:attribute).uniq.freeze`,
# with each attribute symbolized by `#add`'s `normalize_arguments`) for
# the CRuby and JRuby trees.
#
# The emitted record's `errors` is the shared runtime's accumulator, an
# Array of full messages: the validates lowering bakes "Title can't be
# blank" and `errors.add(:title, :blank)` grounds to the same push
# (`lower::errors_add`, `lower::model_to_library::validations`). Rails
# keeps an Error per message, which knows its attribute; the Array does
# not. Where the app reads `attribute_names`, the lowering's pushes are
# written `ActiveModel::ErrorAttributes.add(errors, :attr, message)`
# (`emit::ruby::expr::error_attribute_push`), which appends the message
# as `errors << message` did and records the pair on that Array, and
# `errors.attribute_names` is written `attribute_names(errors)`.
#
# The pairs are matched back to the messages still in the Array when
# read, so a `clear`, a `delete` or a fresh accumulator (`valid?` starts
# one) drops their attributes as Rails' collection would.
#
# Differences, at the edges:
# - A message pushed without an attribute (a hand-written `errors << "x"`,
#   an `errors.add` the lowering left as residue) answers none, where
#   Rails' would name its attribute.
# - `errors.attribute_names` on a receiver that is not the accumulator
#   (an Array the app built) answers the attributes recorded on it,
#   usually none.
# - On the PostgreSQL tree a record over the ActiveModel::Validations
#   port has Rails' ActiveModel::Errors, whose own `attribute_names`
#   answers.
module ActiveModel
  module ErrorAttributes
    PAIRS = :@__roundhouse_error_attributes

    # `errors << message`, recording `attribute` for it. Returns the
    # accumulator, as `<<` does.
    def self.add(errors, attribute, message)
      errors << message
      pairs = errors.instance_variable_get(PAIRS)
      unless pairs
        pairs = []
        errors.instance_variable_set(PAIRS, pairs)
      end
      pairs << [attribute.to_sym, message]
      errors
    end

    # The attributes of the messages in `errors`, in order, each once,
    # frozen: Rails' `@errors.map(&:attribute).uniq.freeze`.
    def self.attribute_names(errors)
      # A record over the ActiveModel::Validations port
      # (runtime/active_model_validations.rb) holds Rails' collection,
      # which answers this itself.
      return errors.attribute_names if defined?(::ActiveModel::Errors) && errors.is_a?(::ActiveModel::Errors)
      pending = (errors.instance_variable_get(PAIRS) || []).dup
      names = []
      errors.each do |message|
        at = pending.index { |pair| pair[1] == message }
        next unless at
        names << pending.delete_at(at)[0]
      end
      names.uniq.freeze
    end
  end
end
