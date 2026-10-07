# frozen_string_literal: true

# ActiveSupport's signed and encrypted messages, ported from activesupport
# 8.1.4 for the CRuby and JRuby trees: SecurityUtils, KeyGenerator and
# CachingKeyGenerator, Messages::Metadata, Messages::SerializerWithFallback,
# Messages::Codec, Messages::Rotator, MessageVerifier, MessageEncryptor and
# SecureCompareRotator. Real OpenSSL does the HMAC, the ciphers and PBKDF2.
#
# Each method follows the 8.1.4 source (lib/active_support/{security_utils,
# key_generator,message_verifier,message_encryptor,secure_compare_rotator}.rb
# and messages/{metadata,serializer_with_fallback,codec,rotator}.rb);
# tests/messages_runtime.rb runs the same observations against native
# ActiveSupport and this file, decrypts each side's messages on the other,
# and requires them equal. Core pins Shopify/rails 5813559f (8.2.0.alpha);
# its copies of these files differ from 8.1.4 only in `inspect` (the
# InspectBackport), Ractor-shareable key caches and Metadata's serializer
# lists becoming frozen class attributes, none of which a message sees.
#
# runtime/spinel/active_support_json.rb (ActiveSupport::JSON's coder) is
# required first: the legacy metadata envelope and an
# `ActiveSupport::JSON` serializer go through it.
#
# Differences, all at the edges:
# - Base64 is the `pack("m0")` / `unpack1("m0")` the base64 gem itself
#   uses, written out (`urlsafe_*` with its padding rules), so the tree's
#   Base64 stand-in is not on this path.
# - The library defaults stand: `Codec.default_serializer` :marshal,
#   `Metadata.use_message_serializer_for_metadata` unset (the legacy
#   envelope), `MessageEncryptor.use_authenticated_message_encryption`
#   false (aes-256-cbc), `KeyGenerator.hash_digest_class` SHA1. A Rails
#   app's `load_defaults` / `config.active_support.*` set them through
#   railtie initializers; assign them the same way. (Core's
#   application.rb configures exactly these values.)
# - The MessagePack serializers (:message_pack, :message_pack_allow_marshal)
#   need ActiveSupport::MessagePack and the msgpack gem, which the tree
#   does not carry: selecting one raises, and a MessagePack payload is not
#   recognized on load (native behavior without msgpack installed).
# - CachingKeyGenerator caches in a Hash rather than a Concurrent::Map:
#   under the GVL `[]` / `||=` behave the same (both may derive a key
#   twice under a race; neither loses one).
# - Rails.application.message_verifier(s), MessageVerifiers /
#   MessageEncryptors and the RotationConfiguration / RotationCoordinator
#   behind them are not ported; no code the tree carries calls them yet.

require "openssl"
require "json"
require "time"
require_relative "active_support_json"
# SerializerWithFallback#load instruments message_serializer_fallback.
require_relative "active_support_notifications"

module ActiveSupport
  module SecurityUtils
    if defined?(OpenSSL.fixed_length_secure_compare)
      def fixed_length_secure_compare(a, b)
        OpenSSL.fixed_length_secure_compare(a, b)
      end
    else
      def fixed_length_secure_compare(a, b)
        raise ArgumentError, "string length mismatch." unless a.bytesize == b.bytesize

        l = a.unpack "C#{a.bytesize}"

        res = 0
        b.each_byte { |byte| res |= byte ^ l.shift }
        res == 0
      end
    end
    module_function :fixed_length_secure_compare

    def secure_compare(a, b)
      a.bytesize == b.bytesize && fixed_length_secure_compare(a, b)
    end
    module_function :secure_compare
  end

  class KeyGenerator
    class << self
      def hash_digest_class=(klass)
        if klass.kind_of?(Class) && klass < OpenSSL::Digest
          @hash_digest_class = klass
        else
          raise ArgumentError, "#{klass} is expected to be an OpenSSL::Digest subclass"
        end
      end

      def hash_digest_class
        @hash_digest_class ||= OpenSSL::Digest::SHA1
      end
    end

    def initialize(secret, options = {})
      @secret = secret
      @iterations = options[:iterations] || 2**16
      @hash_digest_class = options[:hash_digest_class] || self.class.hash_digest_class
    end

    def generate_key(salt, key_size = 64)
      OpenSSL::PKCS5.pbkdf2_hmac(@secret, salt, @iterations, key_size, @hash_digest_class.new)
    end

    def inspect # :nodoc:
      "#<#{self.class.name}:#{'%#016x' % (object_id << 1)}>"
    end
  end

  class CachingKeyGenerator
    def initialize(key_generator)
      @key_generator = key_generator
      @cache_keys = {}
    end

    def generate_key(*args)
      @cache_keys[args.join("|")] ||= @key_generator.generate_key(*args)
    end
  end

  module Messages # :nodoc:
    module SerializerWithFallback # :nodoc:
      def self.[](format)
        if format.to_s.include?("message_pack") && !defined?(ActiveSupport::MessagePack)
          raise LoadError, "the #{format.inspect} message serializer needs ActiveSupport::MessagePack and the msgpack gem, which this tree does not carry"
        end

        SERIALIZERS.fetch(format)
      end

      def load(dumped)
        format = detect_format(dumped)

        if format == self.format
          _load(dumped)
        elsif format && fallback?(format)
          payload = { serializer: SERIALIZERS.key(self), fallback: format, serialized: dumped }
          ActiveSupport::Notifications.instrument("message_serializer_fallback.active_support", payload) do
            payload[:deserialized] = SERIALIZERS[format]._load(dumped)
          end
        else
          raise "Unsupported serialization format"
        end
      end

      private
        def detect_format(dumped)
          case
          when MessagePackWithFallback.dumped?(dumped)
            :message_pack
          when MarshalWithFallback.dumped?(dumped)
            :marshal
          when JsonWithFallback.dumped?(dumped)
            :json
          end
        end

        def fallback?(format)
          format != :marshal
        end

        module AllowMarshal
          private
            def fallback?(format)
              super || format == :marshal
            end
        end

        module MarshalWithFallback
          include SerializerWithFallback
          extend self

          def format
            :marshal
          end

          def dump(object)
            Marshal.dump(object)
          end

          def _load(dumped)
            Marshal.load(dumped)
          end

          MARSHAL_SIGNATURE = "\x04\x08"

          def dumped?(dumped)
            dumped.start_with?(MARSHAL_SIGNATURE)
          end
        end

        module JsonWithFallback
          include SerializerWithFallback
          extend self

          def format
            :json
          end

          def dump(object)
            ActiveSupport::JSON.encode(object)
          end

          def _load(dumped)
            ActiveSupport::JSON.decode(dumped, allow_comments: true)
          end

          JSON_START_WITH = /\A(?:[{\["]|-?\d|true|false|null)/

          def dumped?(dumped)
            JSON_START_WITH.match?(dumped)
          end

          private
            def detect_format(dumped)
              super || :json
            end
        end

        module JsonWithFallbackAllowMarshal
          include JsonWithFallback
          include AllowMarshal
          extend self
        end

        module MessagePackWithFallback
          include SerializerWithFallback
          extend self

          def format
            :message_pack
          end

          def dump(object)
            ActiveSupport::MessagePack.dump(object)
          end

          def _load(dumped)
            ActiveSupport::MessagePack.load(dumped)
          end

          def dumped?(dumped)
            available? && ActiveSupport::MessagePack.signature?(dumped)
          end

          private
            # ActiveSupport::MessagePack is not carried (see the header).
            def available?
              return @available if defined?(@available)
              @available = defined?(ActiveSupport::MessagePack) ? true : false
            end
        end

        module MessagePackWithFallbackAllowMarshal
          include MessagePackWithFallback
          include AllowMarshal
          extend self
        end

        SERIALIZERS = {
          marshal: MarshalWithFallback,
          json: JsonWithFallback,
          json_allow_marshal: JsonWithFallbackAllowMarshal,
          message_pack: MessagePackWithFallback,
          message_pack_allow_marshal: MessagePackWithFallbackAllowMarshal,
        }
    end

    module Metadata # :nodoc:
      singleton_class.attr_accessor :use_message_serializer_for_metadata

      ENVELOPE_SERIALIZERS = [
        *SerializerWithFallback::SERIALIZERS.values,
        ActiveSupport::JSON,
        ::JSON,
        Marshal,
      ]

      TIMESTAMP_SERIALIZERS = [
        SerializerWithFallback::SERIALIZERS.fetch(:message_pack),
        SerializerWithFallback::SERIALIZERS.fetch(:message_pack_allow_marshal),
      ]

      private
        def serialize_with_metadata(data, **metadata)
          has_metadata = metadata.any? { |k, v| v }

          if has_metadata && !use_message_serializer_for_metadata?
            data_string = serialize_to_json_safe_string(data)
            envelope = wrap_in_metadata_legacy_envelope({ "message" => data_string }, **metadata)
            serialize_to_json(envelope)
          else
            data = wrap_in_metadata_envelope({ "data" => data }, **metadata) if has_metadata
            serialize(data)
          end
        end

        def deserialize_with_metadata(message, **expected_metadata)
          if dual_serialized_metadata_envelope_json?(message)
            envelope = deserialize_from_json(message)
            extracted = extract_from_metadata_envelope(envelope, **expected_metadata)
            deserialize_from_json_safe_string(extracted["message"])
          else
            deserialized = deserialize(message)
            if metadata_envelope?(deserialized)
              extract_from_metadata_envelope(deserialized, **expected_metadata)["data"]
            elsif expected_metadata.none? { |k, v| v }
              deserialized
            else
              throw :invalid_message_content, "missing metadata"
            end
          end
        end

        def use_message_serializer_for_metadata?
          Metadata.use_message_serializer_for_metadata && Metadata::ENVELOPE_SERIALIZERS.include?(serializer)
        end

        def wrap_in_metadata_envelope(hash, expires_at: nil, expires_in: nil, purpose: nil)
          expiry = pick_expiry(expires_at, expires_in)
          hash["exp"] = expiry if expiry
          hash["pur"] = purpose.to_s if purpose
          { "_rails" => hash }
        end

        def wrap_in_metadata_legacy_envelope(hash, expires_at: nil, expires_in: nil, purpose: nil)
          expiry = pick_expiry(expires_at, expires_in)
          hash["exp"] = expiry
          hash["pur"] = purpose
          { "_rails" => hash }
        end

        def extract_from_metadata_envelope(envelope, purpose: nil)
          hash = envelope["_rails"]

          if hash["exp"] && Time.now.utc >= parse_expiry(hash["exp"])
            throw :invalid_message_content, "expired"
          end

          if hash["pur"].to_s != purpose.to_s
            throw :invalid_message_content, "mismatched purpose"
          end

          hash
        end

        def metadata_envelope?(object)
          object.is_a?(Hash) && object.key?("_rails")
        end

        def dual_serialized_metadata_envelope_json?(string)
          string.start_with?('{"_rails":{"message":')
        end

        # `Time.now.utc.advance(seconds: expires_in)`: Time#advance with
        # only `seconds:` is `since(seconds)`, that is `self + seconds`
        # (a Duration adds through the tree's Duration-aware Time#+).
        def pick_expiry(expires_at, expires_in)
          expiry = if expires_at
            expires_at.utc
          elsif expires_in
            Time.now.utc + expires_in
          end

          unless Metadata::TIMESTAMP_SERIALIZERS.include?(serializer)
            expiry = expiry&.iso8601(3)
          end

          expiry
        end

        def parse_expiry(expires_at)
          if !expires_at.is_a?(String)
            expires_at
          elsif ActiveSupport.use_standard_json_time_format
            Time.iso8601(expires_at)
          else
            Time.parse(expires_at)
          end
        end

        def serialize_to_json(data)
          ActiveSupport::JSON.encode(data)
        end

        def deserialize_from_json(serialized)
          ActiveSupport::JSON.decode(serialized)
        rescue ::JSON::ParserError => error
          throw :invalid_message_format, error
        end

        def serialize_to_json_safe_string(data)
          encode(serialize(data), url_safe: false)
        end

        def deserialize_from_json_safe_string(string)
          deserialize(decode(string, url_safe: false))
        end
    end

    class Codec # :nodoc:
      include Metadata

      # class_attribute :default_serializer, default: :marshal,
      #   instance_accessor: false, instance_predicate: false
      class << self
        def default_serializer
          if instance_variable_defined?(:@default_serializer)
            @default_serializer
          elsif superclass.respond_to?(:default_serializer)
            superclass.default_serializer
          end
        end

        attr_writer :default_serializer
      end
      self.default_serializer = :marshal

      def initialize(**options)
        @serializer = options[:serializer] || self.class.default_serializer
        @serializer = SerializerWithFallback[@serializer] if @serializer.is_a?(Symbol)
        @url_safe = options[:url_safe]
        @force_legacy_metadata_serializer = options[:force_legacy_metadata_serializer]
      end

      private
        attr_reader :serializer

        def encode(data, url_safe: @url_safe)
          url_safe ? Codec.urlsafe_encode64(data, padding: false) : Codec.strict_encode64(data)
        end

        def decode(encoded, url_safe: @url_safe)
          url_safe ? Codec.urlsafe_decode64(encoded) : Codec.strict_decode64(encoded)
        rescue StandardError => error
          throw :invalid_message_format, error
        end

        def serialize(data)
          serializer.dump(data)
        end

        def deserialize(serialized)
          serializer.load(serialized)
        rescue StandardError => error
          throw :invalid_message_serialization, error
        end

        def catch_and_ignore(throwable, &block)
          catch throwable do
            return block.call
          end
          nil
        end

        def catch_and_raise(throwable, as: nil, &block)
          error = catch throwable do
            return block.call
          end
          error = as.new(error.to_s) if as
          raise error
        end

        def use_message_serializer_for_metadata?
          !@force_legacy_metadata_serializer && super
        end

      # The base64 gem's own definitions (base64 0.3.0), over pack/unpack.
      class << self
        def strict_encode64(bin) # :nodoc:
          [bin].pack("m0")
        end

        def strict_decode64(str) # :nodoc:
          str.unpack1("m0")
        end

        def urlsafe_encode64(bin, padding: true) # :nodoc:
          str = strict_encode64(bin)
          str.chomp!("==") or str.chomp!("=") unless padding
          str.tr!("+/", "-_")
          str
        end

        def urlsafe_decode64(str) # :nodoc:
          if !str.end_with?("=") && str.length % 4 != 0
            str = str.ljust((str.length + 3) & ~3, "=")
            str.tr!("-_", "+/")
          else
            str = str.tr("-_", "+/")
          end
          strict_decode64(str)
        end
      end
    end

    module Rotator # :nodoc:
      def initialize(*args, on_rotation: nil, **options)
        super(*args, **options)
        @args = args
        @options = options
        @rotations = []
        @on_rotation = on_rotation
      end

      def rotate(*args, **options)
        fall_back_to build_rotation(*args, **options)
      end

      def on_rotation(&on_rotation)
        @on_rotation = on_rotation
        self
      end

      def fall_back_to(fallback)
        @rotations << fallback
        self
      end

      def read_message(message, on_rotation: @on_rotation, **options)
        if @rotations.empty?
          super(message, **options)
        else
          thrown, error = catch_rotation_error do
            return super(message, **options)
          end

          @rotations.each do |rotation|
            catch_rotation_error do
              value = rotation.read_message(message, **options)
              on_rotation&.call
              return value
            end
          end

          throw thrown, error
        end
      end

      def initialize_dup(*)
        super
        @rotations = @rotations.dup
      end

      private
        def build_rotation(*args, **options)
          self.class.new(*args, *@args.drop(args.length), **@options, **options)
        end

        def catch_rotation_error(&block)
          error = catch :invalid_message_format do
            error = catch :invalid_message_serialization do
              return [nil, block.call]
            end
            return [:invalid_message_serialization, error]
          end
          [:invalid_message_format, error]
        end
    end
  end

  class MessageVerifier < Messages::Codec
    prepend Messages::Rotator

    class InvalidSignature < StandardError; end

    SEPARATOR = "--" # :nodoc:
    SEPARATOR_LENGTH = SEPARATOR.length # :nodoc:

    def initialize(secret, **options)
      raise ArgumentError, "Secret should not be nil." unless secret
      super(**options)
      @secret = secret
      @digest = options[:digest]&.to_s || "SHA1"
    end

    def valid_message?(message)
      !!catch_and_ignore(:invalid_message_format) { extract_encoded(message) }
    end

    def verified(message, **options)
      catch_and_ignore :invalid_message_format do
        catch_and_raise :invalid_message_serialization do
          catch_and_ignore :invalid_message_content do
            read_message(message, **options)
          end
        end
      end
    end

    def verify(message, **options)
      catch_and_raise :invalid_message_format, as: InvalidSignature do
        catch_and_raise :invalid_message_serialization do
          catch_and_raise :invalid_message_content, as: InvalidSignature do
            read_message(message, **options)
          end
        end
      end
    end

    def generate(value, **options)
      create_message(value, **options)
    end

    def create_message(value, **options) # :nodoc:
      sign_encoded(encode(serialize_with_metadata(value, **options)))
    end

    def read_message(message, **options) # :nodoc:
      deserialize_with_metadata(decode(extract_encoded(message)), **options)
    end

    def inspect # :nodoc:
      "#<#{self.class.name}:#{'%#016x' % (object_id << 1)}>"
    end

    private
      def decode(encoded, url_safe: @url_safe)
        catch :invalid_message_format do
          return super
        end
        super(encoded, url_safe: !url_safe)
      end

      def sign_encoded(encoded)
        digest = generate_digest(encoded)
        encoded << SEPARATOR << digest
      end

      def extract_encoded(signed)
        if signed.nil? || !signed.valid_encoding?
          throw :invalid_message_format, "invalid message string"
        end

        if separator_index = separator_index_for(signed)
          encoded = signed[0, separator_index]
          digest = signed[separator_index + SEPARATOR_LENGTH, digest_length_in_hex]
        end

        unless digest_matches_data?(digest, encoded)
          throw :invalid_message_format, "mismatched digest"
        end

        encoded
      end

      def generate_digest(data)
        OpenSSL::HMAC.hexdigest(@digest, @secret, data)
      end

      def digest_length_in_hex
        @digest_length_in_hex ||= OpenSSL::Digest.new(@digest).digest_length * 2
      end

      def separator_at?(signed_message, index)
        signed_message[index, SEPARATOR_LENGTH] == SEPARATOR
      end

      def separator_index_for(signed_message)
        index = signed_message.length - digest_length_in_hex - SEPARATOR_LENGTH
        index unless index.negative? || !separator_at?(signed_message, index)
      end

      # `data.present? && digest.present?`: a non-blank String (nil and
      # whitespace-only strings are blank).
      def digest_matches_data?(digest, data)
        present_string?(data) && present_string?(digest) && ActiveSupport::SecurityUtils.secure_compare(digest, generate_digest(data))
      end

      def present_string?(value)
        !value.nil? && !/\A[[:space:]]*\z/.match?(value)
      end
  end

  class MessageEncryptor < Messages::Codec
    prepend Messages::Rotator

    # cattr_accessor :use_authenticated_message_encryption,
    #   instance_accessor: false, default: false
    @@use_authenticated_message_encryption = false

    class << self
      def use_authenticated_message_encryption
        @@use_authenticated_message_encryption
      end

      def use_authenticated_message_encryption=(value)
        @@use_authenticated_message_encryption = value
      end

      def default_cipher # :nodoc:
        if use_authenticated_message_encryption
          "aes-256-gcm"
        else
          "aes-256-cbc"
        end
      end
    end

    module NullSerializer # :nodoc:
      def self.load(value)
        value
      end

      def self.dump(value)
        value
      end
    end

    class InvalidMessage < StandardError; end
    OpenSSLCipherError = OpenSSL::Cipher::CipherError

    AUTH_TAG_LENGTH = 16 # :nodoc:
    SEPARATOR = "--" # :nodoc:

    def initialize(secret, sign_secret = nil, **options)
      super(**options)
      @secret = secret
      @cipher = options[:cipher] || self.class.default_cipher
      @aead_mode = new_cipher.authenticated?
      @verifier = if !@aead_mode
        MessageVerifier.new(sign_secret || secret, **options, serializer: NullSerializer)
      end
    end

    def encrypt_and_sign(value, **options)
      create_message(value, **options)
    end

    def decrypt_and_verify(message, **options)
      catch_and_raise :invalid_message_format, as: InvalidMessage do
        catch_and_raise :invalid_message_serialization, as: InvalidMessage do
          catch_and_ignore :invalid_message_content do
            read_message(message, **options)
          end
        end
      end
    end

    def self.key_len(cipher = default_cipher)
      OpenSSL::Cipher.new(cipher).key_len
    end

    def create_message(value, **options) # :nodoc:
      sign(encrypt(serialize_with_metadata(value, **options)))
    end

    def read_message(message, **options) # :nodoc:
      deserialize_with_metadata(decrypt(verify(message)), **options)
    end

    def inspect # :nodoc:
      "#<#{self.class.name}:#{'%#016x' % (object_id << 1)}>"
    end

    private
      def sign(data)
        @verifier ? @verifier.create_message(data) : data
      end

      def verify(data)
        @verifier ? @verifier.read_message(data) : data
      end

      def encrypt(data)
        cipher = new_cipher
        cipher.encrypt
        cipher.key = @secret

        iv = cipher.random_iv
        cipher.auth_data = "" if aead_mode?

        encrypted_data = cipher.update(data)
        encrypted_data << cipher.final

        parts = [encrypted_data, iv]
        parts << cipher.auth_tag(AUTH_TAG_LENGTH) if aead_mode?

        join_parts(parts)
      end

      def decrypt(encrypted_message)
        cipher = new_cipher
        encrypted_data, iv, auth_tag = extract_parts(encrypted_message)

        if aead_mode? && auth_tag.bytesize != AUTH_TAG_LENGTH
          throw :invalid_message_format, "truncated auth_tag"
        end

        cipher.decrypt
        cipher.key = @secret
        cipher.iv  = iv
        if aead_mode?
          cipher.auth_tag = auth_tag
          cipher.auth_data = ""
        end

        decrypted_data = cipher.update(encrypted_data)
        decrypted_data << cipher.final
      rescue OpenSSLCipherError => error
        throw :invalid_message_format, error
      end

      def length_after_encode(length_before_encode)
        if @url_safe
          (4 * length_before_encode / 3.0).ceil # length without padding
        else
          4 * (length_before_encode / 3.0).ceil # length with padding
        end
      end

      def length_of_encoded_iv
        @length_of_encoded_iv ||= length_after_encode(new_cipher.iv_len)
      end

      def length_of_encoded_auth_tag
        @length_of_encoded_auth_tag ||= length_after_encode(AUTH_TAG_LENGTH)
      end

      def join_parts(parts)
        parts.map! { |part| encode(part) }.join(SEPARATOR)
      end

      def extract_part(encrypted_message, rindex, length)
        index = rindex - length

        if encrypted_message[index - SEPARATOR.length, SEPARATOR.length] == SEPARATOR
          encrypted_message[index, length]
        else
          throw :invalid_message_format, "missing separator"
        end
      end

      def extract_parts(encrypted_message)
        parts = []
        rindex = encrypted_message.length

        if aead_mode?
          parts << extract_part(encrypted_message, rindex, length_of_encoded_auth_tag)
          rindex -= SEPARATOR.length + length_of_encoded_auth_tag
        end

        parts << extract_part(encrypted_message, rindex, length_of_encoded_iv)
        rindex -= SEPARATOR.length + length_of_encoded_iv

        parts << encrypted_message[0, rindex]

        parts.reverse!.map! { |part| decode(part) }
      end

      def new_cipher
        OpenSSL::Cipher.new(@cipher)
      end

      attr_reader :aead_mode
      alias :aead_mode? :aead_mode
  end

  class SecureCompareRotator
    include SecurityUtils

    InvalidMatch = Class.new(StandardError)

    def initialize(value, on_rotation: nil)
      @value = value
      @rotate_values = []
      @on_rotation = on_rotation
    end

    def rotate(previous_value)
      @rotate_values << previous_value
    end

    def secure_compare!(other_value, on_rotation: @on_rotation)
      if secure_compare(@value, other_value)
        true
      elsif @rotate_values.any? { |value| secure_compare(value, other_value) }
        on_rotation&.call
        true
      else
        raise InvalidMatch
      end
    end
  end
end
