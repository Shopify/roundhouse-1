# frozen_string_literal: true

# `cookies.encrypted` — Rails' EncryptedKeyRotatingCookieJar (actionpack's
# cookies.rb) over the CRuby tree's ActiveSupport::MessageEncryptor port
# (runtime/spinel/active_support_messages.rb).
#
#   cookies.encrypted[:k] = v
#   cookies.encrypted[:k]
#   cookies.permanent.encrypted[:k] = v
#
# The wire format is Rails': AES-256-GCM, its key the application's key
# generator (PBKDF2 over `secret_key_base`, 1,000 iterations, the
# `authenticated_encrypted_cookie_salt` "authenticated encrypted cookie",
# 32 bytes), the value serialized as `config.action_dispatch.
# cookies_serializer` says (Core sets :marshal), and the cookie name
# bound into the message as its purpose, `cookie.<name>`. A value that is
# absent, tampered with, or minted for another cookie reads as nil, as it
# does in Rails.
#
# A view over the plain jar, as `cookies.signed` is (shared
# runtime/ruby/action_controller/cookies.rb): the encrypted write lands in
# the same pending map the dispatcher serializes into Set-Cookie. Not in
# the shared runtime because the encryptor is dynamic CRuby (OpenSSL,
# Marshal), which the strict targets and Spinel do not carry.
module ActionController
  class EncryptedCookieJar
    SALT = "authenticated encrypted cookie"

    def initialize(jar, permanent = false)
      @jar = jar
      @permanent = permanent
    end

    # nil when the cookie is absent or does not decrypt for this name.
    def [](key)
      raw = @jar.raw(key)
      return nil if raw.nil? || raw == ""
      EncryptedCookieJar.encryptor.decrypt_and_verify(raw, purpose: "cookie.#{key}")
    rescue ActiveSupport::MessageEncryptor::InvalidMessage
      nil
    end

    # A bare value, or the options Hash Rails takes (`value:` beside
    # `expires:`, `httponly:`, `same_site:`, `secure:`).
    def []=(key, value)
      options = value.is_a?(Hash) ? value : { value: value }
      encrypted = EncryptedCookieJar.encryptor.encrypt_and_sign(
        options[:value],
        purpose: "cookie.#{key}",
        expires_at: options[:expires],
        expires_in: options[:expire_after]
      )
      @jar.raw_set(key, encrypted)
      @jar.record_permanent(key) if @permanent
      if value.is_a?(Hash)
        @jar.record_flags(key, options[:httponly] != false, options[:same_site].to_s, options[:secure] == true)
      end
      value
    end

    def permanent
      EncryptedCookieJar.new(@jar, true)
    end

    def delete(key)
      @jar.delete(key)
    end

    # One encryptor per secret and serializer, as Rails builds one per jar
    # from the request's cached key generator.
    def self.encryptor
      secret = Rails.application.secret_key_base
      serializer = serializer_name
      cached = @encryptor
      return cached[2] if cached && cached[0] == secret && cached[1] == serializer
      generator = ActiveSupport::KeyGenerator.new(secret, iterations: 1000)
      cipher = "aes-256-gcm"
      key = generator.generate_key(SALT, ActiveSupport::MessageEncryptor.key_len(cipher))
      built = ActiveSupport::MessageEncryptor.new(
        key,
        cipher: cipher,
        serializer: ActiveSupport::Messages::SerializerWithFallback[serializer]
      )
      @encryptor = [secret, serializer, built].freeze
      built
    end

    # `config.action_dispatch.cookies_serializer`; Rails 7.0+'s default,
    # :json, where an app sets none.
    def self.serializer_name
      options = Rails.application.config.action_dispatch
      name = options.respond_to?(:cookies_serializer) ? options.cookies_serializer : nil
      name.nil? ? :json : name.to_sym
    end
  end

  class CookieJar
    def encrypted
      ActionController::EncryptedCookieJar.new(self)
    end
  end

  class PermanentCookieJar
    def encrypted
      ActionController::EncryptedCookieJar.new(@jar, true)
    end
  end
end
