# Native ActiveSupport oracle for ActiveSupport::Duration's reader surface
# (`to_i`, `to_f`, `in_seconds`, `in_minutes` .. `in_years`) against the
# runtime ports: the CRuby overlay
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
when "overlay"
  module ActiveSupport; def self.now = Time.now; end
  require_relative "../runtime/spinel/scaffold/ruby_overlay/runtime/active_support_duration"
when "spinel"
  module ActiveSupport; def self.now = Time.now; end
  require_relative "../runtime/spinel/active_support_duration"
else
  abort "mode: compare | native | overlay | spinel"
end

# No `fortnights`: Rails builds it on Integer only (`2.fortnights`), and
# the ports carry the class-side builder as a lowering target.
UNITS = %i[seconds minutes hours days weeks months years].freeze
READERS = %i[to_i to_f in_seconds in_minutes in_hours in_days in_weeks in_months in_years].freeze

def outcome
  value = yield
  "#{value.class} #{value.inspect}"
rescue Exception => e
  "#{e.class}: #{e.message.lines.first.to_s.strip}"
end

[0, 1, 2, 3, 45, 90, 1.5].each do |n|
  UNITS.each do |unit|
    READERS.each do |reader|
      puts "#{n}.#{unit}.#{reader}: #{outcome { ActiveSupport::Duration.public_send(unit, n).public_send(reader) }}"
    end
  end
end
