# Native ActiveSupport oracle for ActiveSupport::Duration's scaling
# operators against the runtime ports: `*`, `/` and `%` with a Duration
# receiver, and a number on the left (`2 * 30.minutes`, `3600 /
# 30.minutes`, `10 - 1.minute`, `1.5 <= 2.seconds`), which Rails answers
# through `Duration#coerce` and `Duration::Scalar`.
#
# The CRuby overlay
# (runtime/spinel/scaffold/ruby_overlay/runtime/active_support_duration.rb)
# answers every observation. Its spinel twin
# (runtime/spinel/active_support_duration.rb) carries `*` and `/` with a
# Duration receiver only, so it is compared on that section. The oracle
# is the Rails native Core runs (tests/support/rails_oracle.rb).
#
#   ruby tests/duration_scale_runtime.rb compare
require_relative "support/rails_oracle"

if ARGV.first == "compare"
  require "open3"
  run = lambda do |mode, section|
    stdout, stderr, status = Open3.capture3(RbConfig.ruby, __FILE__, mode, section)
    abort "#{mode} #{section}: #{stderr}" unless status.success?
    stdout
  end
  pairs = [["overlay", "all"], ["spinel", "receiver"]]
  natives = {}
  pairs.each do |mode, section|
    native = natives[section] ||= run.("native", section)
    out = run.(mode, section)
    next if out == native
    diff = native.lines.zip(out.lines).each_with_index.reject { |(x, y), _| x == y }.first(5)
    abort "#{mode} differs:\n" + diff.map { |(x, y), i| "line #{i + 1}\n  native: #{x}  #{mode}: #{y}" }.join("\n")
  end
  puts natives["all"]
  puts "Native ActiveSupport and the Duration ports agree (#{RailsOracle.label})"
  exit
end

case ARGV.first
when "native"
  RailsOracle.activate("activesupport")
  require "active_support"
  require "active_support/duration"
  require "active_support/core_ext/time"
when "overlay"
  module ActiveSupport; def self.now = Time.now; end
  require_relative "../runtime/spinel/scaffold/ruby_overlay/runtime/active_support_duration"
when "spinel"
  module ActiveSupport; def self.now = Time.now; end
  require_relative "../runtime/spinel/active_support_duration"
else
  abort "mode: compare | native | overlay | spinel"
end
SECTION = ARGV[1] || "all"

UNITS = %i[seconds minutes hours days weeks months years].freeze
D = ActiveSupport::Duration

def dur(unit, n) = D.public_send(unit, n)

def outcome
  value = yield
  value.is_a?(D) ? "Duration #{value.inspect} #{value.to_i} #{value.to_s}" : "#{value.class} #{value.inspect}"
rescue Exception => e
  "#{e.class}: #{e.message.lines.first.to_s.strip}"
end

FACTORS = [0, 1, 2, 3, -1, 7, 1.5, 0.5].freeze

# A Duration on the left: both ports.
[1, 2, 30, 45, -3].each do |n|
  UNITS.each do |unit|
    FACTORS.each do |k|
      puts "#{n}.#{unit} * #{k}: #{outcome { dur(unit, n) * k }}"
      puts "#{n}.#{unit} / #{k}: #{outcome { dur(unit, n) / k }}" unless k == 0
    end
    UNITS.each do |other|
      puts "#{n}.#{unit} / 1.#{other}: #{outcome { dur(unit, n) / dur(other, 1) }}"
      puts "#{n}.#{unit} / 7.#{other}: #{outcome { dur(unit, n) / dur(other, 7) }}"
    end
  end
end
puts "(1.day + 2.hours) * 3: #{outcome { (D.days(1) + D.hours(2)) * 3 }}"
puts "(1.day + 2.hours) / 2: #{outcome { (D.days(1) + D.hours(2)) / 2 }}"
puts "(30.minutes + 5) * 2: #{outcome { (D.minutes(30) + 5) * 2 }}"
puts "1.hour * 1.minute: #{outcome { D.hours(1) * D.minutes(1) }}"
puts "1.hour * nil: #{outcome { D.hours(1) * nil }}"
puts "1.hour * \"2\": #{outcome { D.hours(1) * "2" }}"
puts "1.hour / \"2\": #{outcome { D.hours(1) / "2" }}"
puts "1.hour / 0: #{outcome { D.hours(1) / 0 }}"
puts "1.hour / 0.0: #{outcome { D.hours(1) / 0.0 }}"
puts "(30.minutes * 2).ago(Time.utc(2024, 1, 1)): #{outcome { (D.minutes(30) * 2).ago(Time.utc(2024, 1, 1)) }}"
puts "(1.month / 2).since(Time.utc(2024, 1, 31)): #{outcome { (D.months(1) / 2).since(Time.utc(2024, 1, 31)) }}"
puts "(2.days * 2).since(Time.utc(2024, 1, 31)): #{outcome { (D.days(2) * 2).since(Time.utc(2024, 1, 31)) }}"
exit if SECTION == "receiver"

# `%` and a number on the left: the overlay (Scalar), not the twin.
[1, 2, 30, 45, -3].each do |n|
  UNITS.each do |unit|
    [1, 7, 25, 3600, 86_400, 1.5].each do |k|
      puts "#{n}.#{unit} % #{k}: #{outcome { dur(unit, n) % k }}"
    end
    UNITS.each do |other|
      puts "#{n}.#{unit} % 7.#{other}: #{outcome { dur(unit, n) % dur(other, 7) }}"
    end
    [0, 2, -1, 1.5, 3600, 10].each do |k|
      puts "#{k} * #{n}.#{unit}: #{outcome { k * dur(unit, n) }}"
      puts "#{k} + #{n}.#{unit}: #{outcome { k + dur(unit, n) }}"
      puts "#{k} - #{n}.#{unit}: #{outcome { k - dur(unit, n) }}"
      puts "#{k} / #{n}.#{unit}: #{outcome { k / dur(unit, n) }}"
      puts "#{k} % #{n}.#{unit}: #{outcome { k % dur(unit, n) }}"
      puts "#{k} <=> #{n}.#{unit}: #{outcome { k <=> dur(unit, n) }}"
      puts "#{k} <= #{n}.#{unit}: #{outcome { k <= dur(unit, n) }}"
      puts "#{k} > #{n}.#{unit}: #{outcome { k > dur(unit, n) }}"
      puts "#{k} == #{n}.#{unit}: #{outcome { k == dur(unit, n) }}"
    end
  end
end
puts "Duration.build(0): #{outcome { D.build(0) }}"
puts "Duration.build(31556952): #{outcome { D.build(31_556_952) }}"
puts "Duration.build(2716146): #{outcome { D.build(2_716_146) }}"
puts "Duration.build(-90061): #{outcome { D.build(-90_061) }}"
puts "Duration.build(90061.5): #{outcome { D.build(90_061.5) }}"
puts "Duration.build(\"1\"): #{outcome { D.build("1") }}"
puts "+1.hour: #{outcome { +D.hours(1) }}"
puts "1.day.positive?: #{outcome { D.days(1).positive? }}"
puts "-1.day.negative?: #{outcome { (-D.days(1)).negative? }}"
puts "1.day.to_r: #{outcome { D.days(1).to_r }}"
puts "1.day.respond_to?(:to_r): #{outcome { D.days(1).respond_to?(:to_r) }}"
puts "1.day.respond_to?(:upcase): #{outcome { D.days(1).respond_to?(:upcase) }}"
puts "1.day.upcase: #{outcome { D.days(1).upcase }}"
puts "Time.at(0 + 1.hour): #{outcome { Time.at(0 + D.hours(1)).utc }}"
puts "(2 * 30.minutes).ago(Time.utc(2024, 1, 1)): #{outcome { (2 * D.minutes(30)).ago(Time.utc(2024, 1, 1)) }}"
puts "Time.utc(2024, 1, 1).to_f - Time.utc(2023, 12, 1).to_f <= 14.days: #{outcome { Time.utc(2024, 1, 1).to_f - Time.utc(2023, 12, 1).to_f <= D.days(14) }}"
puts "[2.hours, 30.minutes, 1.day].sort: #{outcome { [D.hours(2), D.minutes(30), D.days(1)].sort }}"
puts "[2.hours, 30.minutes].sum(0): #{outcome { [D.hours(2), D.minutes(30)].sum(0) }}"
puts "[2.hours, 30.minutes].max: #{outcome { [D.hours(2), D.minutes(30)].max }}"
