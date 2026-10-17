# frozen_string_literal: true
# The storage boundary for schema-less ActiveRecord json/jsonb columns
# and `serialize …, coder: JSON`. SQLite hands the generated model
# serialized text; Rails exposes the decoded Array/Hash/scalar.
# Hydration writes stored text directly to the backing slot (never
# through dump); application assignment always JSON-encodes, including
# String values, so `"true"` round-trips as a string rather than bool.
#
# Named for Roundhouse, not the app: Shopify Core defines its own
# top-level `class JsonColumn` (a serialize coder), and a runtime
# `module JsonColumn` loaded first made that class fail to load
# ("JsonColumn is not a class").
require "json"

module RoundhouseJsonColumn
  def self.load(serialized)
    return nil if serialized.nil?
    JSON.parse(serialized)
  end

  # The dirty-tracking baseline: stored text that is not JSON hydrates
  # as that text, as Rails' lazily decoded attribute does, so only the
  # reader raises (an unrelated update leaves the column as stored).
  def self.snapshot(serialized)
    load(serialized)
  rescue JSON::ParserError
    serialized
  end

  def self.dump(value)
    return nil if value.nil?
    JSON.generate(value)
  end
end
