# frozen_string_literal: true

# ActiveSupport::NumberHelper and its converters (Shopify/rails 5813559f4aed,
# 8.2.0.alpha activesupport/lib/active_support/number_helper.rb and
# number_helper/*.rb, in the order the module eager-autoloads them),
# verbatim; Rails' doc comments dropped. The module's `extend
# ActiveSupport::Autoload` / `eager_autoload` block is this file's own
# definitions, and the files' requires (bigdecimal, big_decimal/conversions,
# hash/keys, active_support/i18n, class/attribute, object/blank) are the
# tree's ports of them. Formats and units translate through I18n and Active
# Support's English locale (active_support_i18n.rb).
# `require "active_support/number_helper"` (or core_ext/numeric) from a
# native gem loads it (runtime/features, `project::FRAMEWORK_FEATURE_PORTS`).
#
# The PostgreSQL CRuby tree whose lockfile has i18n only
# (`project::apply_postgres_bootstrap`).

require "bigdecimal"
require "bigdecimal/util"
require_relative "active_support_big_decimal_conversions"
require_relative "active_support_hash_keys"
require_relative "active_support_i18n"
require_relative "active_support_class_attribute"

module ActiveSupport
  module NumberHelper
    extend self

    def number_to_phone(number, options = {})
      NumberToPhoneConverter.convert(number, options)
    end

    def number_to_currency(number, options = {})
      NumberToCurrencyConverter.convert(number, options)
    end

    def number_to_percentage(number, options = {})
      NumberToPercentageConverter.convert(number, options)
    end

    def number_to_delimited(number, options = {})
      NumberToDelimitedConverter.convert(number, options)
    end

    def number_to_rounded(number, options = {})
      NumberToRoundedConverter.convert(number, options)
    end

    def number_to_human_size(number, options = {})
      NumberToHumanSizeConverter.convert(number, options)
    end

    def number_to_human(number, options = {})
      NumberToHumanConverter.convert(number, options)
    end
  end
end

module ActiveSupport
  module NumberHelper
    class NumberConverter # :nodoc:
      class_attribute :namespace

      class_attribute :validate_float

      attr_reader :number, :opts

      DEFAULTS = {
        format: {
          separator: ".",
          delimiter: ",",
          precision: 3,
          significant: false,
          strip_insignificant_zeros: false
        }.freeze,

        currency: {
          format: {
            format: "%u%n",
            negative_format: "-%u%n",
            unit: "$",
            separator: ".",
            delimiter: ",",
            precision: 2,
            significant: false,
            strip_insignificant_zeros: false
          }.freeze
        }.freeze,

        percentage: {
          format: {
            delimiter: "",
            format: "%n%"
          }.freeze
        }.freeze,

        precision: {
          format: {
            delimiter: ""
          }.freeze
        }.freeze,

        human: {
          format: {
            delimiter: "",
            precision: 3,
            significant: true,
            strip_insignificant_zeros: true
          }.freeze,
          storage_units: {
            format: "%n %u",
            units: {
              byte: "Bytes",
              kb: "KB",
              mb: "MB",
              gb: "GB",
              tb: "TB",
              pb: "PB",
              eb: "EB",
              zb: "ZB"
            }.freeze
          }.freeze,
          decimal_units: {
            format: "%n %u",
            units: {
              unit: "",
              thousand: "Thousand",
              million: "Million",
              billion: "Billion",
              trillion: "Trillion",
              quadrillion: "Quadrillion"
            }.freeze
          }.freeze
        }.freeze
      }.freeze

      def self.convert(number, options)
        new(number, options).execute
      end

      def initialize(number, options)
        @number = number
        @opts = options.symbolize_keys
        @options = nil
      end

      def execute
        if !number
          nil
        elsif validate_float? && !valid_bigdecimal
          number
        else
          convert
        end
      end

      private
        def options
          @options ||= format_options.merge(opts)
        end

        def format_options
          default_format_options.merge!(i18n_format_options)
        end

        def default_format_options
          options = DEFAULTS[:format].dup
          options.merge!(DEFAULTS[namespace][:format]) if namespace
          options
        end

        def i18n_format_options
          locale = opts[:locale]
          options = I18n.translate(:'number.format', locale: locale, default: {}).dup

          if namespace
            options.merge!(I18n.translate(:"number.#{namespace}.format", locale: locale, default: {}))
          end

          options
        end

        def translate_number_value_with_default(key, **i18n_options)
          I18n.translate(key, default: default_value(key), scope: :number, **i18n_options)
        end

        def translate_in_locale(key, **i18n_options)
          translate_number_value_with_default(key, locale: options[:locale], **i18n_options)
        end

        def default_value(key)
          key.split(".").reduce(DEFAULTS) { |defaults, k| defaults[k.to_sym] }
        end

        def valid_bigdecimal
          case number
          when Float, Rational
            number.to_d(0)
          when String
            BigDecimal(number, exception: false) unless number.to_s.match?(/[de]/i)
          else
            number.to_d rescue nil
          end
        end
    end
  end
end

module ActiveSupport
  module NumberHelper
    class RoundingHelper # :nodoc:
      attr_reader :options

      def initialize(options)
        @options = options
      end

      def round(number)
        precision = absolute_precision(number)
        return number unless precision

        rounded_number = convert_to_decimal(number).round(precision, options.fetch(:round_mode, :default).to_sym)
        rounded_number.zero? ? rounded_number.abs : rounded_number # prevent showing negative zeros
      end

      def digit_count(number)
        return 1 if number.zero?
        return 1 unless number.respond_to?(:finite?) && number.finite?
        (Math.log10(number.abs) + 1).floor
      end

      private
        def convert_to_decimal(number)
          case number
          when Float, String
            BigDecimal(number.to_s)
          when Rational
            BigDecimal(number, digit_count(number.to_i) + options[:precision])
          else
            number.to_d
          end
        end

        def absolute_precision(number)
          if options[:significant] && options[:precision] && options[:precision] > 0
            options[:precision] - digit_count(convert_to_decimal(number))
          else
            options[:precision]
          end
        end
    end
  end
end

module ActiveSupport
  module NumberHelper
    class NumberToRoundedConverter < NumberConverter # :nodoc:
      self.namespace      = :precision
      self.validate_float = true

      def convert
        helper = RoundingHelper.new(options)
        rounded_number = helper.round(number)

        if precision = options[:precision]
          if options[:significant] && precision > 0
            digits = helper.digit_count(rounded_number)
            precision -= digits
            precision = 0 if precision < 0 # don't let it be negative
          end

          formatted_string =
            if rounded_number.finite?
              s = rounded_number.to_s("F")
              a, b = s.split(".", 2)
              if precision != 0
                b << "0" * precision
                a << "."
                a << b[0, precision]
              end
              a
            else
              "%f" % rounded_number
            end
        else
          formatted_string = rounded_number
        end

        delimited_number = NumberToDelimitedConverter.convert(formatted_string, options)
        format_number(delimited_number)
      end

      private
        def strip_insignificant_zeros
          options[:strip_insignificant_zeros]
        end

        def format_number(number)
          if strip_insignificant_zeros
            escaped_separator = Regexp.escape(options[:separator])
            number.sub(/(#{escaped_separator})(\d*[1-9])?0+\z/, '\1\2').sub(/#{escaped_separator}\z/, "")
          else
            number
          end
        end
    end
  end
end

module ActiveSupport
  module NumberHelper
    class NumberToDelimitedConverter < NumberConverter # :nodoc:
      self.validate_float = true

      def convert
        parts.join(options[:separator])
      end

      private
        def parts
          return [number.to_s] if number.respond_to?(:finite?) && !number.finite?

          left, right = number.to_s.split(".")
          if delimiter_pattern
            left.gsub!(delimiter_pattern) do |digit_to_delimit|
              "#{digit_to_delimit}#{options[:delimiter]}"
            end
          else
            left_parts = []
            sign = left.slice!(0) if left.start_with?("-", "+")
            offset = left.size % 3
            if offset > 0
              left_parts << left[0, offset]
            end

            (left.size / 3).times do |i|
              left_parts << left[offset + (i * 3), 3]
            end

            left = "#{sign}#{left_parts.join(options[:delimiter])}"
          end

          [left, right].compact
        end

        def delimiter_pattern
          options[:delimiter_pattern]
        end
    end
  end
end

module ActiveSupport
  module NumberHelper
    class NumberToHumanConverter < NumberConverter # :nodoc:
      DECIMAL_UNITS = { 0 => :unit, 1 => :ten, 2 => :hundred, 3 => :thousand, 6 => :million, 9 => :billion, 12 => :trillion, 15 => :quadrillion,
        -1 => :deci, -2 => :centi, -3 => :mili, -6 => :micro, -9 => :nano, -12 => :pico, -15 => :femto }.freeze
      INVERTED_DECIMAL_UNITS = DECIMAL_UNITS.invert

      self.namespace      = :human
      self.validate_float = true

      def convert # :nodoc:
        @number = RoundingHelper.new(options).round(number)
        @number = Float(number)

        unless options.key?(:strip_insignificant_zeros)
          options[:strip_insignificant_zeros] = true
        end

        units = opts[:units]
        exponent = calculate_exponent(units)
        @number = number / (10**exponent)

        rounded_number = NumberToRoundedConverter.convert(number, options)
        unit = determine_unit(units, exponent)
        format.gsub("%n", rounded_number).gsub("%u", unit).strip
      end

      private
        def format
          options[:format] || translate_in_locale("human.decimal_units.format")
        end

        def determine_unit(units, exponent)
          exp = DECIMAL_UNITS[exponent]
          case units
          when Hash
            units[exp] || ""
          when String, Symbol
            I18n.translate("#{units}.#{exp}", locale: options[:locale], count: number.to_i)
          else
            translate_in_locale("human.decimal_units.units.#{exp}", count: number.to_i)
          end
        end

        def calculate_exponent(units)
          exponent = number != 0 ? Math.log10(number.abs).floor : 0
          unit_exponents(units).find { |e| exponent >= e } || 0
        end

        def unit_exponents(units)
          case units
          when Hash
            units
          when String, Symbol
            I18n.translate(units.to_s, locale: options[:locale], raise: true)
          when nil
            translate_in_locale("human.decimal_units.units", raise: true)
          else
            raise ArgumentError, ":units must be a Hash or String translation scope."
          end.keys.map { |e_name| INVERTED_DECIMAL_UNITS[e_name] }.sort_by(&:-@)
        end
    end
  end
end

module ActiveSupport
  module NumberHelper
    class NumberToHumanSizeConverter < NumberConverter # :nodoc:
      STORAGE_UNITS = [:byte, :kb, :mb, :gb, :tb, :pb, :eb, :zb].freeze

      self.namespace      = :human
      self.validate_float = true

      def convert
        @number = Float(number)

        unless options.key?(:strip_insignificant_zeros)
          options[:strip_insignificant_zeros] = true
        end

        if smaller_than_base?
          number_to_format = number.to_i.to_s
        else
          human_size = number / (base**exponent)
          number_to_format = NumberToRoundedConverter.convert(human_size, options)
        end
        conversion_format.gsub("%n", number_to_format).gsub("%u", unit)
      end

      private
        def conversion_format
          translate_number_value_with_default("human.storage_units.format", locale: options[:locale], raise: true)
        end

        def unit
          translate_number_value_with_default(storage_unit_key, locale: options[:locale], count: number.to_i, raise: true)
        end

        def storage_unit_key
          key_end = smaller_than_base? ? "byte" : STORAGE_UNITS[exponent]
          "human.storage_units.units.#{key_end}"
        end

        def exponent
          max = STORAGE_UNITS.size - 1
          exp = (Math.log(number.abs) / Math.log(base)).to_i
          exp = max if exp > max # avoid overflow for the highest unit
          exp
        end

        def smaller_than_base?
          number.to_i.abs < base
        end

        def base
          1024
        end
    end
  end
end

module ActiveSupport
  module NumberHelper
    class NumberToPhoneConverter < NumberConverter # :nodoc:
      def convert
        str = country_code(opts[:country_code]).dup
        str << convert_to_phone_number(number.to_s.strip)
        str << phone_ext(opts[:extension])
      end

      private
        def convert_to_phone_number(number)
          if opts[:area_code]
            convert_with_area_code(number)
          else
            convert_without_area_code(number)
          end
        end

        def convert_with_area_code(number)
          default_pattern = /(\d{1,3})(\d{3})(\d{4}$)/
          number.gsub!(regexp_pattern(default_pattern),
                       "(\\1) \\2#{delimiter}\\3")
          number
        end

        def convert_without_area_code(number)
          default_pattern = /(\d{0,3})(\d{3})(\d{4})$/
          number.gsub!(regexp_pattern(default_pattern)) do
            $1.empty? ? "#{$2}#{delimiter}#{$3}" : "#{$1}#{delimiter}#{$2}#{delimiter}#{$3}"
          end
          number
        end

        def delimiter
          opts[:delimiter] || "-"
        end

        def country_code(code)
          code.blank? ? "" : "+#{code}#{delimiter}"
        end

        def phone_ext(ext)
          ext.blank? ? "" : " x #{ext}"
        end

        def regexp_pattern(default_pattern)
          opts.fetch :pattern, default_pattern
        end
    end
  end
end

module ActiveSupport
  module NumberHelper
    class NumberToCurrencyConverter < NumberConverter # :nodoc:
      self.namespace = :currency

      def convert
        format = options[:format]

        number_d = valid_bigdecimal
        if number_d
          if number_d.negative?
            number_d = number_d.abs
            format = options[:negative_format] if options[:precision].nil? || (number_d * 10**options[:precision]) >= 0.5
          end
          number_s = NumberToRoundedConverter.convert(number_d, options)
        else
          number_s = number.to_s.strip
          format = options[:negative_format] if number_s.sub!(/^-/, "")
        end

        format.gsub("%n", number_s).gsub("%u", options[:unit])
      end

      private
        def options
          @options ||= begin
            defaults = default_format_options.merge(i18n_opts)
            defaults[:negative_format] = "-#{opts[:format]}" if opts[:format]
            defaults.merge!(opts)
          end
        end

        def i18n_opts
          i18n = i18n_format_options
          i18n[:negative_format] ||= "-#{i18n[:format]}" if i18n[:format]
          i18n
        end
    end
  end
end

module ActiveSupport
  module NumberHelper
    class NumberToPercentageConverter < NumberConverter # :nodoc:
      self.namespace = :percentage

      def convert
        rounded_number = NumberToRoundedConverter.convert(number, options)
        options[:format].gsub("%n", rounded_number)
      end
    end
  end
end
