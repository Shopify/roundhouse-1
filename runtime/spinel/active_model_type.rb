# frozen_string_literal: true

# ActiveModel's attribute types and the ActiveRecord::Type names over them,
# ported from activemodel / activerecord 8.2.0.alpha (Shopify/rails
# 5eb8c4c45bee, the version Core pins) for the CRuby and JRuby
# trees: Type::Value, SerializeCastValue, the Immutable / Mutable /
# Numeric / Timezone / TimeValue / AcceptsMultiparameterTime helpers,
# Type::{ImmutableString,String,Integer,BigInteger,Binary,Boolean,DateTime,
# Decimal,Float,Date,Time}, and ActiveRecord::Type::{Value,ImmutableString,String,Integer,
# BigInteger,Binary,Boolean,Decimal,Float} (the same classes), ActiveRecord::Type::Text,
# ActiveRecord::Type::{DateTime,Date,Time} with Internal::Timezone,
# ActiveRecord::Type::Json, and
# PostgreSQL's OID::{Bytea,Decimal,Date,DateTime,Timestamp,Jsonb,Uuid}. An app that builds column types itself needs
# them, for example Core's DeveloperDashboard::FastDev::TablelessSchema,
# which builds `ActiveRecord::Type::Integer.new(limit: 8)` and its
# siblings at load.
#
# Each method follows the 8.2.0.alpha source (activemodel
# lib/active_model/type/{value,serialize_cast_value,immutable_string,
# string,integer,big_integer,binary,boolean,date_time,decimal,float,date,time}.rb and type/helpers/*.rb; activerecord
# lib/active_record/type/{text,date_time,date,time,json}.rb, type/internal/
# timezone.rb and connection_adapters/postgresql/oid/{decimal,date,
# date_time,timestamp,jsonb,uuid}.rb). The compiled models' column writers
# cast through these (runtime `Base._cast_column`). tests/active_model_type_runtime.rb runs the same
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
# - ActiveRecord::Type::Serialized and Coders::ColumnSerializer, the
#   decorator and the `type:` check `serialize` builds over an app coder,
#   are ported (type/serialized.rb, coders/column_serializer.rb; see
#   them below). The JSON and YAML coders are not.
# - PostgreSQL's OID::Bytea (connection_adapters/postgresql/oid/bytea.rb)
#   is ported for the tree's driver, which hands a bytea column back
#   already decoded (PG::BasicTypeMapForResults' bytea decoder): the
#   answer Rails' adapter gives with `decode_bytea` and marks
#   `@ar_pg_bytea_decoded`. So a String it deserializes counts as
#   decoded and is not unescaped again.
# - ActiveRecord::Type::Time::Value is Ruby's DelegateClass(::Time), where
#   Rails builds it with ActiveSupport::Delegation's faster equivalent.
# - Json#serialize uses `ActiveSupport::JSON::Encoding.encode_without_escape`
#   where the tree carries it, else `JSON.generate` over
#   active_support_ext.rb's jsonable value. Json#accessor needs
#   ActiveRecord::Store, which is not ported.
# - The registries are (see the end of this file); the runtime's
#   `type_for_attribute` raises NotImplementedError for a column whose
#   type is not here.
# - Binary::Data#as_json answers String#as_json's value (the string
#   itself) where the tree does not define String#as_json.
# - SerializeCastValue extends its ClassMethods in `included`, which is
#   what ActiveSupport::Concern does for it. Its compatibility flag is
#   computed in place: `ActiveSupport::Ractors.on_main` does exactly that
#   on the main Ractor, and dispatches there only from another Ractor.
# - ActiveRecord's `Type::DEFAULT_TYPE` (`Value.new.freeze`, the
#   registry's fallback) is not ported; `default_value` is.
# - Type::QueryPredicates (type/query_predicates.rb) is ported as the
#   module a type includes (Core's Shopify::NormalizedStringType does):
#   `query_predicates?`, `comparison_expression`, the Decorator modules
#   and `QueryPredicates.type?`. The tree's query building does not ask
#   a type for its comparison expression (natively PredicateBuilder,
#   QueryMethods#order and Arel's ToSql do), so a model attribute whose
#   type rewrites comparisons is not modeled by the queries.

require "bigdecimal/util"
require "date"
require "delegate"

module ActiveModel
  class RangeError < ::RangeError
  end

module Type
  # type.rb: the registry's fallback cast type, one shared Value.
  def self.default_value # :nodoc:
    @default_value ||= Value.new
  end

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
            max_bytesize = max_cast_bytesize
            str = value.bytesize > max_bytesize ? value.byteslice(0, max_bytesize) : value
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

        # Only the first bytes of a string are cast, to bound the work done by
        # +to_i+ on very long input.
        def max_cast_bytesize
          _limit * 4
        end
    end

    class BigInteger < Integer
      MAX_PRECISION = 1000 # :nodoc:
      private_constant :MAX_PRECISION

      def serialize(value) # :nodoc:
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

        value
      end

      def serialize_cast_value(value) # :nodoc:
        value
      end

      def serializable?(value, &)
        true
      end

      private
        def max_value
          ::Float::INFINITY
        end

        # Values are not limited to +limit+ bytes, so bound the cast by the
        # precision (number of digits) instead. Without a precision, fall back
        # to the largest precision databases accept for a numeric column
        # (1000 digits on PostgreSQL). Twice the precision leaves room for a
        # sign or a short slug suffix.
        def max_cast_bytesize
          (precision || MAX_PRECISION) * 2
        end
    end

    class Binary < Value
      def type
        :binary
      end

      def binary?
        true
      end

      def cast(value)
        if value.is_a?(Data)
          value.to_s
        else
          value = super
          value = value.b if ::String === value && value.encoding != Encoding::BINARY
          value
        end
      end

      def serialize(value)
        return if value.nil?
        Data.new(super)
      end

      def changed_in_place?(raw_old_value, value)
        old_value = deserialize(raw_old_value)
        old_value != value
      end

      class Data # :nodoc:
        def initialize(value)
          value = value.to_s
          value = value.b unless value.encoding == Encoding::BINARY
          @value = value
        end

        def to_s
          @value
        end
        alias_method :to_str, :to_s

        # String#as_json (core_ext/object/json) answers the string itself.
        def as_json(options = nil)
          @value.respond_to?(:as_json) ? @value.as_json(options) : @value
        end

        def hex
          @value.unpack1("H*")
        end

        def ==(other)
          other == to_s || super
        end
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

    # activemodel lib/active_model/type/decimal.rb. shopify-money's
    # MoneyColumn::ActiveRecordType and Money::Type subclass it (through
    # ActiveRecord::Type::Decimal) and reach `cast` / `serialize` by super.
    class Decimal < Value
      include Helpers::Immutable
      include Helpers::Numeric
      BIGDECIMAL_PRECISION = 18

      def type
        :decimal
      end

      def type_cast_for_schema(value)
        value.to_s.inspect
      end

      private
        def cast_value(value)
          casted_value = \
            case value
            when ::Float
              convert_float_to_big_decimal(value)
            when ::Numeric
              BigDecimal(value, precision || BIGDECIMAL_PRECISION)
            when ::String
              begin
                value.to_d
              rescue ArgumentError
                BigDecimal(0)
              end
            else
              if value.respond_to?(:to_d)
                value.to_d
              else
                cast_value(value.to_s)
              end
            end

          apply_scale(casted_value)
        end

        def convert_float_to_big_decimal(value)
          if precision
            BigDecimal(apply_scale(value), float_precision)
          else
            value.to_d
          end
        end

        def float_precision
          if precision.to_i > ::Float::DIG + 1
            ::Float::DIG + 1
          else
            precision.to_i
          end
        end

        def apply_scale(value)
          if scale
            value.round(scale)
          else
            value
          end
        end
    end

    # activemodel lib/active_model/type/float.rb.
    class Float < Value
      include Helpers::Numeric

      def type
        :float
      end

      def type_cast_for_schema(value)
        return "::Float::NAN" if value.respond_to?(:nan?) && value.nan?

        case value
        when ::Float::INFINITY then "::Float::INFINITY"
        when -::Float::INFINITY then "-::Float::INFINITY"
        else super
        end
      end

      private
        def cast_value(value)
          case value
          when ::Float then value
          when "Infinity" then ::Float::INFINITY
          when "-Infinity" then -::Float::INFINITY
          when "NaN" then ::Float::NAN
          else value.to_f
          end
        end
    end

    # activemodel lib/active_model/type/date.rb.
    class Date < Value
      include Helpers::Timezone
      include Helpers::AcceptsMultiparameterTime.new

      def type
        :date
      end

      # `value.to_fs(:db)` is a Date's ISO 8601 text.
      def type_cast_for_schema(value)
        value.iso8601.inspect
      end

      private
        def cast_value(value)
          if value.is_a?(::String)
            return if value.empty?
            fast_string_to_date(value) || fallback_string_to_date(value)
          elsif value.respond_to?(:to_date)
            value.to_date
          else
            value
          end
        end

        ISO_DATE = /\A(\d{4})-(\d\d)-(\d\d)\z/
        def fast_string_to_date(string)
          if string =~ ISO_DATE
            new_date $1.to_i, $2.to_i, $3.to_i
          end
        end

        def fallback_string_to_date(string)
          parts = begin
            ::Date._parse(string, false)
          rescue ArgumentError
          end

          new_date(*parts.values_at(:year, :mon, :mday)) if parts
        end

        def new_date(year, mon, mday)
          unless year.nil? || (year == 0 && mon == 0 && mday == 0)
            ::Date.new(year, mon, mday) rescue nil
          end
        end

        def value_from_multiparameter_assignment(*)
          time = super
          time && new_date(time.year, time.mon, time.mday)
        end
    end

    # activemodel lib/active_model/type/time.rb: a time of day, on
    # 2000-01-01. `value.present?` and `Time#change(year:, month:, day:)`
    # are the private helpers' answers (see the header).
    class Time < Value
      include Helpers::Timezone
      include Helpers::AcceptsMultiparameterTime.new(
        defaults: { 1 => 2000, 2 => 1, 3 => 1, 4 => 0, 5 => 0 }
      )
      include Helpers::TimeValue

      def type
        :time
      end

      def user_input_in_time_zone(value)
        return if Blank.blank?(value)

        case value
        when ::String
          value = "2000-01-01 #{value}"
          time_hash = begin
            ::Date._parse(value)
          rescue ArgumentError
          end

          return if time_hash.nil? || time_hash[:hour].nil?
        when ::Time
          value = change_date(value)
        end

        super(value)
      end

      private
        def cast_value(value)
          return apply_seconds_precision(value) unless value.is_a?(::String)
          return if Blank.blank?(value)

          dummy_time_value = value.sub(/\A\d{4}-\d\d-\d\d(?:T|\s)|/, "2000-01-01 ")

          fast_string_to_time(dummy_time_value) || begin
            time_hash = begin
              ::Date._parse(dummy_time_value)
            rescue ArgumentError
            end

            return if time_hash.nil? || time_hash[:hour].nil?
            new_time(*time_hash.values_at(:year, :mon, :mday, :hour, :min, :sec, :sec_fraction, :offset))
          end
        end

        # `value.change(year: 2000, day: 1, month: 1)`
        # (core_ext/time/calculations.rb): the same clock time and offset.
        def change_date(value)
          return value.change(year: 2000, day: 1, month: 1) if value.respond_to?(:change)
          sec = value.sec + value.subsec
          value.utc? ? ::Time.utc(2000, 1, 1, value.hour, value.min, sec) : ::Time.new(2000, 1, 1, value.hour, value.min, sec, value.utc_offset)
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
    BigInteger = ActiveModel::Type::BigInteger
    Binary = ActiveModel::Type::Binary
    Decimal = ActiveModel::Type::Decimal
    Float = ActiveModel::Type::Float
    ImmutableString = ActiveModel::Type::ImmutableString
    String = ActiveModel::Type::String
    Value = ActiveModel::Type::Value

    # activerecord/lib/active_record/type/decimal_without_scale.rb: the
    # type PostgreSQL's `numeric(p)` / `numeric(p,0)` column gets
    # (postgresql_adapter.rb initialize_type_map), an integer that
    # reports itself a decimal.
    class DecimalWithoutScale < ActiveModel::Type::BigInteger # :nodoc:
      def type
        :decimal
      end

      def type_cast_for_schema(value)
        value.to_s.inspect
      end
    end

    class DateTime < ActiveModel::Type::DateTime
      include Internal::Timezone
    end

    class Date < ActiveModel::Type::Date
      include Internal::Timezone
    end

    class Text < ActiveModel::Type::String # :nodoc:
      def type
        :text
      end
    end

    # type/time.rb: a time of day in the adapter's zone. A serialized
    # value is wrapped as `Time::Value` (ActiveSupport::Delegation's
    # DelegateClass(::Time); Ruby's here), which the adapter quotes as a
    # time of day.
    class Time < ActiveModel::Type::Time
      include Internal::Timezone

      Value = DelegateClass(::Time) # :nodoc:

      def serialize(value)
        case value = super
        when ::Time
          Value.new(value)
        else
          value
        end
      end

      def serialize_cast_value(value) # :nodoc:
        Value.new(super) if value
      end

      private
        def cast_value(value)
          case value = super
          when Value
            value.__getobj__
          else
            value
          end
        end
    end

    # type/json.rb: a json column. Values encode through Active Support
    # without HTML escaping and decode back; text that is not JSON is
    # reported and reads as nil.
    class Json < ActiveModel::Type::Value
      include ActiveModel::Type::Helpers::Mutable

      def type
        :json
      end

      def deserialize(value)
        return value unless value.is_a?(::String)
        begin
          ActiveSupport::JSON.decode(value)
        rescue ::JSON::ParserError => e
          ActiveSupport.error_reporter.report(e, source: "application.active_record")
          nil
        end
      end

      # `ActiveSupport::JSON::Encoding.encode_without_escape`. A tree
      # without runtime/active_support_json.rb has only
      # active_support_ext.rb's encoder, whose output is HTML-escaped;
      # its unescaped form is `JSON.generate` over the same jsonable value.
      def serialize(value)
        return if value.nil?
        encoding = (ActiveSupport::JSON::Encoding if defined?(ActiveSupport::JSON::Encoding))
        return encoding.encode_without_escape(value) if encoding.respond_to?(:encode_without_escape)
        ::JSON.generate(ActiveSupport::JSON.jsonable(value))
      end

      def changed_in_place?(raw_old_value, new_value)
        deserialize(raw_old_value) != new_value
      end

      def accessor
        unless defined?(ActiveRecord::Store::StringKeyedHashAccessor)
          raise NotImplementedError, "ActiveRecord::Type::Json#accessor: ActiveRecord::Store::StringKeyedHashAccessor is not ported"
        end
        ActiveRecord::Store::StringKeyedHashAccessor
      end
    end

    module QueryPredicates
      def query_predicates? # :nodoc:
        true
      end

      # Converts a stored attribute or serialized query value into the
      # expression on which comparisons are performed.
      def comparison_expression(expression)
        expression
      end

      # Include in a representation-preserving type decorator that exposes its
      # wrapped type as `subtype`.
      module Decorator
        include QueryPredicates

        def query_predicates?
          QueryPredicates.type?(subtype)
        end

        def comparison_expression(expression)
          subtype.comparison_expression(expression)
        end
      end

      module NormalizedValueTypeDecorator # :nodoc:
        include Decorator

        def subtype
          cast_type
        end
      end

      def self.type?(type) # :nodoc:
        self === type && type.query_predicates?
      end
    end
  end
end

module ActiveRecord
  module Type
    # ActiveSupport::Delegation::DelegateClass(ActiveModel::Type::Value),
    # which Serialized is built on: every public method of Value, and
    # `to_s`, `hash` and `<=>`, answers the wrapped type's, as does any
    # other method the wrapped type answers. `==`, `!=` and `eql?` are
    # Delegator's.
    class SerializedDelegator # :nodoc:
      def initialize(obj)
        @delegate_dc_obj = obj
      end

      def __getobj__
        @delegate_dc_obj
      end

      def ==(obj)
        return true if obj.equal?(self)
        @delegate_dc_obj == obj
      end

      def !=(obj)
        return false if obj.equal?(self)
        @delegate_dc_obj != obj
      end

      def eql?(obj)
        return true if obj.equal?(self)
        obj.eql?(@delegate_dc_obj)
      end

      (ActiveModel::Type::Value.public_instance_methods - ::Object.public_instance_methods + %i[to_s hash <=>] - %i[== != eql?]).each do |name|
        define_method(name) { |*args, **kwargs, &block| @delegate_dc_obj.__send__(name, *args, **kwargs, &block) }
      end

      def respond_to_missing?(name, include_private = false)
        @delegate_dc_obj.respond_to?(name, include_private) || super
      end

      def method_missing(name, *args, **kwargs, &block)
        return super unless @delegate_dc_obj.respond_to?(name)
        @delegate_dc_obj.__send__(name, *args, **kwargs, &block)
      end
    end

    # type/serialized.rb (8.2.0.alpha): the type `serialize` decorates a
    # column's type with. Reads load the subtype's value through the
    # coder, writes dump it; a value equal to the coder's load of nil
    # (its default) stores nil and reads back as itself. `accessor`
    # (ActiveRecord::Store's IndifferentHashAccessor) is not ported.
    class Serialized < SerializedDelegator # :nodoc:
      include ActiveModel::Type::Helpers::Mutable
      include QueryPredicates::Decorator

      attr_reader :subtype, :coder

      # What `owner.serialize(attr_name, coder:, type:, comparable:)`
      # registers (attribute_methods/serialization.rb): the column
      # serializer, built at the call (`Coders.column_serializer`), and
      # the `decorate_attributes` block, `call`, which wraps the column's
      # cast type, unwrapping an inherited serialization first. The
      # runtime's `serialize` applies it when the model's types are first
      # read.
      class Decoration # :nodoc:
        def initialize(owner, attr_name, coder, type, comparable)
          @column_serializer = Coders.column_serializer(owner, attr_name, coder, type)
          @comparable = comparable
        end

        def call(cast_type)
          cast_type = cast_type.subtype if Serialized === cast_type
          Serialized.new(cast_type, @column_serializer, comparable: @comparable)
        end
      end

      def initialize(subtype, coder, comparable: false)
        @subtype = subtype
        @coder = coder
        @comparable = comparable
        super(subtype)
      end

      def deserialize(value)
        if default_value?(value)
          value
        else
          coder.load(super)
        end
      end

      def serialize(value)
        return if value.nil?
        unless default_value?(value)
          super coder.dump(value)
        end
      end

      define_method(:inspect, Kernel.instance_method(:inspect))

      def changed_in_place?(raw_old_value, value)
        return false if value.nil?

        if @comparable
          old_value = deserialize(raw_old_value)
          old_value != value
        else
          raw_new_value = encoded(value)
          raw_old_value.nil? != raw_new_value.nil? ||
            subtype.changed_in_place?(raw_old_value, raw_new_value)
        end
      end

      def assert_valid_value(value)
        if coder.respond_to?(:assert_valid_value)
          coder.assert_valid_value(value, action: "serialize")
        end
      end

      def force_equality?(value)
        coder.respond_to?(:object_class) && value.is_a?(coder.object_class)
      end

      def serialized? # :nodoc:
        true
      end

      private
        def default_value?(value)
          value == coder.load(nil)
        end

        def encoded(value)
          return if default_value?(value)
          payload = coder.dump(value)
          if payload && @subtype.binary?
            ActiveModel::Type::Binary::Data.new(payload)
          else
            payload
          end
        end
    end
  end

  module Coders
    # coders/column_serializer.rb (8.2.0.alpha): an app coder behind the
    # `type:` check `serialize` adds when given a class other than Object.
    class ColumnSerializer # :nodoc:
      attr_reader :object_class
      attr_reader :coder

      def initialize(attr_name, coder, object_class = Object)
        @attr_name = attr_name
        @object_class = object_class
        @coder = coder
        check_arity_of_constructor
      end

      def init_with(coder) # :nodoc:
        @attr_name = coder["attr_name"]
        @object_class = coder["object_class"]
        @coder = coder["coder"]
      end

      def dump(object)
        return if object.nil?

        assert_valid_value(object, action: "dump")
        coder.dump(object)
      end

      def load(payload)
        if payload.nil?
          if @object_class != ::Object
            return @object_class.new
          end
          return nil
        end

        object = coder.load(payload)

        assert_valid_value(object, action: "load")
        object ||= object_class.new if object_class != Object

        object
      end

      def assert_valid_value(object, action:)
        unless object.nil? || object_class === object
          raise SerializationTypeMismatch,
            "can't #{action} `#{@attr_name}`: was supposed to be a #{object_class}, but was a #{object.class}. -- #{object.inspect}"
        end
      end

      private
        def check_arity_of_constructor
          load(nil)
        rescue ArgumentError
          raise ArgumentError, "Cannot serialize #{object_class}. Classes passed to `serialize` must have a 0 argument constructor."
        end
    end

    # coders/json.rb (8.2.0.alpha): what `coder: JSON` serializes
    # through, Active Support's encoder without HTML escaping, and a
    # blank payload loads as nil.
    class JSON # :nodoc:
      DEFAULT_ENCODE_OPTIONS = { escape: false }.freeze

      def initialize(encode_options: nil, decode_options: nil)
        @encode_options = encode_options ? DEFAULT_ENCODE_OPTIONS.merge(encode_options) : DEFAULT_ENCODE_OPTIONS
        @decode_options = decode_options
      end

      # A tree without runtime/active_support_json.rb has only
      # active_support_ext.rb's one-argument encoder.
      def dump(obj)
        return ActiveSupport::JSON.encode(obj) if ActiveSupport::JSON.method(:encode).arity == 1
        ActiveSupport::JSON.encode(obj, @encode_options)
      end

      def load(json)
        return if json.nil? || json.to_s.strip.empty?
        return ActiveSupport::JSON.decode(json) unless @decode_options
        ActiveSupport::JSON.decode(json, @decode_options)
      end
    end

    # attribute_methods/serialization.rb's `build_column_serializer` for
    # `owner.serialize(attr_name, coder:, type:)`; `coder: JSON` goes
    # through Active Support's encoder (`Coders::JSON`). The YAML coder,
    # and the configured default one a call without `coder:` takes, are
    # not ported: they raise NotImplementedError, naming the call.
    def self.column_serializer(owner, attr_name, coder, type)
      if coder.nil? || (defined?(::YAML) && coder == ::YAML)
        raise NotImplementedError, "#{owner}.serialize(#{attr_name.inspect}, coder: #{coder.inspect}): the default and YAML column coders are not ported"
      end
      coder = Coders::JSON.new if coder == ::JSON || coder == Coders::JSON
      if coder.respond_to?(:new) && !coder.respond_to?(:load)
        coder.new(attr_name, type)
      elsif type && type != Object
        ColumnSerializer.new(attr_name, coder, type)
      else
        coder
      end
    end
  end
end

module ActiveRecord
  # activerecord/lib/active_record/enum.rb: the type `enum` decorates an
  # attribute's type with (`decorate_attributes([name])`), which maps a
  # label to the stored value on the way to the database and back. Only
  # EnumType is ported; the `enum` macro itself is lowered by the compiler.
  module Enum
    class EnumType < ActiveRecord::Type::Value # :nodoc:
      include ActiveRecord::Type::QueryPredicates::Decorator

      def initialize(name, mapping, subtype, raise_on_invalid_values: true)
        @name = name
        @mapping = mapping
        @subtype = subtype
        @_raise_on_invalid_values = raise_on_invalid_values
      end

      def type
        subtype.type
      end

      def cast(value)
        if mapping.has_key?(value)
          value.to_s
        elsif mapping.has_value?(value)
          mapping.key(value)
        else
          ActiveModel::Type.const_get(:Blank).presence(value)
        end
      end

      def deserialize(value)
        mapping.key(subtype.deserialize(value))
      end

      def serialize(value)
        subtype.serialize(mapping.fetch(value, value))
      end

      def serializable?(value, &block)
        subtype.serializable?(mapping.fetch(value, value), &block)
      end

      def assert_valid_value(value)
        return unless @_raise_on_invalid_values

        unless ActiveModel::Type.const_get(:Blank).blank?(value) || mapping.has_key?(value) || mapping.has_value?(value)
          raise ArgumentError, "'#{value}' is not a valid #{name}. Valid values are: #{mapping.keys.map(&:inspect).join(", ")}"
        end
      end

      attr_reader :subtype

      private
        attr_reader :name, :mapping
    end
  end
end

module ActiveRecord
  module ConnectionAdapters
    module PostgreSQL
      module OID # :nodoc:
        # connection_adapters/postgresql/oid/bytea.rb (8.2.0.alpha), the
        # type a PostgreSQL bytea column answers. A String is the
        # driver's decoded value (see the header); Rails dups a marked
        # one only to drop the marker, which the tree's Strings do not
        # carry.
        class Bytea < ActiveModel::Type::Binary # :nodoc:
          def deserialize(value)
            case value
            when nil
              nil
            when ActiveModel::Type::Binary::Data
              value.to_s
            when ::String
              value
            else
              raise NotImplementedError, "OID::Bytea#deserialize(#{value.class}): only the driver's decoded String is ported"
            end
          end
        end

        # oid/decimal.rb: a numeric column.
        class Decimal < ActiveModel::Type::Decimal # :nodoc:
          def infinity(options = {})
            BigDecimal("Infinity") * (options[:negative] ? -1 : 1)
          end
        end

        # oid/date.rb: a date column; PostgreSQL's infinities and BC dates.
        class Date < ActiveRecord::Type::Date # :nodoc:
          def cast_value(value)
            case value
            when "infinity" then ::Float::INFINITY
            when "-infinity" then -::Float::INFINITY
            when / BC$/
              value = value.sub(/^\d+/) { |year| format("%04d", -year.to_i + 1) }
              super(value.delete_suffix!(" BC"))
            else
              super
            end
          end

          def type_cast_for_schema(value)
            case value
            when ::Float::INFINITY then "::Float::INFINITY"
            when -::Float::INFINITY then "-::Float::INFINITY"
            else super
            end
          end
        end

        # oid/date_time.rb.
        class DateTime < ActiveRecord::Type::DateTime # :nodoc:
          def cast_value(value)
            case value
            when "infinity" then ::Float::INFINITY
            when "-infinity" then -::Float::INFINITY
            when / BC$/
              value = value.sub(/^\d+/) { |year| format("%04d", -year.to_i + 1) }
              super(value.delete_suffix!(" BC"))
            else
              super
            end
          end

          def type_cast_for_schema(value)
            case value
            when ::Float::INFINITY then "::Float::INFINITY"
            when -::Float::INFINITY then "-::Float::INFINITY"
            else super
            end
          end

          protected
            # `PostgreSQLAdapter.datetime_type` is :timestamp unless an
            # app sets it, which the tree does not model.
            def real_type_unless_aliased(real_type)
              real_type == :timestamp ? :datetime : real_type
            end
        end

        # oid/timestamp.rb: a `timestamp without time zone` column.
        class Timestamp < DateTime # :nodoc:
          def type
            real_type_unless_aliased(:timestamp)
          end
        end

        # oid/jsonb.rb: a jsonb column, Type::Json under its own name.
        class Jsonb < ActiveRecord::Type::Json # :nodoc:
          def type
            :jsonb
          end
        end

        # oid/uuid.rb.
        class Uuid < ActiveModel::Type::Value # :nodoc:
          ACCEPTABLE_UUID = %r{\A(\{)?([a-fA-F0-9]{4}-?){8}(?(1)\}|)\z}
          CANONICAL_UUID = %r{\A[0-9a-f]{8}-(?:[0-9a-f]{4}-){3}[0-9a-f]{12}\z}

          alias :serialize :deserialize

          def type
            :uuid
          end

          def changed?(old_value, new_value, _new_value_before_type_cast)
            old_value.class != new_value.class ||
              new_value != old_value
          end

          def changed_in_place?(raw_old_value, new_value)
            raw_old_value.class != new_value.class ||
              new_value != raw_old_value
          end

          private
            def cast_value(value)
              value = value.to_s
              format_uuid(value) if value.match?(ACCEPTABLE_UUID)
            end

            def format_uuid(uuid)
              if uuid.match?(CANONICAL_UUID)
                uuid
              else
                uuid = uuid.delete("{}-").downcase
                "#{uuid[..7]}-#{uuid[8..11]}-#{uuid[12..15]}-#{uuid[16..19]}-#{uuid[20..]}"
              end
            end
        end
      end
    end
  end
end

# The type registries: activemodel type/registry.rb with type.rb's
# `ActiveModel::Type.register` / `lookup`, and activerecord
# type/adapter_specific_registry.rb with type.rb's
# `ActiveRecord::Type.register` / `lookup` / `add_modifier`, so an app's
# initializer can register its own type class (Core's uuid_config.rb:
# `ActiveRecord::Type.register(:model_uuid_as_integer,
# Shopify::ModelUuidAsInteger, adapter: :postgresql)`) and `attribute
# name, :sym` finds it. The built-in names are registered as in type.rb.
module ActiveModel
  module Type
    class Registry # :nodoc:
      def initialize
        @registrations = {}
      end

      def initialize_copy(other)
        @registrations = @registrations.dup
        super
      end

      def register(type_name, klass = nil, &block)
        block ||= proc { |_, *args, **kwargs| klass.new(*args, **kwargs) }
        registrations[type_name] = block
      end

      def lookup(symbol, ...)
        registration = registrations[symbol]

        if registration
          registration.call(symbol, ...)
        else
          raise ArgumentError, "Unknown type #{symbol.inspect}"
        end
      end

      private
        attr_reader :registrations
    end

    @registry = Registry.new

    class << self
      attr_accessor :registry # :nodoc:

      def register(type_name, klass = nil, &block)
        registry.register(type_name, klass, &block)
      end

      def lookup(...) # :nodoc:
        registry.lookup(...)
      end
    end

    register(:big_integer, Type::BigInteger)
    register(:binary, Type::Binary)
    register(:boolean, Type::Boolean)
    register(:date, Type::Date)
    register(:datetime, Type::DateTime)
    register(:decimal, Type::Decimal)
    register(:float, Type::Float)
    register(:immutable_string, Type::ImmutableString)
    register(:integer, Type::Integer)
    register(:string, Type::String)
    register(:time, Type::Time)
  end
end

module ActiveRecord
  module Type
    class AdapterSpecificRegistry # :nodoc:
      def initialize
        @registrations = []
      end

      def initialize_copy(other)
        @registrations = @registrations.dup
      end

      def add_modifier(options, klass, **args)
        registrations << DecorationRegistration.new(options, klass, **args)
      end

      def register(type_name, klass = nil, **options, &block)
        block ||= proc { |_, *args, **kwargs| klass.new(*args, **kwargs) }
        registrations << Registration.new(type_name, block, **options)
      end

      def lookup(symbol, *args, **kwargs)
        registration = find_registration(symbol, *args, **kwargs)

        if registration
          registration.call(self, symbol, *args, **kwargs)
        else
          raise ArgumentError, "Unknown type #{symbol.inspect}"
        end
      end

      private
        attr_reader :registrations

        def find_registration(symbol, *args, **kwargs)
          registrations
            .select { |registration| registration.matches?(symbol, *args, **kwargs) }
            .max
        end
    end

    class Registration # :nodoc:
      def initialize(name, block, adapter: nil, override: nil)
        @name = name
        @block = block
        @adapter = adapter
        @override = override
      end

      def call(_registry, *args, adapter: nil, **kwargs)
        block.call(*args, **kwargs)
      end

      def matches?(type_name, *args, **kwargs)
        type_name == name && matches_adapter?(**kwargs)
      end

      # The message is the source's heredoc-like literal after `squish`.
      def <=>(other)
        if conflicts_with?(other)
          raise TypeConflictError.new("Type #{name} was registered for all adapters, but shadows a native type with the same name for #{other.adapter}")
        end
        priority <=> other.priority
      end

      protected
        attr_reader :name, :block, :adapter, :override

        def priority
          result = 0
          if adapter
            result |= 1
          end
          if override
            result |= 2
          end
          result
        end

        def priority_except_adapter
          priority & 0b111111100
        end

      private
        def matches_adapter?(adapter: nil, **)
          (self.adapter.nil? || adapter == self.adapter)
        end

        def conflicts_with?(other)
          same_priority_except_adapter?(other) &&
            has_adapter_conflict?(other)
        end

        def same_priority_except_adapter?(other)
          priority_except_adapter == other.priority_except_adapter
        end

        def has_adapter_conflict?(other)
          (override.nil? && other.adapter) ||
            (adapter && other.override.nil?)
        end
    end

    class DecorationRegistration < Registration # :nodoc:
      def initialize(options, klass, adapter: nil)
        @options = options
        @klass = klass
        @adapter = adapter
      end

      def call(registry, *args, **kwargs)
        subtype = registry.lookup(*args, **kwargs.except(*options.keys))
        klass.new(subtype)
      end

      def matches?(*args, **kwargs)
        matches_adapter?(**kwargs) && matches_options?(**kwargs)
      end

      def priority
        super | 4
      end

      private
        attr_reader :options, :klass

        def matches_options?(**kwargs)
          options.all? do |key, value|
            kwargs[key] == value
          end
        end
    end

    @registry = AdapterSpecificRegistry.new

    class << self
      attr_accessor :registry # :nodoc:

      def add_modifier(...)
        registry.add_modifier(...)
      end

      def register(type_name, klass = nil, **options, &block)
        registry.register(type_name, klass, **options, &block)
      end

      def lookup(*args, adapter: current_adapter_name, **kwargs) # :nodoc:
        registry.lookup(*args, adapter: adapter, **kwargs)
      end

      def adapter_name_from(model) # :nodoc:
        model.connection_db_config.adapter.to_sym
      end

      private
        def current_adapter_name
          adapter_name_from(ActiveRecord::Base)
        end
    end

    register(:big_integer, Type::BigInteger, override: false)
    register(:binary, Type::Binary, override: false)
    register(:boolean, Type::Boolean, override: false)
    register(:date, Type::Date, override: false)
    register(:datetime, Type::DateTime, override: false)
    register(:decimal, Type::Decimal, override: false)
    register(:float, Type::Float, override: false)
    register(:integer, Type::Integer, override: false)
    register(:immutable_string, Type::ImmutableString, override: false)
    register(:json, Type::Json, override: false)
    register(:string, Type::String, override: false)
    register(:text, Type::Text, override: false)
    register(:time, Type::Time, override: false)
  end

  class TypeConflictError < StandardError # :nodoc:
  end
end
