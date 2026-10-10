# Native Rails oracle (the Rails native Core runs; tests/support/
# rails_oracle.rb) for SecureRandom.base58/.base36/.base32
# (core_ext/securerandom) against runtime/spinel/active_support_securerandom.rb.
#
#   ruby tests/active_support_securerandom_runtime.rb compare
#
# runs this script twice: `native` requires the pinned activesupport's
# feature; `emitted` loads the port as the emitted PostgreSQL tree's
# runtime/ does, over the tree's stub slot. Both replace SecureRandom's
# byte source with the same fixed sequence, so the strings themselves
# are compared, not only their shape. Each prints one line per
# observation; `compare` requires them equal.
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
  puts "Native Rails and emitted SecureRandom.base58 agree (#{RailsOracle.label})"
  exit
end

MODE = ARGV.first
abort "usage: #{$PROGRAM_NAME} compare | native | emitted" unless %w[native emitted].include?(MODE)

if MODE == "native"
  RailsOracle.activate("activesupport")
  require "active_support"
  require "active_support/core_ext/securerandom"
else
  require "securerandom"
  abort "SecureRandom.base58 defined before the port loads" if SecureRandom.respond_to?(:base58)
  # The emitted Ruby tree's stub slot (project.rs::SECURE_RANDOM_STUB_REOPEN),
  # which reopens SecureRandom.alphanumeric before the port loads.
  src = File.read(File.join(ROOT, "src/project.rs"))
  eval(src[/const SECURE_RANDOM_STUB_REOPEN: &str = r##"(.*?)"##;/m, 1], TOPLEVEL_BINDING)
  require File.join(ROOT, "runtime/spinel/active_support_securerandom")
end

def show(label, value)
  puts "#{label}: #{value.inspect}"
end

def outcome
  yield.inspect
rescue Exception => e
  "#{e.class}: #{e.message}"
end

show("base58 alphabet", SecureRandom::BASE58_ALPHABET.join)
show("base36 alphabet", SecureRandom::BASE36_ALPHABET.join)
show("base32 alphabet", SecureRandom::BASE32_ALPHABET.join)
show("default lengths", [SecureRandom.base58, SecureRandom.base36, SecureRandom.base32].map(&:length))
show("nil length", SecureRandom.base58(nil).length)
show("base58 charset", 200.times.flat_map { SecureRandom.base58(24).chars }.uniq.all? { |c| SecureRandom::BASE58_ALPHABET.include?(c) })
show("base36 charset", 200.times.flat_map { SecureRandom.base36(28).chars }.uniq.all? { |c| SecureRandom::BASE36_ALPHABET.include?(c) })
show("encoding", SecureRandom.base58(4).encoding)
show("negative", outcome { SecureRandom.base58(-1) })

# A fixed byte source: the strings themselves, as Core calls them
# (access_and_auth's `SecureRandom.base58(24)`, bulk_operation_asset's
# `SecureRandom.base36(28)`).
module SecureRandom
  @fixed = 0
  def self.gen_random(n)
    (0...n).map { (@fixed = (@fixed * 37 + 11) % 256).chr }.join.b
  end
end
show("base58(24)", SecureRandom.base58(24))
show("base36(28)", SecureRandom.base36(28))
show("base32", SecureRandom.base32)
show("base58(0)", SecureRandom.base58(0))
