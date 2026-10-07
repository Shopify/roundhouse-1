# frozen_string_literal: true

# ActiveModel::Validator, EachValidator and BlockValidator, ported from
# activemodel 8.2.0.alpha (Shopify/rails 5813559f, the version Core pins,
# lib/active_model/validator.rb) for the CRuby and JRuby trees. Core
# subclasses these 130-odd times (app/validators/*, `validates_with`
# targets, `validates :attr, <kind>: ...` validators), and before this
# port every subclass was dropped at emit as an unported Rails base.
#
# Each method follows the 8.2.0.alpha source. tests/active_model_validator_runtime.rs
# runs the same observations against native ActiveModel and this file
# and requires them equal.
#
# Differences, all at the edges:
# - `value.blank?` (core_ext/object/blank.rb) is a private helper with
#   the same answers, so the port does not depend on the tree's core_ext
#   overlay; `kind` underscores with the tree's String#underscore when
#   it has one, and ActiveSupport.underscore otherwise.
# - EachValidator reads the attribute with
#   `record.read_attribute_for_validation` when the record answers it,
#   which every ActiveModel::Validations includer does natively (an
#   alias of `send`), and with `send` otherwise, which is what that alias
#   does. The runtime's models do not define the alias.
module ActiveModel
  class Validator
    attr_reader :options

    # Returns the kind of the validator: `PresenceValidator.kind` is
    # `:presence`, an anonymous class answers nil.
    def self.kind
      unless name.nil?
        word = name.split("::").last
        @kind ||= (word.respond_to?(:underscore) ? word.underscore : ActiveSupport.underscore(word)).chomp("_validator").to_sym
      end
    end

    # Accepts options that will be made available through the +options+ reader.
    def initialize(options = {})
      @options = options.except(:class).freeze
    end

    # Returns the kind for this validator.
    def kind
      self.class.kind
    end

    # Override this method in subclasses with validation logic, adding errors
    # to the records +errors+ array where necessary.
    def validate(record)
      raise NotImplementedError, "Subclasses must implement a validate(record) method."
    end
  end

  # EachValidator is a validator which iterates through the attributes given
  # in the options hash invoking the validate_each method passing in the
  # record, attribute, and value.
  class EachValidator < Validator
    BLANK_RE = /\A[[:space:]]*\z/
    private_constant :BLANK_RE

    attr_reader :attributes

    # Returns a new validator instance. All options will be available via the
    # +options+ reader, however the <tt>:attributes</tt> option will be removed
    # and instead be made available through the +attributes+ reader.
    def initialize(options)
      @attributes = Array(options.delete(:attributes))
      raise ArgumentError, ":attributes cannot be blank" if @attributes.empty?
      super
      check_validity!
    end

    # Performs validation on the supplied record. By default this will call
    # +validate_each+ to determine validity therefore subclasses should
    # override +validate_each+ with validation logic.
    def validate(record)
      attributes.each do |attribute|
        value = if record.respond_to?(:read_attribute_for_validation)
          record.read_attribute_for_validation(attribute)
        else
          record.send(attribute)
        end
        next if (value.nil? && allow_nil?(record)) || (validator_blank?(value) && allow_blank?(record))
        value = prepare_value_for_validation(value, record, attribute)
        validate_each(record, attribute, value)
      end
    end

    # Override this method in subclasses with the validation logic, adding
    # errors to the records +errors+ array where necessary.
    def validate_each(record, attribute, value)
      raise NotImplementedError, "Subclasses must implement a validate_each(record, attribute, value) method"
    end

    # Hook method that gets called by the initializer allowing verification
    # that the arguments supplied are valid. You could for example raise an
    # +ArgumentError+ when invalid options are supplied.
    def check_validity!
    end

    private
      def prepare_value_for_validation(value, record, attr_name)
        value
      end

      def allow_nil?(record)
        if options[:allow_nil].respond_to?(:to_proc)
          options[:allow_nil].to_proc.call(record)
        else
          options[:allow_nil]
        end
      end

      def allow_blank?(record)
        if options[:allow_blank].respond_to?(:to_proc)
          options[:allow_blank].to_proc.call(record)
        else
          options[:allow_blank]
        end
      end

      # core_ext/object/blank.rb's answers.
      def validator_blank?(value)
        case value
        when ::String
          value.empty? || BLANK_RE.match?(value)
        when ::Numeric, true
          false
        else
          value.respond_to?(:empty?) ? !!value.empty? : !value
        end
      end
  end

  # +BlockValidator+ is a special +EachValidator+ which receives a block on initialization
  # and call this block for each attribute being validated. +validates_each+ uses this validator.
  class BlockValidator < EachValidator # :nodoc:
    def initialize(options, &block)
      @block = block
      super
    end

    private
      def validate_each(record, attribute, value)
        @block.call(record, attribute, value)
      end
  end
end
