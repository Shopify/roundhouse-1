# frozen_string_literal: true

# ActiveModel's attribute types and the ActiveRecord::Type names over them,
# ported from activemodel / activerecord 8.2.0.alpha (Shopify/rails
# 5813559f, the version Core pins) for the CRuby and JRuby
# trees: Type::Value, SerializeCastValue, the Immutable / Mutable /
# Numeric / Timezone / TimeValue / AcceptsMultiparameterTime helpers,
# Type::{ImmutableString,String,Integer,Boolean,DateTime}, and
# ActiveRecord::Type::{Value,ImmutableString,String,Integer,Boolean} (the
# same classes), ActiveRecord::Type::Text, ActiveRecord::Type::DateTime
# with Internal::Timezone. An app that builds column types itself needs
# them, for example Core's DeveloperDashboard::FastDev::TablelessSchema,
# which builds `ActiveRecord::Type::Integer.new(limit: 8)` and its
# siblings at load.
#
# Each method follows the 8.2.0.alpha source (activemodel
# lib/active_model/type/{value,serialize_cast_value,immutable_string,
# string,integer,boolean,date_time}.rb and type/helpers/*.rb; activerecord
# lib/active_record/type/{text,date_time}.rb and type/internal/
# timezone.rb). tests/active_model_type_runtime.rb runs the same
# observations against that native ActiveModel and this file and
# requires them equal.
#
# Differences, all at the edges:
# - The ActiveSupport core extensions the source calls are private
#   helpers here, so the port does not depend on the tree's core_ext
#   overlay: `blank?` / `presence` (core_ext/object/blank.rb, whitespace
#   Strings included), `acts_like?(:time)` (a Time, a DateTime, or an
#   object answering `acts_like_time?`), and `Time#change(nsec:)` (a
#   value without `change` is shifted back by the dropped nanoseconds,
#   keeping its offset).
# - `ActiveRecord.default_timezone` and `Time.zone_default` are read when
#   the tree defines them; otherwise they are their native defaults (:utc,
#   no zone), so every time casts in UTC.
# - The type registries (`ActiveModel::Type.lookup` / `register`,
#   ActiveRecord's AdapterSpecificRegistry) and the other types (Date,
#   Time, Decimal, Float, BigInteger, Binary, Json, Serialized) are not
#   ported. No code the tree carries reaches them yet.
# - SerializeCastValue extends its ClassMethods in `included`, which is
#   what ActiveSupport::Concern does for it. Its compatibility flag is
#   computed in place: `ActiveSupport::Ractors.on_main` does exactly that
#   on the main Ractor, and dispatches there only from another Ractor.
# - ActiveRecord's `Type::DEFAULT_TYPE` (`Value.new.freeze`, the
#   registry's fallback) and the opt-in Type::QueryPredicates (Arel
#   comparison expressions) are not ported with the registries.

require "date"

module ActiveModel
  class RangeError < ::RangeError
  end

  module Type
    module SerializeCastValue # :nodoc:
      module ClassMethods
        def serialize_cast_value_compatible?
          return @serialize_cast_value_compatible if defined?(@serialize_cast_value_compatible)
          @serialize_cast_value_compatible = ancestors.index(instance_method(:serialize_cast_value).owner) <= ancestors.index(instance_method(:serialize).owner)
        end
      end

      module DefaultImplementation
        def serialize_cast_value(value)
          value
        end
      end

      def self.included(klass)
        klass.extend ClassMethods
        klass.include DefaultImplementation unless klass.method_defined?(:serialize_cast_value)
      end

      def self.serialize(type, value)
        # Using `type.equal?(type.itself_if_...)` is a performant way to also
        # ensure that `type` is not just a DelegateClass instance (e.g.
        # ActiveRecord::Type::Serialized) unintentionally delegating
        # SerializeCastValue methods.
        if type.equal?((type.itself_if_serialize_cast_value_compatible rescue nil))
          type.serialize_cast_value(value)
        else
          type.serialize(value)
        end
      end

      def itself_if_serialize_cast_value_compatible
        self if self.class.serialize_cast_value_compatible?
      end

      def initialize(...)
        super
        self.class.serialize_cast_value_compatible? # eagerly compute
      end
    end

    # core_ext/object/blank.rb's answers, for the values a type casts.
    module Blank # :nodoc:
      BLANK_RE = /\A[[:space:]]*\z/

      module_function

      def blank?(value)
        case value
        when ::String
          value.empty? || BLANK_RE.match?(value)
        when ::Numeric, true
          false
        else
          value.respond_to?(:empty?) ? !!value.empty? : !value
        end
      end

      def presence(value)
        blank?(value) ? nil : value
      end
    end
    private_constant :Blank

    class Value
      include SerializeCastValue

      attr_reader :precision, :scale, :limit

      def initialize(precision: nil, limit: nil, scale: nil)
        super()
        @precision = precision
        @scale = scale
        @limit = limit
      end

      def serializable?(value, &)
        true
      end

      def type
      end

      def deserialize(value)
        cast(value)
      end

      def cast(value)
        cast_value(value) unless value.nil?
      end

      def serialize(value)
        value
      end

      def type_cast_for_schema(value) # :nodoc:
        value.inspect
      end

      def binary? # :nodoc:
        false
      end

      def changed?(old_value, new_value, _new_value_before_type_cast)
        old_value != new_value
      end

      def changed_in_place?(raw_old_value, new_value)
        false
      end

      def value_constructed_by_mass_assignment?(_value) # :nodoc:
        false
      end

      def force_equality?(_value) # :nodoc:
        false
      end

      def map(value, &) # :nodoc:
        value
      end

      def ==(other)
        self.class == other.class &&
          precision == other.precision &&
          scale == other.scale &&
          limit == other.limit
      end
      alias eql? ==

      def hash
        [self.class, precision, scale, limit].hash
      end

      def assert_valid_value(_)
      end

      def serialized? # :nodoc:
        false
      end

      def mutable? # :nodoc:
        true
      end

      def as_json(*)
        raise NoMethodError
      end

      def as_schema_json
        { "precision" => precision, "scale" => scale, "limit" => limit }
      end

      def init_from_schema_json(coder, references)
        @precision = coder["precision"]
        @scale = coder["scale"]
        @limit = coder["limit"]
      end

      private
        def cast_value(value) # :doc:
          value
        end
    end

    module Helpers # :nodoc: all
      module Immutable
        def mutable? # :nodoc:
          false
        end
      end

      module Mutable
        def cast(value)
          deserialize(serialize(value))
        end

        def changed_in_place?(raw_old_value, new_value)
          raw_old_value != serialize(new_value)
        end

        def mutable? # :nodoc:
          true
        end
      end

      module Numeric
        def serialize(value)
          cast(value)
        end

        def serialize_cast_value(value)
          value
        end

        def cast(value)
          # Checks whether the value is numeric. Spaceship operator
          # will return nil if value is not numeric.
          value = if value <=> 0
            value
          else
            case value
            when true then 1
            when false then 0
            else Blank.presence(value)
            end
          end

          super(value)
        end

        def changed?(old_value, _new_value, new_value_before_type_cast) # :nodoc:
          (super || number_to_non_number?(old_value, new_value_before_type_cast)) &&
            !equal_nan?(old_value, new_value_before_type_cast)
        end

        private
          def equal_nan?(old_value, new_value)
            (old_value.is_a?(::Float) || (defined?(::BigDecimal) && old_value.is_a?(::BigDecimal))) &&
              old_value.nan? &&
              old_value.instance_of?(new_value.class) &&
              new_value.nan?
          end

          def number_to_non_number?(old_value, new_value_before_type_cast)
            old_value != nil && !new_value_before_type_cast.is_a?(::Numeric) &&
              non_numeric_string?(new_value_before_type_cast.to_s)
          end

          def non_numeric_string?(value)
            # 'wibble'.to_i will give zero, we want to make sure
            # that we aren't marking int zero to string zero as
            # changed.
            !NUMERIC_REGEX.match?(value)
          end

          NUMERIC_REGEX = /\A\s*[+-]?\d/
          private_constant :NUMERIC_REGEX
      end

      module Timezone
        def is_utc?
          if default = (::Time.zone_default if ::Time.respond_to?(:zone_default))
            default.name == "UTC"
          else
            true
          end
        end

        def default_timezone
          is_utc? ? :utc : :local
        end
      end

      class AcceptsMultiparameterTime < Module
        module InstanceMethods
          def serialize(value)
            serialize_cast_value(cast(value))
          end

          def serialize_cast_value(value)
            value
          end

          def cast(value)
            if value.is_a?(Hash)
              value_from_multiparameter_assignment(value)
            else
              super(value)
            end
          end

          def assert_valid_value(value)
            if value.is_a?(Hash)
              value_from_multiparameter_assignment(value)
            else
              super(value)
            end
          end

          def value_constructed_by_mass_assignment?(value)
            value.is_a?(Hash)
          end
        end

        def initialize(defaults: {})
          include InstanceMethods

          define_method(:value_from_multiparameter_assignment) do |values_hash|
            defaults.each do |k, v|
              values_hash[k] ||= v
            end
            return unless values_hash[1] && values_hash[2] && values_hash[3]
            values = values_hash.sort.map!(&:last)
            ::Time.public_send(default_timezone, *values)
          end
          private :value_from_multiparameter_assignment
        end
      end

      module TimeValue
        def serialize_cast_value(value)
          value = apply_seconds_precision(value)

          if acts_like_time?(value)
            if is_utc?
              value = value.getutc if !value.utc?
            else
              value = value.getlocal
            end
          end

          value
        end

        def apply_seconds_precision(value)
          return value unless precision && value.respond_to?(:nsec)

          number_of_insignificant_digits = 9 - precision
          round_power = 10**number_of_insignificant_digits
          rounded_off_nsec = value.nsec % round_power

          if rounded_off_nsec > 0
            change_nsec(value, value.nsec - rounded_off_nsec, rounded_off_nsec)
          else
            value
          end
        end

        def type_cast_for_schema(value)
          value.to_fs(:db).inspect
        end

        def user_input_in_time_zone(value)
          value.in_time_zone
        end

        private
          # `value.acts_like?(:time)` (core_ext/object/acts_like.rb with
          # core_ext/{time,date_time}/acts_like.rb).
          def acts_like_time?(value)
            ::Time === value || ::DateTime === value || value.respond_to?(:acts_like_time?)
          end

          # `value.change(nsec: nsec)` (core_ext/time/calculations.rb): the
          # same time and offset with the fraction replaced.
          def change_nsec(value, nsec, dropped)
            if value.respond_to?(:change)
              value.change(nsec: nsec)
            else
              value - Rational(dropped, 1_000_000_000)
            end
          end

          def new_time(year, mon, mday, hour, min, sec, microsec, offset = nil)
            # Treat 0000-00-00 00:00:00 as nil.
            return if year.nil? || (year == 0 && mon == 0 && mday == 0)

            if offset
              time = ::Time.utc(year, mon, mday, hour, min, sec, microsec) rescue nil
              return unless time

              time -= offset unless offset == 0
              is_utc? ? time : time.getlocal
            elsif is_utc?
              ::Time.utc(year, mon, mday, hour, min, sec, microsec) rescue nil
            else
              ::Time.local(year, mon, mday, hour, min, sec, microsec) rescue nil
            end
          end

          def fast_string_to_time(string)
            return unless string.include?("-") #  Time.new("1234") # => 1234-01-01 00:00:00

            if is_utc?
              ::Time.new(string, in: "UTC")
            else
              ::Time.new(string)
            end
          rescue ArgumentError
            nil
          end
      end
    end

    class ImmutableString < Value
      include Helpers::Immutable

      def initialize(**args)
        @true  = -(args.delete(:true)&.to_s  || "t")
        @false = -(args.delete(:false)&.to_s || "f")
        super
      end

      def type
        :string
      end

      def serialize(value)
        case value
        when ::Numeric, ::Symbol then value.to_s
        when true then @true
        when false then @false
        else
          if defined?(::ActiveSupport::Duration) && ::ActiveSupport::Duration === value
            value.to_s
          else
            super
          end
        end
      end

      def serialize_cast_value(value) # :nodoc:
        value
      end

      def as_schema_json
        super.merge!("true" => @true, "false" => @false)
      end

      def init_from_schema_json(coder, references)
        super
        @true = coder["true"]
        @false = coder["false"]
      end

      private
        def cast_value(value)
          case value
          when true then @true
          when false then @false
          else value.to_s.freeze
          end
        end
    end

    class String < ImmutableString
      def changed_in_place?(raw_old_value, new_value)
        if new_value.is_a?(::String)
          raw_old_value != new_value
        end
      end

      def mutable? # :nodoc:
        true
      end

      def to_immutable_string
        ImmutableString.new(
          true: @true,
          false: @false,
          limit: limit,
          precision: precision,
          scale: scale,
        )
      end

      private
        def cast_value(value)
          case value
          when ::String then ::String.new(value)
          when true then @true
          when false then @false
          else value.to_s
          end
        end
    end

    class Integer < Value
      include Helpers::Immutable
      include Helpers::Numeric

      # Column storage size in bytes.
      # 4 bytes means an integer as opposed to smallint etc.
      DEFAULT_LIMIT = 4

      def initialize(**)
        super
        @max = max_value
        @min = min_value
      end

      def type
        :integer
      end

      def deserialize(value)
        return if Blank.blank?(value)
        value.to_i
      end

      def serialize(value)
        case value
        when ::Integer
          # noop
        when ::String
          int = value.to_i
          if int.zero? && value != "0"
            return if non_numeric_string?(value)
          end
          value = int
        else
          value = super
        end

        if out_of_range?(value)
          raise ActiveModel::RangeError, "#{value} is out of range for #{self.class} with limit #{_limit} bytes"
        end

        value
      end

      def serialize_cast_value(value) # :nodoc:
        if out_of_range?(value)
          raise ActiveModel::RangeError, "#{value} is out of range for #{self.class} with limit #{_limit} bytes"
        end

        value
      end

      def serializable?(value)
        cast_value = cast(value)
        return true unless out_of_range?(cast_value)
        yield cast_value if block_given?
        false
      end

      def init_from_schema_json(coder, references)
        super
        @max = max_value
        @min = min_value
      end

      private
        def out_of_range?(value)
          if @max.nil?
            @max = max_value
            @min = min_value
          end

          value && (@max <= value || @min > value)
        end

        def cast_value(value)
          case value
          when ::Integer
            value
          when ::String
            str = value.bytesize > _limit * 4 ? value.byteslice(0, _limit * 4) : value
            str.to_i rescue nil
          else
            value.to_i rescue nil
          end
        end

        def max_value
          1 << (_limit * 8 - 1) # 8 bits per byte with one bit for sign
        end

        def min_value
          -max_value
        end

        def _limit
          limit || DEFAULT_LIMIT
        end
    end

    class Boolean < Value
      include Helpers::Immutable

      FALSE_VALUES = [
        false, 0,
        "0", :"0",
        "f", :f,
        "F", :F,
        "false", :false,
        "FALSE", :FALSE,
        "off", :off,
        "OFF", :OFF,
      ].to_set.freeze

      def type # :nodoc:
        :boolean
      end

      def serialize(value) # :nodoc:
        cast(value)
      end

      def serialize_cast_value(value) # :nodoc:
        value
      end

      private
        def cast_value(value)
          if value == ""
            nil
          else
            !FALSE_VALUES.include?(value)
          end
        end
    end

    class DateTime < Value
      include Helpers::Timezone
      include Helpers::AcceptsMultiparameterTime.new(
        defaults: { 4 => 0, 5 => 0 }
      )
      include Helpers::TimeValue

      def type
        :datetime
      end

      def mutable? # :nodoc:
        # Time#change(nsec:) and similar mutate in place,
        # so DateTime values are effectively mutable.
        true
      end

      private
        def cast_value(value)
          return apply_seconds_precision(value) unless value.is_a?(::String)
          return if value.empty?

          fast_string_to_time(value) || fallback_string_to_time(value)
        end

        # '0.123456' -> 123456
        # '1.123456' -> 123456
        def microseconds(time)
          time[:sec_fraction] ? (time[:sec_fraction] * 1_000_000).to_i : 0
        end

        def fallback_string_to_time(string)
          time_hash = begin
            ::Date._parse(string)
          rescue ArgumentError
          end
          return unless time_hash

          time_hash[:sec_fraction] = microseconds(time_hash)

          new_time(*time_hash.values_at(:year, :mon, :mday, :hour, :min, :sec, :sec_fraction, :offset))
        end

        def value_from_multiparameter_assignment(values_hash)
          missing_parameters = [1, 2, 3].delete_if { |key| values_hash.key?(key) }
          unless missing_parameters.empty?
            raise ArgumentError, "Provided hash #{values_hash} doesn't contain necessary keys: #{missing_parameters}"
          end
          super
        end
    end
  end
end

module ActiveRecord
  module Type
    module Internal # :nodoc:
      module Timezone
        def initialize(timezone: nil, **kwargs)
          super(**kwargs)
          @timezone = timezone
        end

        def is_utc?
          default_timezone == :utc
        end

        def default_timezone
          @timezone || (ActiveRecord.respond_to?(:default_timezone) ? ActiveRecord.default_timezone : :utc)
        end

        def ==(other)
          super(other) && timezone == other.timezone
        end

        def as_schema_json
          json = super
          json["timezone"] = @timezone
          json
        end

        def init_from_schema_json(coder, references)
          @timezone = coder["timezone"]&.to_sym
          super
        end

        protected
          attr_reader :timezone
      end
    end

    Boolean = ActiveModel::Type::Boolean
    Integer = ActiveModel::Type::Integer
    ImmutableString = ActiveModel::Type::ImmutableString
    String = ActiveModel::Type::String
    Value = ActiveModel::Type::Value

    class DateTime < ActiveModel::Type::DateTime
      include Internal::Timezone
    end

    class Text < ActiveModel::Type::String # :nodoc:
      def type
        :text
      end
    end
  end
end
