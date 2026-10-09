# Native ActiveSupport oracle for ActiveSupport::Duration's class
# predicates (`is_a?`, `kind_of?`, `instance_of?`, `Duration.===`,
# `Module#===`) and for sorbet's inline check over them (`T.let(d,
# Integer)`), against the runtime ports: the CRuby overlay
# (runtime/spinel/scaffold/ruby_overlay/runtime/active_support_duration.rb)
# and its spinel twin (runtime/spinel/active_support_duration.rb), each
# with the overlay's SorbetRuntimeCheck, which is what the ingest lowers
# `T.let` to. The oracle is the Rails native Core runs
# (tests/support/rails_oracle.rb) with Core's sorbet-runtime 0.6.13502.
# Each mode prints one line per observation; `compare` requires them
# equal. Core's ShopifyPayments::Capability has
# `CACHE_TTL = T.let(10.minutes, Integer)`, which loads at boot.
require_relative "support/rails_oracle"

if ARGV.first == "compare"
  require "open3"
  modes = ["native", "overlay", "spinel"]
  outputs = modes.map do |mode|
    stdout, stderr, status = Open3.capture3(RbConfig.ruby, __FILE__, mode)
    abort "#{mode}: #{stderr}" unless status.success?
    stdout
  end
  modes.zip(outputs).drop(1).each do |mode, out|
    next if out == outputs.first
    diff = outputs.first.lines.zip(out.lines).each_with_index.reject { |(x, y), _| x == y }.first(5)
    abort "#{mode} differs:\n" + diff.map { |(x, y), i| "line #{i + 1}\n  native: #{x}  #{mode}: #{y}" }.join("\n")
  end
  puts outputs.first
  puts "Native ActiveSupport and the Duration ports agree (#{RailsOracle.label})"
  exit
end

case ARGV.first
when "native"
  RailsOracle.activate("activesupport", "sorbet-runtime": "0.6.13502")
  require "active_support"
  require "active_support/duration"
  require "sorbet-runtime"
  # `T.let(value, type)` itself; `type` is a class or a [:nilable, X] /
  # [:any, X, Y] form.
  def tlet(value, type)
    case type
    in [:nilable, inner] then T.let(value, T.nilable(inner))
    in [:any, *types] then T.let(value, T.any(*types))
    else T.let(value, type)
    end
  end
when "overlay", "spinel"
  module ActiveSupport; def self.now = Time.now; end
  if ARGV.first == "overlay"
    require_relative "../runtime/spinel/scaffold/ruby_overlay/runtime/active_support_duration"
  else
    require_relative "../runtime/spinel/active_support_duration"
  end
  require_relative "../runtime/spinel/scaffold/ruby_overlay/runtime/sorbet_runtime_check"
  # What the ingest lowers `T.let(value, type)` to.
  def tlet(value, type)
    type =
      case type
      in [:nilable, inner] then SorbetRuntimeCheck.nilable(inner)
      in [:any, *types] then SorbetRuntimeCheck.any(*types)
      else type
      end
    SorbetRuntimeCheck.cast(value, type, "T.let", "probe.rb", 1)
  end
else
  abort "mode: compare | native | overlay | spinel"
end

def outcome
  value = yield
  "#{value.class} #{value.inspect}"
rescue Exception => e
  "#{e.class}: #{e.message.lines.first.to_s.strip}"
end

D = ActiveSupport::Duration
VALUES = {
  "minutes(10)" => -> { D.minutes(10) },
  "seconds(0)" => -> { D.seconds(0) },
  "hours(1.5)" => -> { D.hours(1.5) },
  "minutes(1/2r)" => -> { D.minutes(Rational(1, 2)) },
  "days(2) + seconds(30)" => -> { D.days(2) + D.seconds(30) },
  "-weeks(1)" => -> { -D.weeks(1) },
}.freeze
CLASSES = [Integer, Float, Rational, Numeric, Comparable, Object, BasicObject, Kernel, D, String, Time, NilClass].freeze

VALUES.each do |label, build|
  CLASSES.each do |klass|
    puts "#{label}.is_a?(#{klass}): #{outcome { build.call.is_a?(klass) }}"
    puts "#{label}.kind_of?(#{klass}): #{outcome { build.call.kind_of?(klass) }}"
    puts "#{label}.instance_of?(#{klass}): #{outcome { build.call.instance_of?(klass) }}"
    puts "#{klass} === #{label}: #{outcome { klass === build.call }}"
  end
end

# `Duration.===` asks the operand; a BasicObject has no `is_a?`.
{ "600" => 600, "600.0" => 600.0, "\"x\"" => "x", "nil" => nil, "BasicObject.new" => BasicObject.new,
  "minutes(10)" => D.minutes(10), "[minutes(10)]" => [D.minutes(10)] }.each do |label, value|
  puts "Duration === #{label}: #{outcome { D === value }}"
end
puts "case minutes(10): #{outcome { case D.minutes(10) when Integer then :integer when D then :duration end }}"

# sorbet's inline check reads `is_a?`, so a Duration passes for its value's class.
TYPES = { "Integer" => Integer, "Float" => Float, "Numeric" => Numeric, "Duration" => D, "String" => String,
          "T.nilable(Integer)" => [:nilable, Integer], "T.nilable(String)" => [:nilable, String],
          "T.any(String, Integer)" => [:any, String, Integer], "T.any(String, Symbol)" => [:any, String, Symbol] }.freeze
VALUES.each do |label, build|
  TYPES.each do |tname, type|
    puts "T.let(#{label}, #{tname}): #{outcome { tlet(build.call, type) }}"
  end
end
