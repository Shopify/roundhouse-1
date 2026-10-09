# Native Rails oracle (the Rails native Core runs; tests/support/
# rails_oracle.rb) for Integer#months / #years (core_ext/integer/time)
# and Time#change (core_ext/time/calculations) against
# runtime/spinel/active_support_integer_time.rb and
# active_support_time_change.rb.
#
#   ruby tests/active_support_integer_time_change_runtime.rb compare
#
# runs this script twice: `native` requires the pinned activesupport's
# features; `emitted` lays the ports out as the emitted PostgreSQL tree's
# runtime/ does (with the CRuby ActiveSupport::Duration overlay) and loads
# them. Each prints one line per observation; `compare` requires them equal.
require_relative "support/rails_oracle"
require "tmpdir"
require "fileutils"

ROOT = File.expand_path("..", __dir__)

if ARGV.first == "compare"
  require "open3"
  outputs = ["native", "emitted"].map do |mode|
    env = mode == "emitted" ? RailsOracle.core_gems_env : {}
    env = env.merge("TZ" => "EST5EDT,M3.2.0,M11.1.0")
    stdout, stderr, status = Open3.capture3(env, RbConfig.ruby, File.expand_path(__FILE__), mode)
    abort "#{mode}: #{stderr}" unless status.success?
    stdout
  end
  native, emitted = outputs.map(&:lines)
  equal = native.zip(emitted).count { |x, y| x == y }
  unless outputs.uniq.length == 1
    diff = native.zip(emitted).each_with_index.reject { |(x, y), _| x == y }.first(8)
    abort "ports differ (#{equal}/#{native.length} observations equal, emitted has #{emitted.length}):\n" +
      diff.map { |(x, y), i| "line #{i + 1}\n  native:  #{x}  emitted: #{y}" }.join("\n")
  end
  puts outputs.first
  puts "observations: #{equal}/#{native.length} equal"
  puts "Native Rails and emitted Integer#year / Time#change agree (#{RailsOracle.label})"
  exit
end

MODE = ARGV.first
abort "usage: #{$PROGRAM_NAME} compare | native | emitted" unless %w[native emitted].include?(MODE)

if MODE == "native"
  RailsOracle.activate("activesupport")
  require "active_support"
  require "active_support/core_ext/integer/time"
  require "active_support/core_ext/time/calculations"
else
  runtime = File.join(Dir.mktmpdir("rh-integer-time"), "runtime")
  FileUtils.mkdir_p(runtime)
  FileUtils.cp(File.join(ROOT, "runtime/spinel/scaffold/ruby_overlay/runtime/active_support_duration.rb"), runtime)
  %w[active_support_numeric_time active_support_integer_time active_support_time_change].each do |port|
    FileUtils.cp(File.join(ROOT, "runtime/spinel/#{port}.rb"), runtime)
  end
  require File.join(runtime, "active_support_integer_time")
  require File.join(runtime, "active_support_time_change")
end

def show(label, value)
  puts "#{label}: #{value.inspect}"
end

def outcome
  value = yield
  value.is_a?(Time) ? "#{value.strftime("%Y-%m-%d %H:%M:%S.%N %z")} utc=#{value.utc?} zone=#{value.zone.inspect}" : value.inspect
rescue Exception => e
  "#{e.class}: #{e.message.lines.first.chomp.inspect}"
end

show("Integer#year owner", Integer.instance_method(:year).owner)
show("Integer#month == #months", Integer.instance_method(:month) == Integer.instance_method(:months))
show("Float responds to year", 1.5.respond_to?(:year))
[[1, :year], [2, :years], [1, :month], [3, :months], [0, :year], [-1, :month]].each do |n, m|
  d = n.public_send(m)
  show("#{n}.#{m}", [d.class.name, d.inspect, d.to_i])
end
show("1.year + 2.months", (1.year + 2.months).to_i)
show("TTL_CASES", [{ expected_ttl: 1.year.to_i }, { expected_ttl: 10.minutes.to_i }])

utc = Time.utc(2024, 1, 31, 13, 45, 30, Rational(123456789, 1000))
local = Time.local(2024, 11, 3, 1, 30, 15)
fixed = Time.new(2024, 3, 10, 2, 30, 0, "+05:30")
show("Time#change owner", Time.instance_method(:change).owner)
{
  "utc nsec 0" => [utc, { nsec: 0 }],
  "utc usec" => [utc, { usec: 5 }],
  "utc hour" => [utc, { hour: 5 }],
  "utc min" => [utc, { min: 7 }],
  "utc sec" => [utc, { sec: 1 }],
  "utc year" => [utc, { year: 2020 }],
  "utc month" => [utc, { month: 2 }],
  "utc day" => [utc, { day: 1 }],
  "utc offset" => [utc, { offset: "+05:00" }],
  "utc nsec and usec" => [utc, { nsec: 1, usec: 1 }],
  "utc usec out of range" => [utc, { usec: 1_000_000 }],
  "utc empty" => [utc, {}],
  "local day" => [local, { day: 1 }],
  "local hour" => [local, { hour: 1 }],
  "local nsec" => [local, { nsec: 0 }],
  "fixed min" => [fixed, { min: 3 }],
  "fixed month" => [fixed, { month: 7 }],
}.each do |label, (time, options)|
  show(label, outcome { time.change(options) })
end
show("Time.now.utc.change(nsec: 0) nsec", Time.now.utc.change(nsec: 0).nsec)
