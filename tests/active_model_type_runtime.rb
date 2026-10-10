# Native ActiveModel / ActiveRecord 8.2.0.alpha oracle for the attribute type
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
# does not carry, not from the types. A BigDecimal RESULT (Decimal's
# cast) prints through `to_s("E")` and `precision`, which neither side
# reopens; Decimal's BigDecimal inputs are observed separately below.
#
# The process runs in a fixed non-UTC zone so the :local timezone
# branches are observed too.
ENV["TZ"] = "ABC-3" # POSIX: UTC+3, no zoneinfo needed

# The native side is the Rails Core pins: Shopify/rails 5813559f
# (8.2.0.alpha) from Core's bundle, run with Core's gem home
# (CORE_GEM_HOME, default the one bin/check-target uses) so its
# dependencies are Core's too.
CORE_GEM_HOME = ENV.fetch("CORE_GEM_HOME", "/nix/store/kr4swvh5mw01fs3kixfa9afy57zsyihg-ruby-gems-dev/lib/ruby/gems/4.0.0")
RAILS_CHECKOUT = File.join(CORE_GEM_HOME, "bundler/gems/rails-5813559f4aed")
RAILS_LIBS = %w[activesupport activemodel activerecord].map { |lib| File.join(RAILS_CHECKOUT, lib, "lib") }

def ruby_command(mode)
  return [{}, RbConfig.ruby] unless mode == "native"
  abort "native: Core's Rails checkout #{RAILS_CHECKOUT} is missing (set CORE_GEM_HOME)" unless File.directory?(RAILS_CHECKOUT)
  [{ "GEM_HOME" => CORE_GEM_HOME, "GEM_PATH" => CORE_GEM_HOME }, RbConfig.ruby, *RAILS_LIBS.flat_map { |lib| ["-I", lib] }]
end

# Loaded from the checkout, not from some other installed Rails.
def require_native_rails
  abort "native: ActiveSupport #{ActiveSupport.gem_version} from #{$LOADED_FEATURES.grep(/active_support\/version/).first}" unless
    ActiveSupport.gem_version.to_s == "8.2.0.alpha" && $LOADED_FEATURES.any? { |f| f.start_with?(RAILS_CHECKOUT) && f.end_with?("active_support/version.rb") }
end

if ARGV.first == "compare"
  require "open3"
  run = lambda do |mode|
    stdout, stderr, status = Open3.capture3(*ruby_command(mode), __FILE__, mode)
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
  require "active_record"
  require_native_rails
  # Touch the autoloads the port defines eagerly.
  ActiveRecord::Type::Text
  ActiveRecord::Type::DateTime
  ActiveModel::Type::ImmutableString
  ActiveRecord::Type::Time
  ActiveRecord::Type::Json
  require "active_record/connection_adapters/postgresql/oid"
else
  # The tree's load order: rails.rb, the CRuby overlay's core
  # extensions, then the port (gem_facades.rb requires it where the app
  # names a type).
  root = File.expand_path("..", __dir__)
  require File.join(root, "runtime/ruby/rails")
  require File.join(root, "runtime/spinel/scaffold/ruby_overlay/runtime/active_support_core_ext")
  require File.join(root, "runtime/spinel/active_model_type")
  # The JSON coder and error reporter Core's tree carries, which Type::Json
  # encodes through and reports a parse failure to.
  require File.join(root, "runtime/spinel/active_support_json")
  require File.join(root, "runtime/ruby/active_support_reporters")
end
require "bigdecimal"
require "date"
require "json"

# ActiveSupport reopens Date / Time / DateTime#inspect, so times print
# by their parts, which both sides compute the same way.
def describe(value)
  case value
  when ::Delegator
    # Type::Time::Value: Rails' ActiveSupport::Delegation::DelegateClass
    # and Ruby's differ in their own methods, not in what they wrap.
    "Delegator(#{describe(value.__getobj__)})"
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
  when ::BigDecimal
    "BigDecimal(#{value.to_s("E")} precision=#{value.precision} frozen=#{value.frozen?})"
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
  "AR::BigInteger" => -> { AR::BigInteger.new },
  "AR::BigInteger(limit: 8)" => -> { AR::BigInteger.new(limit: 8) },
  "AR::Binary" => -> { AR::Binary.new },
  "AR::Binary(limit: 16)" => -> { AR::Binary.new(limit: 16) },
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
  # shopify-money's MoneyColumn::ActiveRecordType / Money::Type build
  # `.new` with no options; the rest are the column shapes a decimal
  # column takes.
  "AR::Decimal" => -> { AR::Decimal.new },
  "AR::Decimal(precision: 20, scale: 3)" => -> { AR::Decimal.new(precision: 20, scale: 3) },
  "AR::Decimal(precision: 5)" => -> { AR::Decimal.new(precision: 5) },
  "AR::Decimal(precision: 30)" => -> { AR::Decimal.new(precision: 30) },
  "AR::Decimal(scale: 2)" => -> { AR::Decimal.new(scale: 2) },
  "AM::Decimal(precision: 10, scale: 0)" => -> { AM::Decimal.new(precision: 10, scale: 0) },
  "AR::Value(precision: 1, scale: 2, limit: 3)" => -> { AR::Value.new(precision: 1, scale: 2, limit: 3) },
  "AM::Time" => -> { AM::Time.new },
  "AR::Time" => -> { AR::Time.new },
  "AR::Time(precision: 6)" => -> { AR::Time.new(precision: 6) },
  "AR::Time(timezone: :local, precision: 0)" => -> { AR::Time.new(timezone: :local, precision: 0) },
  "AR::Json" => -> { AR::Json.new },
  "OID::Jsonb" => -> { ActiveRecord::ConnectionAdapters::PostgreSQL::OID::Jsonb.new },
}

TIME = Time.utc(2020, 6, 20, 10, 20, 30, Rational(123_456_789, 1000))
LOCAL = Time.new(2020, 6, 20, 10, 20, 30.5r, "+09:00")

VALUES = [
  nil, "", " ", "\t\n", "0", "12", " 12 ", "+12", "-7", "12abc", "abc", "1.7", "1e3",
  "99999999999999999999", "123456789012345678901234", "été", "\xFF\x00".b, "2147483648", "-2147483648", "32767",
  0, 1, -1, 12, 2**15, 2**31, -(2**31), 2**31 - 1, 2**63, -(2**63), 2**63 - 1,
  1.7, -1.2, Float::INFINITY, Float::NAN, Rational(7, 2),
  true, false, :sym, :"0", :f, :off, :FALSE, "f", "F", "false", "off", "OFF", "t", "true", "on", "yes", "no",
  AM::Binary::Data.new("ab"), [], [1], {}, Object.new.tap { |o| def o.inspect = "#<Object>" },
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
  # 8.2's schema-cache coder: a type rebuilt from its JSON on an
  # allocated (not initialized) instance answers like the original.
  puts "#{name} as_schema_json: #{outcome { type.as_schema_json }}"
  rebuilt = outcome {
    rebuilt = type.class.allocate
    rebuilt.init_from_schema_json(JSON.parse(JSON.generate(type.as_schema_json)), nil)
    [rebuilt == type, rebuilt.as_schema_json == type.as_schema_json, rebuilt.cast("12"), rebuilt.serialize(true), outcome { rebuilt.serialize(2**40) }]
  }
  puts "#{name} from schema json: #{rebuilt}"
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
show "aliases", [AR::Decimal.equal?(AM::Decimal), AR::BigInteger.equal?(AM::BigInteger), AR::Binary.equal?(AM::Binary), AR::Integer.equal?(AM::Integer), AR::String.equal?(AM::String), AR::Boolean.equal?(AM::Boolean), AR::Value.equal?(AM::Value), AR::ImmutableString.equal?(AM::ImmutableString)]
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
# QueryPredicates, the way Core's Shopify::NormalizedStringType uses it.
class NormalizedProbe < ActiveRecord::Type::String
  include ActiveRecord::Type::QueryPredicates

  def comparison_expression(expression)
    [:normalize, expression]
  end
end
class DecoratedProbe
  include ActiveRecord::Type::QueryPredicates::Decorator
  attr_reader :subtype
  def initialize(subtype) = @subtype = subtype
end
class NormalizedValueProbe
  include ActiveRecord::Type::QueryPredicates::NormalizedValueTypeDecorator
  attr_reader :cast_type
  def initialize(cast_type) = @cast_type = cast_type
end
QP = ActiveRecord::Type::QueryPredicates
show "QueryPredicates", [QP.instance_methods.sort, QP::Decorator.ancestors.map(&:name), QP::NormalizedValueTypeDecorator.ancestors.map(&:name)]
show "QueryPredicates type?", [QP.type?(NormalizedProbe.new), QP.type?(AR::String.new), QP.type?(DecoratedProbe.new(NormalizedProbe.new)), QP.type?(DecoratedProbe.new(AR::String.new)), QP.type?(NormalizedValueProbe.new(NormalizedProbe.new)), QP.type?(nil)]
show "QueryPredicates expressions", [NormalizedProbe.new.comparison_expression(:col), DecoratedProbe.new(NormalizedProbe.new).comparison_expression(:col), NormalizedValueProbe.new(NormalizedProbe.new).comparison_expression(:col), outcome { DecoratedProbe.new(AR::String.new).comparison_expression(:col) }]
show "QueryPredicates subtype", [NormalizedProbe.new.query_predicates?, NormalizedProbe.new.cast(" Ab "), NormalizedProbe.new.type, NormalizedProbe.new == AR::String.new]
show "Integer serializable? yields", outcome { y = nil; [AR::Integer.new(limit: 2).serializable?("40000") { |x| y = x }, y] }
# Binary's serialized value, the way an adapter receives it.
DATA = AM::Binary::Data
show "BigInteger superclass", AR::BigInteger.superclass.name
show "BigInteger serialize 2**80", outcome { AR::BigInteger.new.serialize(2**80) }
show "BigInteger serialize_cast_value", outcome { AR::BigInteger.new(limit: 8).serialize_cast_value(2**70) }
show "BigInteger serializable? no yield", outcome { y = :none; [AR::BigInteger.new.serializable?(2**90) { |x| y = x }, y] }
show "BigInteger SerializeCastValue", outcome { AM::SerializeCastValue.serialize(AR::BigInteger.new, 2**70) }
show "Binary::Data", outcome { d = DATA.new("é"); [d.to_s, d.to_str, d.hex, d == "é".b, d == "é", d == DATA.new("x"), d == d, d.as_json, d.to_s.encoding.name] }
show "Binary::Data of non-string", outcome { d = DATA.new(12); [d.to_s, d.hex, d == "12"] }
show "Binary serialize", outcome { s = AR::Binary.new.serialize("ab"); [s.class.name, s.to_s, s.to_s.encoding.name] }
show "Binary serialize Data", outcome { s = AR::Binary.new.serialize(DATA.new("ab")); [s.class.name, s.to_s] }
show "Binary cast Data", outcome { v = AR::Binary.new.cast(DATA.new("é")); [v, v.encoding.name] }
show "Binary changed_in_place?", [AR::Binary.new.changed_in_place?("ab", "ab"), AR::Binary.new.changed_in_place?("ab", "ac"), AR::Binary.new.changed_in_place?(DATA.new("ab"), "ab")]
# Decimal over BigDecimal inputs and its constants.
show "Decimal BIGDECIMAL_PRECISION", AM::Decimal::BIGDECIMAL_PRECISION
show "Decimal private", [:cast_value, :convert_float_to_big_decimal, :float_precision, :apply_scale].map { |m| AR::Decimal.private_method_defined?(m) }
[AR::Decimal.new, AR::Decimal.new(precision: 20, scale: 3), AR::Decimal.new(precision: 4), AR::Decimal.new(scale: 0)].each_with_index do |type, i|
  [BigDecimal("1.23456"), BigDecimal("-0.0005"), BigDecimal("123456789012345678901234.5"), BigDecimal("NaN"), BigDecimal("Infinity"), BigDecimal("-0")].each do |value|
    show "Decimal##{i} cast BigDecimal(#{value.to_s("E")})", outcome { type.cast(value) }
    show "Decimal##{i} serialize BigDecimal(#{value.to_s("E")})", outcome { type.serialize(value) }
    show "Decimal##{i} changed?(x, x, x) BigDecimal(#{value.to_s("E")})", outcome { type.changed?(value, value, value) }
  end
  show "Decimal##{i} cast to_d object", outcome { o = Object.new; def o.to_d = BigDecimal("4.56789"); type.cast(o) }
  show "Decimal##{i} cast 0.1 + 0.2", outcome { type.cast(0.1 + 0.2) }
  show "Decimal##{i} cast 2**70", outcome { type.cast(2**70) }
  show "Decimal##{i} cast Rational(1, 3)", outcome { type.cast(Rational(1, 3)) }
end
show "Decimal changed? string to number", [AR::Decimal.new.changed?(BigDecimal("1"), BigDecimal("1"), "1.0"), AR::Decimal.new.changed?(BigDecimal("1"), nil, "wibble")]
# The shape of shopify-money's MoneyColumn::ActiveRecordType: a subclass
# whose serialize hands `money.to_d` to Decimal's.
class MoneyColumnProbe < ActiveRecord::Type::Decimal
  def serialize(money)
    return unless money
    super(money.to_d)
  end
end
show "MoneyColumn-like serialize", [MoneyColumnProbe.new.serialize(nil), MoneyColumnProbe.new.serialize(12), MoneyColumnProbe.new.serialize("3.5"), MoneyColumnProbe.new.cast("7.25"), MoneyColumnProbe.new.type, MoneyColumnProbe.serialize_cast_value_compatible?]
show "Binary string binary cast keeps object", outcome { s = "ab".b; AR::Binary.new.cast(s).equal?(s) }
# Time of day and JSON, the way a time / json / jsonb column reads them.
show "Time cast clock", [AR::Time.new.cast("10:20:30.5"), AR::Time.new(precision: 0).cast("2020-06-20 23:59:59.9"), AR::Time.new.cast("25:00"), AR::Time.new.cast(" ")]
show "Time serialize class", outcome { v = AR::Time.new(precision: 6).serialize("10:20:30.123456789"); [v.is_a?(::Delegator), v.__getobj__, v.hour, v.usec] }
show "Time cast serialized", outcome { t = AR::Time.new; t.cast(t.serialize("10:20")) }
show "Json round trip", outcome { j = AR::Json.new; [j.serialize({ "a" => [1, 2.5, nil, true], :b => { "c" => "</script>" } }), j.deserialize('{"a":[1,{"b":null}]}'), j.cast({ a: 1 }), j.deserialize("not json"), j.serialize(nil)] }
show "Json changed_in_place?", [AR::Json.new.changed_in_place?('{"a":1}', { "a" => 1 }), AR::Json.new.changed_in_place?('{"a":1}', { "a" => 2 })]
show "Jsonb superclass", [ActiveRecord::ConnectionAdapters::PostgreSQL::OID::Jsonb.superclass.name, ActiveRecord::ConnectionAdapters::PostgreSQL::OID::Jsonb.new == AR::Json.new]
show "lookup time json", outcome { [ActiveRecord::Type.lookup(:time, adapter: :postgresql).class.name, ActiveRecord::Type.lookup(:json, adapter: :postgresql).class.name, ActiveModel::Type.lookup(:time).class.name] }
