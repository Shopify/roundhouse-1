# frozen_string_literal: true

# SecureRandom.base58, .base36 and .base32 (Shopify/rails 5813559f4aed,
# 8.2.0.alpha activesupport/lib/active_support/core_ext/securerandom.rb),
# verbatim; Rails' doc comments dropped. Rails' :load_active_support
# initializer loads it with active_support/all, so Core calls
# SecureRandom.base58 with no require; the emitted tree loads it where
# app code calls one of the three (`project::BOOT_LOADED_CORE_EXTENSIONS`),
# and a native gem's `require "active_support/core_ext/securerandom"`
# answers from runtime/features (`project::FRAMEWORK_FEATURE_PORTS`).
#
# Each calls `alphanumeric`, as natively, so the tree's stub slot
# (runtime/secure_random_stub.rb) answers a stubbed `alphanumeric` here
# too, as mocha's stub does under Rails.
#
# The PostgreSQL CRuby tree only (`project::apply_postgres_bootstrap`).

require "securerandom"

module SecureRandom
  BASE58_ALPHABET = ("0".."9").to_a + ("A".."Z").to_a + ("a".."z").to_a - ["0", "O", "I", "l"]
  BASE36_ALPHABET = ("0".."9").to_a + ("a".."z").to_a
  BASE32_ALPHABET = ("0".."9").to_a + ("A".."Z").to_a - ["I", "L", "O", "U"]

  def self.base58(n = 16)
    alphanumeric(n, chars: BASE58_ALPHABET)
  end

  def self.base36(n = 16)
    alphanumeric(n, chars: BASE36_ALPHABET)
  end

  def self.base32(n = 16)
    alphanumeric(n, chars: BASE32_ALPHABET)
  end
end
