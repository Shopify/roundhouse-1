# Native ActiveSupport oracle for ActiveSupport::Duration's `+`, `-`,
# unary `-` and `inspect` against the runtime ports: the CRuby overlay
# (runtime/spinel/scaffold/ruby_overlay/runtime/active_support_duration.rb)
# and its spinel twin (runtime/spinel/active_support_duration.rb). The
# oracle is the Rails native Core runs (tests/support/rails_oracle.rb).
# Each mode prints one line per observation; `compare` requires them equal.
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
  RailsOracle.activate("activesupport")
  require "active_support"
  require "active_support/duration"
  require "active_support/core_ext/numeric/time"
when "overlay"
  module ActiveSupport; def self.now = Time.now; end
  require_relative "../runtime/spinel/scaffold/ruby_overlay/runtime/active_support_duration"
when "spinel"
  module ActiveSupport; def self.now = Time.now; end
  require_relative "../runtime/spinel/active_support_duration"
else
  abort "mode: compare | native | overlay | spinel"
end

# `fortnights` is the class-side builder the ports carry as a lowering
# target; Rails builds it on Integer (`2.fortnights`, weeks * 2).
UNITS = %i[seconds minutes hours days weeks months years].freeze
D = ActiveSupport::Duration

def dur(unit, n) = D.public_send(unit, n)

# Natively `n.fortnights` (Integer); on the ports the class-side builder
# the lowering targets.
def fortnights(n) = ARGV.first == "native" ? n.fortnights : D.fortnights(n)

def outcome
  value = yield
  value.is_a?(D) ? "Duration #{value.inspect} #{value.to_i} #{value.to_s}" : "#{value.class} #{value.inspect}"
rescue Exception => e
  "#{e.class}: #{e.message.lines.first.to_s.strip}"
end

NS = [0, 1, 2, -1, 45, 1.5].freeze
NS.each do |n|
  UNITS.each do |unit|
    puts "#{n}.#{unit}: #{outcome { dur(unit, n) }}"
    puts "-#{n}.#{unit}: #{outcome { -dur(unit, n) }}"
    [0, 1, 30, -5, 2.5].each do |k|
      puts "#{n}.#{unit} + #{k}: #{outcome { dur(unit, n) + k }}"
      puts "#{n}.#{unit} - #{k}: #{outcome { dur(unit, n) - k }}"
    end
    UNITS.each do |other|
      [1, 2, n].uniq.each do |m|
        puts "#{n}.#{unit} + #{m}.#{other}: #{outcome { dur(unit, n) + dur(other, m) }}"
        puts "#{n}.#{unit} - #{m}.#{other}: #{outcome { dur(unit, n) - dur(other, m) }}"
      end
    end
  end
end
puts "1.day + 2.hours + 5.minutes: #{outcome { D.days(1) + D.hours(2) + D.minutes(5) }}"
puts "1.year + 1.month + 1.week + 1.day + 1.hour + 1.minute + 1.second: #{outcome { D.years(1) + D.months(1) + D.weeks(1) + D.days(1) + D.hours(1) + D.minutes(1) + D.seconds(1) }}"
puts "2.days - 2.days + 3: #{outcome { D.days(2) - D.days(2) + 3 }}"
puts "2.days + 3 - 3: #{outcome { D.days(2) + 3 - 3 }}"
puts "1.fortnight: #{outcome { fortnights(1) }}"
puts "2.fortnights + 1.week: #{outcome { fortnights(2) + D.weeks(1) }}"
puts "2.days + nil: #{outcome { D.days(2) + nil }}"
puts "2.days + 1.hour == 49.hours: #{outcome { D.days(2) + D.hours(1) == D.hours(49) }}"
puts "[1.day, 2.hours]: #{outcome { [D.days(1), D.hours(2)] }}"
puts "{ttl: 2.days}: #{outcome { { ttl: D.days(2) } }}"
