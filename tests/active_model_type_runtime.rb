# Native ActiveModel / ActiveRecord 8.1.4 oracle for the attribute type
# port (runtime/spinel/active_model_type.rb).
#
# Modes:
# - `native` / `emitted` print one line per observation: for each type
#   and option set (the ones Core's DeveloperDashboard::FastDev::
#   TablelessSchema builds, and their neighbours) its type, limit,
#   precision, scale, ancestry and equality, and what cast, serialize,
#   deserialize, serializable?, changed? and changed_in_place? answer
#   over a battery of values, the failure each raises included.
# - `compare` runs both and requires every observation equal.
#
# DateTime and BigDecimal values are not in the battery: what the types
# answer for one comes from ActiveSupport's extensions of those classes
# (DateTime#to_i / #nsec / #getutc, BigDecimal#to_s), which the tree
# does not carry, not from the types.
#
# The process runs in a fixed non-UTC zone so the :local timezone
# branches are observed too.
ENV["TZ"] = "ABC-3" # POSIX: UTC+3, no zoneinfo needed

if ARGV.first == "compare"
  require "open3"
  run = lambda do |mode|
    stdout, stderr, status = Open3.capture3(RbConfig.ruby, __FILE__, mode)
    abort "#{mode}: #{stderr}" unless status.success?
    stdout
  end
  outputs = [run.call("native"), run.call("emitted")]
  unless outputs.uniq.length == 1
    a, b = outputs.map(&:lines)
    diff = a.zip(b).each_with_index.reject { |(x, y), _| x == y }.first(12)
    abort "types differ (#{a.length} native lines, #{b.length} emitted):\n" +
      diff.map { |(x, y), i| "line #{i + 1}\n  native:  #{x}  emitted: #{y}" }.join("\n")
  end
  puts outputs.first
  puts "Native ActiveModel and the emitted ActiveModel types agree"
  exit
end

if ARGV.first == "native"
  gem "activerecord", "8.1.4"
  gem "activemodel", "8.1.4"
  require "active_record"
  # Touch the autoloads the port defines eagerly.
  ActiveRecord::Type::Text
  ActiveRecord::Type::DateTime
  ActiveModel::Type::ImmutableString
else
  # The tree's load order: rails.rb, the CRuby overlay's core
  # extensions, then the port (gem_facades.rb requires it where the app
  # names a type).
  root = File.expand_path("..", __dir__)
  require File.join(root, "runtime/ruby/rails")
  require File.join(root, "runtime/spinel/scaffold/ruby_overlay/runtime/active_support_core_ext")
  require File.join(root, "runtime/spinel/active_model_type")
end
require "bigdecimal"
require "date"

# ActiveSupport reopens Date / Time / DateTime#inspect, so times print
# by their parts, which both sides compute the same way.
def describe(value)
  case value
  when ::String
    "#{value.inspect.gsub(/0x\h+/, "0x")} #{value.encoding} frozen=#{value.frozen?}"
  when ::DateTime
    "DateTime(#{value.iso8601(9)})"
  when ::Date
    "Date(#{value.iso8601})"
  when ::Time
    "Time(#{value.strftime("%Y-%m-%d %H:%M:%S.%N %z")} utc=#{value.utc?} zone=#{value.zone.inspect})"
  when ::Array
    "[#{value.map { |v| describe(v) }.join(", ")}]"
  else
    value.inspect.gsub(/0x\h+/, "0x")
  end
end

def show(label, value)
  puts "#{label}: #{describe(value)}"
end

def outcome
  describe(yield)
rescue Exception => e
  "#{e.class}: #{e.message.lines.first.to_s.strip.gsub(/0x\h+/, "0x")}"
end

AM = ActiveModel::Type
AR = ActiveRecord::Type

TYPES = {
  "AR::Integer(limit: 8)" => -> { AR::Integer.new(limit: 8) },
  "AR::Integer" => -> { AR::Integer.new },
  "AR::Integer(limit: 2)" => -> { AR::Integer.new(limit: 2) },
  "AR::String(limit: 255)" => -> { AR::String.new(limit: 255) },
  "AR::String(limit: 12)" => -> { AR::String.new(limit: 12) },
  "AR::String(limit: 20)" => -> { AR::String.new(limit: 20) },
  "AR::String(true: 1, false: 0)" => -> { AR::String.new(true: 1, false: 0) },
  "AR::ImmutableString" => -> { AR::ImmutableString.new },
  "AR::Text" => -> { AR::Text.new },
  "AR::DateTime(precision: 6)" => -> { AR::DateTime.new(precision: 6) },
  "AR::DateTime(precision: 0)" => -> { AR::DateTime.new(precision: 0) },
  "AR::DateTime" => -> { AR::DateTime.new },
  "AR::DateTime(timezone: :local, precision: 3)" => -> { AR::DateTime.new(timezone: :local, precision: 3) },
  "AM::DateTime(precision: 6)" => -> { AM::DateTime.new(precision: 6) },
  "AR::Boolean" => -> { AR::Boolean.new },
  "AR::Value(precision: 1, scale: 2, limit: 3)" => -> { AR::Value.new(precision: 1, scale: 2, limit: 3) },
}

TIME = Time.utc(2020, 6, 20, 10, 20, 30, Rational(123_456_789, 1000))
LOCAL = Time.new(2020, 6, 20, 10, 20, 30.5r, "+09:00")

VALUES = [
  nil, "", " ", "\t\n", "0", "12", " 12 ", "+12", "-7", "12abc", "abc", "1.7", "1e3",
  "99999999999999999999", "2147483648", "-2147483648", "32767",
  0, 1, -1, 12, 2**15, 2**31, -(2**31), 2**31 - 1, 2**63, -(2**63), 2**63 - 1,
  1.7, -1.2, Float::INFINITY, Float::NAN, Rational(7, 2),
  true, false, :sym, :"0", :f, :off, :FALSE, "f", "F", "false", "off", "OFF", "t", "true", "on", "yes", "no",
  [], [1], {}, Object.new.tap { |o| def o.inspect = "#<Object>" },
  TIME, LOCAL, Time.at(0), Date.new(2020, 6, 20),
  "2020-06-20T10:20:30.123456789Z", "2020-06-20 10:20:30+09:00", "2020-06-20 10:20:30.5-05:30",
  "2020-06-20 10:20:30", "2020-06-20T10:20:30Z", "2020-06-20", "June 20 2020 10:20", "20 Jun 2020",
  "2020-13-45 10:00:00", "0000-00-00 00:00:00", "10:20", "2020-06-20T25:00:00", "garbage-text",
  { 1 => 2020, 2 => 6, 3 => 20 }, { 1 => 2020, 2 => 6, 3 => 20, 4 => 10, 5 => 20, 6 => 30 },
  { 1 => 2020, 2 => 6 }, { 1 => nil, 2 => 6, 3 => 20 }, { 1 => 2020, 2 => 2, 3 => 30 },
]

def label_of(value)
  value.is_a?(::String) ? value.inspect : describe(value)
end

TYPES.each do |name, build|
  type = build.call
  show "#{name} class", type.class.name
  show "#{name} ancestors", type.class.ancestors.take_while { |m| m != Object }.map(&:inspect)
  show "#{name} type", type.type
  show "#{name} limit/precision/scale", [type.limit, type.precision, type.scale]
  show "#{name} mutable/binary/serialized", [type.mutable?, type.binary?, type.serialized?]
  show "#{name} == same", type == build.call
  show "#{name} eql same", type.eql?(build.call)
  show "#{name} hash same", type.hash == build.call.hash
  show "#{name} serialize_cast_value_compatible", type.class.serialize_cast_value_compatible?
  show "#{name} itself_if_compatible", type.itself_if_serialize_cast_value_compatible.equal?(type)
  show "#{name} try(:limit)", type.respond_to?(:limit) ? type.limit : :none
  show "#{name} as_json", outcome { type.as_json }
  VALUES.each do |value|
    v = label_of(value)
    copy = -> { value.is_a?(::String) ? value.dup : (value.is_a?(Hash) ? value.dup : value) }
    puts "#{name} cast #{v}: #{outcome { type.cast(copy.call) }}"
    puts "#{name} serialize #{v}: #{outcome { type.serialize(copy.call) }}"
    puts "#{name} deserialize #{v}: #{outcome { type.deserialize(copy.call) }}"
    puts "#{name} SerializeCastValue.serialize(cast) #{v}: #{outcome { AM::SerializeCastValue.serialize(type, type.cast(copy.call)) }}"
    puts "#{name} serializable? #{v}: #{outcome { yielded = []; [type.serializable?(copy.call) { |x| yielded << x }, yielded] }}"
    puts "#{name} assert_valid_value #{v}: #{outcome { type.assert_valid_value(copy.call) }}"
    puts "#{name} by mass assignment #{v}: #{outcome { type.value_constructed_by_mass_assignment?(value) }}"
    puts "#{name} changed?(1, x, x) #{v}: #{outcome { type.changed?(1, value, value) }}"
    puts "#{name} changed?(nil, x, x) #{v}: #{outcome { type.changed?(nil, value, value) }}"
    puts "#{name} changed?(x, x, x) #{v}: #{outcome { type.changed?(value, value, value) }}"
    puts "#{name} changed_in_place?(\"12\", x) #{v}: #{outcome { type.changed_in_place?("12", value) }}"
  end
end

# Equality across option sets and classes.
show "Integer limit 8 vs 4", AR::Integer.new(limit: 8) == AR::Integer.new
show "String vs Text", AR::String.new == AR::Text.new
show "String vs ImmutableString", AR::String.new == AR::ImmutableString.new
show "AR::DateTime vs AM::DateTime", AR::DateTime.new == AM::DateTime.new
show "AR::DateTime timezone", AR::DateTime.new(timezone: :local) == AR::DateTime.new(timezone: :utc)
show "AR::DateTime same timezone", AR::DateTime.new(timezone: :local) == AR::DateTime.new(timezone: :local)
show "hash differs by limit", AR::Integer.new(limit: 8).hash == AR::Integer.new.hash
show "aliases", [AR::Integer.equal?(AM::Integer), AR::String.equal?(AM::String), AR::Boolean.equal?(AM::Boolean), AR::Value.equal?(AM::Value), AR::ImmutableString.equal?(AM::ImmutableString)]
show "Text superclass", AR::Text.superclass.name
show "AR::DateTime superclass", AR::DateTime.superclass.name
show "RangeError ancestry", ActiveModel::RangeError.ancestors.take_while { |m| m != Exception }.map(&:name)
show "FALSE_VALUES", AM::Boolean::FALSE_VALUES.to_a
show "DEFAULT_LIMIT", AM::Integer::DEFAULT_LIMIT
show "to_immutable_string", outcome { s = AR::String.new(true: "y", false: "n", limit: 9).to_immutable_string; [s.class.name, s.limit, s.cast(true), s.cast(false)] }
show "Value cast_value private", AR::Value.private_method_defined?(:cast_value)
show "Value map", AR::Value.new.map(5) { |x| x * 2 }
show "Value force_equality?", AR::Value.new.force_equality?(1)
show "Value type_cast_for_schema", AR::Value.new.type_cast_for_schema("a\"b")
show "Integer type_cast_for_schema", AR::Integer.new.type_cast_for_schema(12)
show "Integer frozen string cast", outcome { AR::Integer.new.cast("12".freeze) }
show "String cast frozen", outcome { AR::String.new.cast("ab".freeze) }
show "ImmutableString cast frozen", outcome { AR::ImmutableString.new.cast(+"ab") }
show "String cast same object", outcome { s = +"ab"; AR::String.new.cast(s).equal?(s) }
show "DateTime multiparameter defaults", outcome { AR::DateTime.new.cast({ 1 => 2020, 2 => 6, 3 => 20, 6 => 5 }) }
show "DateTime local multiparameter", outcome { AR::DateTime.new(timezone: :local).cast({ 1 => 2020, 2 => 6, 3 => 20, 4 => 1 }) }
show "DateTime apply_seconds_precision", outcome { AR::DateTime.new(precision: 2).apply_seconds_precision(TIME) }
show "DateTime apply_seconds_precision local", outcome { AR::DateTime.new(precision: 1).apply_seconds_precision(LOCAL + Rational(37, 1000)) }
show "DateTime serialize local zone", outcome { AR::DateTime.new(timezone: :local, precision: 6).serialize(TIME) }
show "Integer RangeError limit 8", outcome { AR::Integer.new(limit: 8).serialize(2**63) }
show "Integer serializable? yields", outcome { y = nil; [AR::Integer.new(limit: 2).serializable?("40000") { |x| y = x }, y] }
