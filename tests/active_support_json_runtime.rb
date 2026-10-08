# Native Rails oracle (the Rails native Core runs; tests/support/
# rails_oracle.rb) for core_ext/object/json.rb's `as_json` table against
# runtime/spinel/active_support_json.rb's `Encoding.as_json`.
#
#   ruby tests/active_support_json_runtime.rb compare
#
# runs this script twice: `native` requires the pinned activesupport's
# json and core_ext/object/json and asks each value `value.as_json(opts)`;
# `emitted` loads the runtime as the emitted PostgreSQL CRuby tree's boot
# does (rails.rb, active_support_ext.rb, then the port it requires) and
# asks `ActiveSupport::JSON::Encoding.as_json(value, opts)`, the call
# `lower::as_json_residue` emits. Each prints one line per observation
# (core values, Hash `only:` / `except:`, Time / Date / DateTime under
# every time_precision and the legacy format, Struct, Data, Enumerable,
# an object's own `as_json`, `to_hash`, instance values, ...), and the
# `ActiveSupport::JSON.encode` text of the same value; `compare` requires
# them equal.
require_relative "support/rails_oracle"

if ARGV.first == "compare"
  require "open3"
  outputs = ["native", "emitted"].map do |mode|
    env = mode == "emitted" ? RailsOracle.core_gems_env : {}
    stdout, stderr, status = Open3.capture3(env, RbConfig.ruby, File.expand_path(__FILE__), mode)
    abort "#{mode}: #{stderr}" unless status.success?
    stdout
  end
  native, emitted = outputs.map(&:lines)
  equal = native.zip(emitted).count { |x, y| x == y }
  unless outputs.uniq.length == 1
    diff = native.zip(emitted).each_with_index.reject { |(x, y), _| x == y }.first(8)
    abort "as_json differs (#{equal}/#{native.length} observations equal, emitted has #{emitted.length}):\n" +
      diff.map { |(x, y), i| "line #{i + 1}\n  native:  #{x}  emitted: #{y}" }.join("\n")
  end
  puts outputs.first
  puts "observations: #{equal}/#{native.length} equal"
  puts "Native Rails and emitted ActiveSupport::JSON as_json agree (#{RailsOracle.label})"
  exit
end

MODE = ARGV.first
abort "usage: #{$PROGRAM_NAME} compare | native | emitted" unless %w[native emitted].include?(MODE)

require "bigdecimal"
require "date"
require "time"
require "set"
require "uri"
require "pathname"
require "ipaddr"

if MODE == "native"
  RailsOracle.activate("activesupport")
  require "active_support"
  require "active_support/json"
  require "active_support/core_ext/object/json"
  require "active_support/duration"
  def ask_as_json(value, *options) = value.as_json(*options)
else
  root = File.expand_path("..", __dir__)
  require File.join(root, "runtime/ruby/rails")
  require File.join(root, "runtime/ruby/active_support_ext")
  require File.join(root, "runtime/spinel/scaffold/ruby_overlay/runtime/active_support_duration")
  require File.join(root, "runtime/spinel/active_support_json")
  def ask_as_json(value, *options) = ActiveSupport::JSON::Encoding.as_json(value, *options)
end

def shown(value)
  case value
  when String then "#{value.inspect} (#{value.encoding})"
  when Hash then "{" + value.map { |k, v| "#{k.inspect}=>#{shown(v)}" }.join(", ") + "}"
  when Array then "[" + value.map { |v| shown(v) }.join(", ") + "]"
  else "#{value.inspect} (#{value.class})"
  end
end

def outcome
  shown(yield)
rescue Exception => e
  "#{e.class}: #{e.message.lines.first.to_s.chomp.inspect}"
end

def observe(label, value, *options)
  puts "#{label}: #{outcome { ask_as_json(value, *options) }}"
  puts "#{label} encode: #{outcome { ActiveSupport::JSON.encode(value) }}"
end

class OwnJson
  def initialize(n) = @n = n
  def as_json(options = nil) = { "own" => @n, "options" => options&.keys&.sort }
end

class HashLike
  def to_hash = { b: 2, a: Time.utc(2024, 1, 2, 3, 4, 5) }
end

class Plain
  def initialize
    @name = :acme
    @at = Date.new(2024, 2, 29)
    @nested = { "k" => [1, 2.5, nil] }
    @secret = "s"
  end
end

class Letters
  include Enumerable
  def each
    yield "a"
    yield :b
  end
end

Point = Struct.new(:x, :y)
Coord = Data.define(:lat, :lng)

utc = Time.utc(2024, 5, 6, 7, 8, 9, 123456.789r)
offset = Time.new(2024, 12, 31, 23, 59, 58.5r, "-05:30")
datetime = DateTime.new(2024, 3, 4, 5, 6, 7.25r, "+02:00")
date = Date.new(2024, 2, 29)
status = (system("exit 3") && nil) || $?

observe("nil", nil)
observe("true", true)
observe("false", false)
observe("string", "héllo <&>")
observe("symbol", :sym)
observe("integer", 42)
observe("bignum", 2**80)
observe("float", 1.5)
observe("float nan", Float::NAN)
observe("float infinity", -Float::INFINITY)
observe("rational", 3/4r)
observe("bigdecimal", BigDecimal("1234.5678"))
observe("bigdecimal small", BigDecimal("0.000001"))
observe("bigdecimal integer", BigDecimal("10"))
observe("bigdecimal nan", BigDecimal("NaN"))
observe("bigdecimal infinity", BigDecimal("Infinity"))
observe("array", [1, :a, "b", nil, [2.0, Float::NAN]])
observe("hash", { a: 1, "b" => :c, 3 => [nil, true], nil => { d: Float::INFINITY } })
observe("hash only", { a: 1, b: 2, c: 3 }, only: [:a, :c])
observe("hash only scalar", { a: 1, b: 2 }, only: :b)
observe("hash except", { a: 1, b: 2, c: 3 }, except: [:b])
observe("hash only string keys", { "a" => 1, "b" => 2 }, only: [:a])
observe("hash only nested", { a: { a: 1, b: 2 }, b: 3 }, only: :a)
observe("hash except nested", { a: [{ a: 1, b: 2 }], b: 3 }, except: :b)
observe("hash empty options", { a: 1 }, {})
observe("array options", [{ a: 1, b: 2 }, { b: 3 }], only: :b)
observe("own as_json", OwnJson.new(1))
observe("own as_json options", OwnJson.new(2), only: :x)
observe("own as_json nested", { k: OwnJson.new(3) }, except: :z)
observe("to_hash", HashLike.new)
observe("to_hash except", HashLike.new, except: :a)
observe("instance values", Plain.new)
observe("instance values only", Plain.new, only: ["name", "nested"])
observe("struct", Point.new(1, date))
observe("struct only", Point.new(1, 2), only: :y)
observe("data", Coord.new(lat: 1.5, lng: BigDecimal("2.25")))
observe("set", Set[3, :x, "y"])
observe("enumerable", Letters.new)
observe("range", 1..3)
observe("string range", "a"..."c")
observe("regexp", /a+b/i)
observe("exception", StandardError.new("boom"))
observe("exception class", ArgumentError)
observe("module", Comparable)
observe("anonymous class", Class.new)
observe("uri", URI("https://shop.test/a?b=1"))
observe("pathname", Pathname("/tmp/x y"))
observe("ipv4", IPAddr.new("10.1.2.3"))
observe("ipv4 net", IPAddr.new("10.0.0.0/8"))
observe("ipv6", IPAddr.new("::1"))
observe("ipv6 net", IPAddr.new("fe80::/64"))
# IO#to_s names the object's address, which differs between the runs.
puts "io: #{outcome { ask_as_json($stdout) == $stdout.to_s }}"
observe("duration", ActiveSupport::Duration.days(3))
puts "process status: #{outcome { ask_as_json(status).merge(pid: ask_as_json(status)[:pid] == status.pid) }}"

[0, 3, 6].each do |precision|
  ActiveSupport::JSON::Encoding.time_precision = precision
  observe("time utc p#{precision}", utc)
  observe("time offset p#{precision}", offset)
  observe("datetime p#{precision}", datetime)
  observe("date p#{precision}", date)
  observe("nested times p#{precision}", { t: [utc, date], d: datetime })
end
ActiveSupport::JSON::Encoding.time_precision = 3
ActiveSupport.use_standard_json_time_format = false
observe("legacy time utc", utc)
observe("legacy time offset", offset)
observe("legacy datetime", datetime)
observe("legacy date", date)
ActiveSupport.use_standard_json_time_format = true
ActiveSupport.time_precision = 1
observe("module switch precision", utc)
