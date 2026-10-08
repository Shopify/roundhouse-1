# Native ActiveSupport oracle for ActiveSupport::Duration and Time
# arithmetic as Core uses it: `Time + Duration` / `Time - Duration`,
# `Duration#since`/`ago` and their aliases with calendar parts (and the
# order the parts were added in), `to_i` with calendar parts, and
# `==`/`<=>`/`eql?`/`hash`/`<` between durations of mixed parts and
# against numbers. Ports: the CRuby overlay
# (runtime/spinel/scaffold/ruby_overlay/runtime/active_support_duration.rb)
# and its spinel twin (runtime/spinel/active_support_duration.rb). The
# oracle is the Rails native Core runs (tests/support/rails_oracle.rb).
# `compare` runs every mode under each TZ below and requires the lines
# equal. Lines marked `T ` are `Time + Duration` / `Time - Duration`:
# reopening Time's operators is CRuby-only, so the spinel twin does not
# print them (the emit grounds those sites on that lane).
require_relative "support/rails_oracle"

ZONES = ["UTC", "EST5EDT,M3.2.0,M11.1.0", "CET-1CEST,M3.5.0,M10.5.0/3"].freeze

if ARGV.first == "compare"
  require "open3"
  modes = ["native", "overlay", "spinel"]
  all = []
  ZONES.each do |zone|
    outputs = modes.map do |mode|
      stdout, stderr, status = Open3.capture3({ "TZ" => zone }, RbConfig.ruby, __FILE__, mode)
      abort "#{mode} (TZ=#{zone}): #{stderr}" unless status.success?
      stdout
    end
    native = outputs.first
    modes.zip(outputs).drop(1).each do |mode, out|
      want = mode == "spinel" ? native.lines.reject { |l| l.start_with?("T ") }.join : native
      next if out == want
      diff = want.lines.zip(out.lines).each_with_index.reject { |(x, y), _| x == y }.first(5)
      abort "#{mode} (TZ=#{zone}) differs:\n" + diff.map { |(x, y), i| "line #{i + 1}\n  native: #{x}  #{mode}: #{y}" }.join("\n")
    end
    all << "TZ=#{zone}\n" << native
  end
  puts all.join
  puts "Native ActiveSupport and the Duration ports agree (#{RailsOracle.label})"
  exit
end

require "date"
case ARGV.first
when "native"
  RailsOracle.activate("activesupport")
  require "active_support"
  require "active_support/duration"
  require "active_support/core_ext/time"
  require "active_support/core_ext/date"
when "overlay"
  module ActiveSupport; def self.now = Time.now; end
  require_relative "../runtime/spinel/scaffold/ruby_overlay/runtime/active_support_duration"
when "spinel"
  module ActiveSupport; def self.now = Time.now; end
  require_relative "../runtime/spinel/active_support_duration"
else
  abort "mode: compare | native | overlay | spinel"
end
SPINEL = ARGV.first == "spinel"

D = ActiveSupport::Duration
UNITS = %i[seconds minutes hours days weeks months years].freeze

def show(value)
  case value
  when Time then "Time #{value.inspect} #{value.utc_offset} #{value.isdst.inspect}"
  when D then "Duration #{value.inspect} #{value.to_i}"
  # activesupport reopens Date#inspect; the ports leave it alone.
  when Date then "#{value.class} #{value.iso8601} #{value.day_fraction.inspect}"
  else "#{value.class} #{value.inspect}"
  end
end

def outcome
  show(yield)
rescue Exception => e
  "#{e.class}: #{e.message.lines.first.to_s.strip}"
end

TIMES = {
  "utc jan31" => Time.utc(2024, 1, 31, 12, 30, 15.5r),
  "utc leap" => Time.utc(2024, 2, 29),
  "utc year end" => Time.utc(2023, 12, 31, 23, 59, 59),
  "offset -05:00" => Time.new(2024, 1, 31, 23, 0, 0, "-05:00"),
  "offset +05:30" => Time.new(2024, 3, 30, 1, 15, 0, "+05:30"),
  "local mar9" => Time.local(2024, 3, 9, 12),
  "local mar30" => Time.local(2024, 3, 30, 2, 30),
  "local nov2" => Time.local(2024, 11, 2, 1, 30),
  "local nov3 first" => Time.local(2024, 11, 3, 1, 30),
  "local nov3 second" => Time.local(2024, 11, 3, 1, 30) + 3600,
  "local oct26" => Time.local(2024, 10, 26, 2, 30),
}.freeze

DURATIONS = {}
[1, 2, -1, 13, 1.5, -1.5, 0.25].each do |n|
  UNITS.each { |unit| DURATIONS["#{n}.#{unit}"] = D.public_send(unit, n) }
end
DURATIONS.merge!(
  "1.month + 1.day" => D.months(1) + D.days(1),
  "1.day + 1.month" => D.days(1) + D.months(1),
  "1.year + 2.months + 3.days + 4.hours" => D.years(1) + D.months(2) + D.days(3) + D.hours(4),
  "4.hours + 3.days + 2.months + 1.year" => D.hours(4) + D.days(3) + D.months(2) + D.years(1),
  "1.day + 5" => D.days(1) + 5,
  "5 + 1.day order" => D.seconds(5) + D.days(1),
  "2.weeks - 3.hours" => D.weeks(2) - D.hours(3),
  "1.day - 1.day" => D.days(1) - D.days(1),
  "-(1.month + 1.day)" => -(D.months(1) + D.days(1)),
  "1.month - 1.day" => D.months(1) - D.days(1),
)

TIMES.each do |tname, t|
  DURATIONS.each do |dname, d|
    %i[since ago from_now until after before].each do |move|
      puts "#{dname}.#{move}(#{tname}): #{outcome { d.public_send(move, t) }}"
    end
    next if SPINEL
    puts "T #{tname} + #{dname}: #{outcome { t + d }}"
    puts "T #{tname} - #{dname}: #{outcome { t - d }}"
  end
end

date = Date.new(2024, 1, 31)
DURATIONS.each do |dname, d|
  puts "#{dname}.since(date): #{outcome { d.since(date) }}"
  puts "#{dname}.ago(date): #{outcome { d.ago(date) }}"
end
["x", nil, 3].each do |bad|
  puts "1.month.since(#{bad.inspect}): #{outcome { D.months(1).since(bad) }}"
end
puts "1.day.ago class: #{outcome { D.days(1).ago.class }}"
puts "1.day.ago near now: #{outcome { ((Time.now - 86400) - D.days(1).ago).abs < 5 }}"
puts "1.month.from_now near now: #{outcome { (D.months(1).from_now - Time.now) > 27 * 86400 }}"

DURATIONS.each do |dname, d|
  puts "#{dname}.to_i: #{outcome { d.to_i }}"
  puts "#{dname}.to_f: #{outcome { d.to_f }}"
end

PAIRS = {
  "1.day" => D.days(1), "24.hours" => D.hours(24), "86400.seconds" => D.seconds(86400),
  "1440.minutes" => D.minutes(1440), "1.week" => D.weeks(1), "7.days" => D.days(7),
  "1.month" => D.months(1), "30.days" => D.days(30), "1.year" => D.years(1),
  "12.months" => D.months(12), "365.days" => D.days(365), "1.5.days" => D.days(1.5),
  "36.hours" => D.hours(36), "1.month + 1.day" => D.months(1) + D.days(1),
  "1.day + 1.month" => D.days(1) + D.months(1), "0.seconds" => D.seconds(0),
  "1.day - 1.day" => D.days(1) - D.days(1),
}.freeze
OTHERS = { "86400" => 86400, "86400.0" => 86400.0, "2629746" => 2629746, "0" => 0, "\"x\"" => "x", "nil" => nil }.freeze
PAIRS.each do |aname, a|
  PAIRS.each do |bname, b|
    %i[== != <=> eql? < <= > >=].each do |op|
      puts "#{aname} #{op} #{bname}: #{outcome { a.public_send(op, b) }}"
    end
    puts "#{aname}.hash == #{bname}.hash: #{outcome { a.hash == b.hash }}"
  end
  OTHERS.each do |oname, o|
    %i[== != <=> eql? < <= > >=].each do |op|
      puts "#{aname} #{op} #{oname}: #{outcome { a.public_send(op, o) }}"
    end
    %i[== < <= > >=].each do |op|
      puts "#{oname} #{op} #{aname}: #{outcome { o.public_send(op, a) }}"
    end
  end
end
puts "sort: #{outcome { PAIRS.values.sort.map(&:inspect) }}"
puts "max: #{outcome { PAIRS.values.max }}"
puts "min: #{outcome { PAIRS.values.min }}"
puts "uniq: #{outcome { PAIRS.values.uniq.map(&:inspect) }}"
puts "hash key: #{outcome { { D.days(1) => :a }[D.hours(24)] }}"
