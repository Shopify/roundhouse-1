# Native ActiveSupport 8.1.4 oracle for ActiveSupport::Testing::TimeHelpers
# against runtime/spinel/active_support_testing_time_helpers.rb. Each mode
# prints one line per observation; `compare` requires them equal.
#
# Both modes run in one zone (TZ=America/New_York, and natively Time.zone
# set to it, which the emitted runtime has as its process zone), so a
# Date's midnight and a String's parse land on the same instant. Values
# are shown as instants with their offsets, not as TimeWithZone or Time
# inspections, which differ by class alone.
require "json"

ZONE = "America/New_York"

if ARGV.first == "compare"
  require "open3"
  outputs = ["native", "emitted"].map do |mode|
    # TZDIR: a nix libc does not look in /usr/share/zoneinfo by itself.
    zoneinfo = ENV["TZDIR"] || "/usr/share/zoneinfo"
    stdout, stderr, status = Open3.capture3({ "TZ" => ZONE, "TZDIR" => zoneinfo }, RbConfig.ruby, __FILE__, mode)
    abort "#{mode}: #{stderr}" unless status.success?
    stdout
  end
  unless outputs.uniq.length == 1
    a, b = outputs.map(&:lines)
    diff = a.zip(b).each_with_index.reject { |(x, y), _| x == y }.first(5)
    abort "time helpers differ:\n" + diff.map { |(x, y), i| "line #{i + 1}\n  native:  #{x}  emitted: #{y}" }.join("\n")
  end
  puts outputs.first
  puts "Native ActiveSupport and the emitted TimeHelpers agree"
  exit
end

abort "run under TZ=#{ZONE}" unless ENV["TZ"] == ZONE
abort "TZ=#{ZONE} is not in effect (set TZDIR)" unless Time.utc(2001, 2, 3).getlocal.utc_offset == -5 * 3600

if ARGV.first == "native"
  gem "activesupport", "8.1.4"
  require "active_support"
  require "active_support/time"
  require "active_support/testing/time_helpers"
  Time.zone = ZONE
  def minutes(n) = n.minutes
else
  require_relative "../runtime/ruby/active_support_ext"
  require_relative "../runtime/spinel/scaffold/ruby_overlay/runtime/active_support_duration"
  require_relative "../runtime/spinel/scaffold/ruby_overlay/runtime/active_support_time_parsing"
  require_relative "../runtime/spinel/active_support_testing_time_helpers"
  def minutes(n) = ActiveSupport::Duration.new(n * 60)
end

def show(label, value)
  puts "#{label}: #{value.inspect}"
end

def outcome
  yield.inspect
rescue Exception => e
  "#{e.class}: #{e.message.strip.lines.first.to_s.strip}"
end

# An instant as both sides can print it.
def at(time)
  [time.class.name, time.to_i, time.usec, time.utc_offset, time.strftime("%Y-%m-%d %H:%M:%S %z")]
end

def clock
  {
    now: at(Time.now),
    new: at(Time.new),
    today: Date.today.iso8601,
    datetime: DateTime.now.iso8601,
    current: Time.current.to_i,
  }
end

def stubs_left
  [Time, Date, DateTime].sum { |k| k.singleton_class.instance_methods(false).count { |m| m.to_s.start_with?("__simple_stub__") } }
end

# Minitest::Test's lifecycle hook, so TimeHelpers#after_teardown's super
# has somewhere to go natively.
module Lifecycle
  def after_teardown
    LOG << :lifecycle_after_teardown
  end
end
LOG = []

class Host
  include Lifecycle
  include ActiveSupport::Testing::TimeHelpers
end

host = Host.new
real_start = Time.now.to_i
target = Time.new(2001, 2, 3, 4, 5, 6.789r)

show "module", ActiveSupport::Testing::TimeHelpers.instance_methods(false).sort
show "private", ActiveSupport::Testing::TimeHelpers.private_instance_methods(false).sort
show "stub struct", ActiveSupport::Testing::SimpleStubs::Stub.members

host.travel_to(target)
show "travel_to", clock
sleep 0.02
show "frozen", Time.now == Time.now && at(Time.now) == at(Time.at(target.to_i))
show "stubs", stubs_left
show "time new args", at(Time.new(2010, 5, 6, 7, 8, 9))
show "time new utc args", at(Time.new(2010, 5, 6, 7, 8, 9, "+00:00"))
show "time at", at(Time.at(0))
show "date new", Date.new(2000, 1, 1).iso8601
host.travel_back
show "travel_back stubs", stubs_left
show "back to real", Time.now.to_i >= real_start

host.travel_to(target, with_usec: true)
show "with_usec", at(Time.now)
host.travel_back

host.travel_to(Date.new(2004, 11, 24))
show "date target", clock
host.travel_to(DateTime.new(2004, 11, 24, 1, 4, 44, "+05:00"))
show "datetime target", clock
host.travel_to("2004-11-24 01:04:44")
show "string target", clock
host.travel_to("2004-11-24T01:04:44Z")
show "string utc target", clock
host.travel_to(Time.utc(2020, 3, 8, 7, 30))
show "utc time target", clock
host.travel_to(Date.new(2020, 3, 8))
show "dst day midnight", clock
host.travel_to("2020-03-08 03:30:00")
show "after spring forward", clock
host.travel_to("2020-11-01 01:30:00 -0400")
show "fall back with offset", clock
host.travel_to(Date.new(2020, 7, 4))
show "summer date", clock
show "restub stubs", stubs_left
host.travel_back

host.travel_to(target)
host.travel(60)
show "travel seconds", at(Time.now)
host.travel(minutes(90))
show "travel duration", at(Time.now)
host.travel(-3600)
show "travel back an hour", at(Time.now)
host.travel_back

host.travel_to(target)
inside = host.travel_to(Time.utc(1999, 12, 31, 23, 59, 59)) { [at(Time.now), :block_value] }
show "block value", inside
show "after block restores earlier stub", at(Time.now)
show "nested block", outcome { host.travel_to(target) { host.travel_to(target) { :never } } }
show "after nested", at(Time.now)
show "travel block", host.travel(120) { at(Time.now) }
show "after travel block", at(Time.now)
host.travel_back

show "block without stub", host.travel_to(target) { at(Time.now) }
show "unstubbed after block", [stubs_left, Time.now.to_i >= real_start]

host.travel_to(target)
inner = host.travel_back { [stubs_left, Time.now.to_i >= real_start] }
show "travel_back block", inner
show "after travel_back block", [at(Time.now), stubs_left]
host.travel_back

host.freeze_time
first = Time.now
sleep 0.02
show "freeze_time", [Time.now == first, first.usec, stubs_left]
host.unfreeze_time
show "unfreeze_time", stubs_left
show "freeze_time target", host.freeze_time(target) { at(Time.now) }
show "freeze_time usec", host.freeze_time(target, with_usec: true) { at(Time.now) }

host.travel_to(target)
LOG.clear
host.after_teardown
show "after_teardown", [LOG, stubs_left, Time.now.to_i >= real_start]

other = Host.new
host.travel_to(target)
show "second host sees stub", at(Time.now)
other.travel_to(Time.utc(2030, 1, 1))
show "second host restubs", at(Time.now)
other.travel_back
show "first host's stub", stubs_left
host.travel_back
show "all back", stubs_left

show "travel_to nil", outcome { host.travel_to(nil) }
show "stubs after error", stubs_left
host.travel_back
show "integer target", outcome { host.travel_to(1_000_000_000) }
host.travel_back
