# frozen_string_literal: true

# Digest::UUID (Shopify/rails 5813559f4aed, 8.2.0.alpha
# activesupport/lib/active_support/core_ext/digest/uuid.rb), verbatim;
# Rails' doc comments dropped. Rails' :load_active_support initializer
# loads it with active_support/all, so Core calls Digest::UUID.uuid_v5
# with no require; the emitted tree loads it where app code names
# Digest::UUID (`project::BOOT_LOADED_CORE_EXTENSIONS`), and a native
# gem's `require "active_support/core_ext/digest/uuid"` answers from
# runtime/features (`project::FRAMEWORK_FEATURE_PORTS`).
#
# The PostgreSQL CRuby tree only (`project::apply_postgres_bootstrap`).

require "securerandom"
require "openssl"

module Digest
  module UUID
    DNS_NAMESPACE  = "k\xA7\xB8\x10\x9D\xAD\x11\xD1\x80\xB4\x00\xC0O\xD40\xC8"
    URL_NAMESPACE  = "k\xA7\xB8\x11\x9D\xAD\x11\xD1\x80\xB4\x00\xC0O\xD40\xC8"
    OID_NAMESPACE  = "k\xA7\xB8\x12\x9D\xAD\x11\xD1\x80\xB4\x00\xC0O\xD40\xC8"
    X500_NAMESPACE = "k\xA7\xB8\x14\x9D\xAD\x11\xD1\x80\xB4\x00\xC0O\xD40\xC8"

    def self.uuid_from_hash(hash_class, namespace, name)
      if hash_class == Digest::MD5 || hash_class == OpenSSL::Digest::MD5
        version = 3
      elsif hash_class == Digest::SHA1 || hash_class == OpenSSL::Digest::SHA1
        version = 5
      else
        raise ArgumentError, "Expected OpenSSL::Digest::SHA1 or OpenSSL::Digest::MD5, got #{hash_class.name}."
      end

      uuid_namespace = pack_uuid_namespace(namespace)

      hash = hash_class.new
      hash.update(uuid_namespace)
      hash.update(name)

      ary = hash.digest.unpack("NnnnnN")
      ary[2] = (ary[2] & 0x0FFF) | (version << 12)
      ary[3] = (ary[3] & 0x3FFF) | 0x8000

      "%08x-%04x-%04x-%04x-%04x%08x" % ary
    end

    def self.uuid_v3(uuid_namespace, name)
      uuid_from_hash(OpenSSL::Digest::MD5, uuid_namespace, name)
    end

    def self.uuid_v5(uuid_namespace, name)
      uuid_from_hash(OpenSSL::Digest::SHA1, uuid_namespace, name)
    end

    def self.uuid_v4
      SecureRandom.uuid
    end

    def self.nil_uuid
      "00000000-0000-0000-0000-000000000000"
    end

    def self.pack_uuid_namespace(namespace)
      if [DNS_NAMESPACE, OID_NAMESPACE, URL_NAMESPACE, X500_NAMESPACE].include?(namespace)
        namespace
      else
        match_data = namespace.match(/\A(\h{8})-(\h{4})-(\h{4})-(\h{4})-(\h{4})(\h{8})\z/)

        raise ArgumentError, "Only UUIDs are valid namespace identifiers" unless match_data.present?

        match_data.captures.map { |s| s.to_i(16) }.pack("NnnnnN")
      end
    end

    private_class_method :pack_uuid_namespace
  end
end
