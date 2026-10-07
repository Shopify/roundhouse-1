# frozen_string_literal: true

# ActiveSupport::JSON's coder, ported from activesupport 8.2.0.alpha
# (Shopify/rails 5813559f, the version Core pins)
# (lib/active_support/json/{encoding,decoding}.rb and the `as_json`
# table of core_ext/object/json.rb) for the CRuby and JRuby trees.
# It replaces runtime/ruby/active_support_ext.rb's flat-Hash `encode`
# where the messages port (runtime/spinel/active_support_messages.rb)
# is carried: a verifier or encryptor whose serializer is
# `ActiveSupport::JSON`, and every legacy metadata envelope
# (`{"_rails":{"message":…,"exp":…,"pur":…}}`), go through `dump` /
# `encode`, `load` / `decode`. tests/messages_runtime.rb holds both
# to that native ActiveSupport.
#
# Differences, all at the edges:
# - `as_json` is not installed on core classes (no Object / Hash /
#   Time reopen). `Encoding.as_json` answers the same table for the
#   values a message carries: an object that defines its own `as_json`
#   (a record) is asked, everything else follows core_ext/object/json.
#   `to_json` is not rerouted through this encoder.
# - The encoder is JSONGemEncoder. Rails picks JSONGemCoderEncoder when
#   json >= 2.15.2 (Core's bundle has 2.19); the two produce the same
#   text for every value this table covers, a non-finite Float (`null`)
#   included (tests/messages_runtime.rb compares them).
# - `parse_json_times` converts with `Time.zone.parse` as natively; it
#   is off unless the app turns it on (Core sets it false).

require "json"
require "time"
require "date"

module ActiveSupport
  module JSON
    DATE_REGEX = /\A\d{4}-\d{2}-\d{2}\z/
    DATETIME_REGEX = /\A(?:\d{4}-\d{2}-\d{2}|\d{4}-\d{1,2}-\d{1,2}[T \t]+\d{1,2}:\d{2}:\d{2}(\.[0-9]*)?(([ \t]*)Z|[-+]\d{2}?(:\d{2})?)?)\z/

    class << self
      def encode(value, options = nil)
        if options.nil? || options.empty?
          Encoding.encode_without_options(value)
        elsif options == { escape: false }.freeze
          Encoding.encode_without_escape(value)
        else
          Encoding.json_encoder.new(options).encode(value)
        end
      end
      alias_method :dump, :encode

      def decode(json, options = {})
        data = ::JSON.parse(json, **options)

        if ActiveSupport.parse_json_times
          convert_dates_from(data)
        else
          data
        end
      end
      alias_method :load, :decode

      def parse_error
        ::JSON::ParserError
      end

      private
        def convert_dates_from(data)
          case data
          when nil
            nil
          when DATE_REGEX
            begin
              Date.parse(data)
            rescue ArgumentError
              data
            end
          when DATETIME_REGEX
            begin
              Time.zone.parse(data)
            rescue ArgumentError
              data
            end
          when Array
            data.map! { |d| convert_dates_from(d) }
          when Hash
            data.transform_values! do |value|
              convert_dates_from(value)
            end
          else
            data
          end
        end
    end

    module Encoding # :nodoc:
      U2028 = -"\u2028".b
      U2029 = -"\u2029".b

      ESCAPED_CHARS = {
        U2028 => -'\u2028'.b,
        U2029 => -'\u2029'.b,
        ">".b => -'\u003e'.b,
        "<".b => -'\u003c'.b,
        "&".b => -'\u0026'.b,
      }.freeze

      HTML_ENTITIES_REGEX = Regexp.union(*(ESCAPED_CHARS.keys - [U2028, U2029])).freeze
      FULL_ESCAPE_REGEX = Regexp.union(*ESCAPED_CHARS.keys).freeze
      JS_SEPARATORS_REGEX = Regexp.union(U2028, U2029).freeze

      class JSONGemEncoder # :nodoc:
        attr_reader :options

        def initialize(options = nil)
          @options = options.dup.freeze || {}.freeze
        end

        def encode(value)
          unless options.empty?
            value = Encoding.as_json(value, options.dup.freeze)
          end
          json = stringify(jsonify(value))

          return json unless @options.fetch(:escape, true)

          json.force_encoding(::Encoding::BINARY)
          if @options.fetch(:escape_html_entities, Encoding.escape_html_entities_in_json)
            if Encoding.escape_js_separators_in_json
              json.gsub!(FULL_ESCAPE_REGEX, ESCAPED_CHARS)
            else
              json.gsub!(HTML_ENTITIES_REGEX, ESCAPED_CHARS)
            end
          elsif Encoding.escape_js_separators_in_json
            json.gsub!(JS_SEPARATORS_REGEX, ESCAPED_CHARS)
          end
          json.force_encoding(::Encoding::UTF_8)
        end

        private
          def jsonify(value)
            case value
            when String, Integer, Symbol, nil, true, false
              value
            when Numeric
              Encoding.as_json(value)
            when Hash
              result = {}
              value.each do |k, v|
                k = k.to_s unless Symbol === k || String === k
                result[k] = jsonify(v)
              end
              result
            when Array
              value.map { |v| jsonify(v) }
            else
              if defined?(::JSON::Fragment) && ::JSON::Fragment === value
                value
              else
                jsonify Encoding.as_json(value)
              end
            end
          end

          def stringify(jsonified)
            ::JSON.generate(jsonified)
          end
      end

      class << self
        attr_accessor :use_standard_json_time_format
        attr_accessor :escape_html_entities_in_json
        attr_accessor :escape_js_separators_in_json
        attr_accessor :time_precision
        attr_reader :json_encoder

        def json_encoder=(encoder)
          @json_encoder = encoder
          @encoder_without_options = encoder.new.freeze
          @encoder_without_escape = encoder.new(escape: false).freeze
        end

        def encode_without_options(value) # :nodoc:
          @encoder_without_options.encode(value)
        end

        def encode_without_escape(value) # :nodoc:
          @encoder_without_escape.encode(value)
        end

        # `value.as_json(options)` as core_ext/object/json.rb answers it,
        # without reopening the core classes. An object with an `as_json`
        # of its own is asked; the rest follow the core_ext/object/json.rb table.
        def as_json(value, options = nil)
          case value
          when String, true, false, nil
            value
          when Symbol
            value.name
          when Float
            value.finite? ? value : nil
          when Hash
            subset = if options
              if attrs = options[:only]
                value.slice(*Array(attrs))
              elsif attrs = options[:except]
                value.except(*Array(attrs))
              else
                value
              end
            else
              value
            end
            result = {}
            subset.each { |k, v| result[k.to_s] = as_json(v, options) }
            result
          when Array
            value.map { |v| as_json(v, options) }
          else
            if value.respond_to?(:as_json)
              options ? value.as_json(options) : value.as_json
            elsif defined?(::BigDecimal) && ::BigDecimal === value
              value.finite? ? value.to_s : nil
            elsif Numeric === value
              value
            elsif Time === value || DateTime === value
              if use_standard_json_time_format
                value.xmlschema(time_precision)
              elsif DateTime === value
                value.strftime("%Y/%m/%d %H:%M:%S %z")
              else
                %(#{value.strftime("%Y/%m/%d %H:%M:%S")} #{value.strftime("%z")})
              end
            elsif Date === value
              use_standard_json_time_format ? value.strftime("%Y-%m-%d") : value.strftime("%Y/%m/%d")
            elsif Module === value
              value.name
            elsif Regexp === value || Range === value || Exception === value || IO === value
              value.to_s
            elsif (defined?(::URI::Generic) && ::URI::Generic === value) || (defined?(::Pathname) && ::Pathname === value)
              value.to_s
            elsif Struct === value || (defined?(::Data) && ::Data === value)
              as_json(value.to_h, options)
            elsif Enumerable === value
              as_json(value.to_a, options)
            elsif value.respond_to?(:to_hash)
              as_json(value.to_hash, options)
            else
              values = value.instance_variables.to_h { |name| [name[1..-1], value.instance_variable_get(name)] }
              as_json(values, options)
            end
          end
        end
      end

      self.use_standard_json_time_format = true
      self.escape_html_entities_in_json  = true
      self.escape_js_separators_in_json = true
      self.json_encoder = JSONGemEncoder
      self.time_precision = 3
    end
  end

  # The module-level switches json/encoding.rb delegates to
  # JSON::Encoding, and decoding.rb's `parse_json_times`.
  class << self
    def use_standard_json_time_format = JSON::Encoding.use_standard_json_time_format
    def use_standard_json_time_format=(value)
      JSON::Encoding.use_standard_json_time_format = value
    end

    def time_precision = JSON::Encoding.time_precision
    def time_precision=(value)
      JSON::Encoding.time_precision = value
    end

    def escape_html_entities_in_json = JSON::Encoding.escape_html_entities_in_json
    def escape_html_entities_in_json=(value)
      JSON::Encoding.escape_html_entities_in_json = value
    end

    def escape_js_separators_in_json = JSON::Encoding.escape_js_separators_in_json
    def escape_js_separators_in_json=(value)
      JSON::Encoding.escape_js_separators_in_json = value
    end

    def json_encoder = JSON::Encoding.json_encoder
    def json_encoder=(value)
      JSON::Encoding.json_encoder = value
    end

    attr_accessor :parse_json_times
  end
end
