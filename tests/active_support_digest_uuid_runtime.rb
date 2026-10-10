# Native Rails oracle (the Rails native Core runs; tests/support/
# rails_oracle.rb) for Digest::UUID (core_ext/digest/uuid) against
# runtime/spinel/active_support_digest_uuid.rb.
#
#   ruby tests/active_support_digest_uuid_runtime.rb compare
#
# runs this script twice: `native` requires the pinned activesupport's
# feature; `emitted` loads the port as the emitted PostgreSQL tree's
# runtime/ does. Each prints one line per observation; `compare` requires
# them equal.
require_relative "support/rails_oracle"

ROOT = File.expand_path("..", __dir__)

if ARGV.first == "compare"
  require "open3"
  outputs = ["native", "emitted"].map do |mode|
    env = mode == "emitted" ? RailsOracle.core_gems_env : {}
    stdout, stderr, status = Open3.capture3(env, RbConfig.ruby, File.expand_path(__FILE__), mode)
    abort "#{mode}: #{stderr}" unless status.success?
    stdout
  end
  native, emitted = outputs.map(&:lines)
  unless outputs.uniq.length == 1
    diff = native.zip(emitted).each_with_index.reject { |(x, y), _| x == y }.first(8)
    abort "ports differ:\n" + diff.map { |(x, y), i| "line #{i + 1}\n  native:  #{x}  emitted: #{y}" }.join("\n")
  end
  puts outputs.first
  puts "observations: #{native.length}"
  puts "Native Rails and emitted Digest::UUID agree (#{RailsOracle.label})"
  exit
end

MODE = ARGV.first
abort "usage: #{$PROGRAM_NAME} compare | native | emitted" unless %w[native emitted].include?(MODE)

if MODE == "native"
  RailsOracle.activate("activesupport")
  require "active_support"
  require "active_support/core_ext/object/blank"
  require "active_support/core_ext/digest/uuid"
else
  abort "Digest::UUID defined before the port loads" if defined?(Digest::UUID)
  # The emitted Ruby tree's Object#present?.
  require File.join(ROOT, "runtime/spinel/scaffold/ruby_overlay/runtime/active_support_core_ext")
  require File.join(ROOT, "runtime/spinel/active_support_digest_uuid")
end

def show(label, value)
  puts "#{label}: #{value.inspect}"
end

def outcome
  yield.inspect
rescue Exception => e
  "#{e.class}: #{e.message}"
end

# reviews/safe_risk_assessment_event_id.rb: uuid_v5 over a UUID-string
# namespace; webhooks' deliverable_webhook_subscription.rb: URL_NAMESPACE.
show("v5 url", Digest::UUID.uuid_v5(Digest::UUID::URL_NAMESPACE, "https://example.com/hooks/1"))
show("v5 oid", Digest::UUID.uuid_v5(Digest::UUID::OID_NAMESPACE, "shop-42"))
show("v5 dns", Digest::UUID.uuid_v5(Digest::UUID::DNS_NAMESPACE, "shopify.com"))
show("v3 x500", Digest::UUID.uuid_v3(Digest::UUID::X500_NAMESPACE, "cn=x"))
show("v5 string namespace", Digest::UUID.uuid_v5("6ba7b810-9dad-11d1-80b4-00c04fd430c8", "assessment-1-42"))
show("from_hash md5", Digest::UUID.uuid_from_hash(OpenSSL::Digest::MD5, Digest::UUID::URL_NAMESPACE, "a"))
show("from_hash Digest::SHA1", Digest::UUID.uuid_from_hash(Digest::SHA1, Digest::UUID::URL_NAMESPACE, "a"))
show("bad hash class", outcome { Digest::UUID.uuid_from_hash(OpenSSL::Digest::SHA256, Digest::UUID::URL_NAMESPACE, "a") })
show("bad namespace", outcome { Digest::UUID.uuid_v5("not-a-uuid", "a") })
show("v4 shape", Digest::UUID.uuid_v4.match?(/\A\h{8}-\h{4}-4\h{3}-[89ab]\h{3}-\h{12}\z/))
show("nil uuid", Digest::UUID.nil_uuid)
show("private pack", outcome { Digest::UUID.pack_uuid_namespace("x") })
show("namespace frozen", Digest::UUID::URL_NAMESPACE.frozen?)
show("namespace encoding", Digest::UUID::URL_NAMESPACE.encoding)
