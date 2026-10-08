# Native ActiveSupport oracle (the Rails native Core runs; tests/support/
# rails_oracle.rb) for ActiveSupport::FileUpdateChecker and
# ActiveSupport::Benchmark against runtime/spinel/active_support_file_update_checker.rb
# and runtime/spinel/active_support_benchmark.rb. Each mode works in its own
# temporary directory and prints one line per observation, with that
# directory shown as DIR; `compare` requires them equal.
#
# The checker's surface is what Core's developer dashboard calls
# (`FileUpdateChecker.new(files, dirs) {}` and `execute_if_updated`): files
# and extension globs, additions, removals, mtime changes, mtimes in the
# future, the block and the gem-path filter. Benchmark's is
# `realtime(unit) { }`: the units Core passes and the default.
require "tmpdir"
require "fileutils"
require_relative "support/rails_oracle"

if ARGV.first == "compare"
  require "open3"
  outputs = ["native", "emitted"].map do |mode|
    stdout, stderr, status = Open3.capture3(RbConfig.ruby, __FILE__, mode)
    abort "#{mode}: #{stderr}" unless status.success?
    stdout
  end
  unless outputs.uniq.length == 1
    a, b = outputs.map(&:lines)
    diff = a.zip(b).each_with_index.reject { |(x, y), _| x == y }.first(5)
    abort "utilities differ:\n" + diff.map { |(x, y), i| "line #{i + 1}\n  native:  #{x}  emitted: #{y}" }.join("\n")
  end
  puts outputs.first
  puts "Native ActiveSupport and the emitted FileUpdateChecker and Benchmark agree (#{RailsOracle.label})"
  exit
end

if ARGV.first == "native"
  RailsOracle.activate("activesupport")
  require "active_support"
  require "active_support/file_update_checker"
  require "active_support/benchmark"
else
  require_relative "../runtime/spinel/active_support_file_update_checker"
  require_relative "../runtime/spinel/active_support_benchmark"
end

ROOT = Dir.mktmpdir("rh-as-utilities-")
at_exit { FileUtils.rm_rf(ROOT) }

def stable(text) = text.gsub(ROOT, "DIR")

def show(label, value)
  puts "#{label}: #{stable(value.inspect)}"
end

def outcome
  stable(yield.inspect)
rescue Exception => e
  "#{e.class}: #{stable(e.message.lines.first.to_s.strip)}"
end

# A fixed past mtime per step, so the order of changes never depends on
# the clock's resolution.
T0 = Time.at(1_600_000_000)
def touch(path, step)
  FileUtils.mkdir_p(File.dirname(path))
  File.write(path, "x") unless File.exist?(path)
  File.utime(T0 + step, T0 + step, path)
end

show "no block", outcome { ActiveSupport::FileUpdateChecker.new([]) }

css = File.join(ROOT, "app/assets/stylesheets")
input = File.join(ROOT, "app/assets/tailwind/application.css")
touch(input, 1)
touch(File.join(css, "theme.css"), 2)
touch(File.join(css, "ignored.txt"), 3)
calls = []
checker = ActiveSupport::FileUpdateChecker.new([input, File.join(ROOT, "missing.css")], { css => ["css"] }) { calls << :ran }
show "fresh updated?", checker.updated?
show "fresh execute_if_updated", checker.execute_if_updated
show "calls", calls

touch(File.join(css, "ignored.txt"), 10)
show "other extension updated?", checker.updated?

touch(File.join(css, "theme.css"), 20)
show "mtime updated?", checker.updated?
show "still updated?", checker.updated?
yielded = []
show "execute_if_updated", checker.execute_if_updated { yielded << :yield }
show "yield and calls", [yielded, calls]
show "after execute updated?", checker.updated?

touch(File.join(css, "nested/added.css"), 5)
show "added older file updated?", checker.updated?
checker.execute
show "execute calls", calls

FileUtils.rm(File.join(css, "nested/added.css"))
show "removed updated?", checker.updated?
checker.execute

future = Time.now + 3600
File.utime(future, future, input)
show "future mtime updated?", checker.updated?
touch(input, 30)
show "back from the future updated?", checker.updated?
checker.execute

touch(File.join(ROOT, "missing.css"), 40)
show "listed file created updated?", checker.updated?
checker.execute

multi = ActiveSupport::FileUpdateChecker.new([], { css => %w[css txt], File.join(ROOT, "a,b") => [] }) {}
show "globs", multi.instance_variable_get(:@globs)
show "multi updated?", multi.updated?
show "no dirs globs", ActiveSupport::FileUpdateChecker.new([input]) {}.instance_variable_get(:@globs)
show "string ext", ActiveSupport::FileUpdateChecker.new([], { css => "css" }) {}.instance_variable_get(:@globs)

gem_file = File.join(Gem.path.first, "some_gem.rb")
gem_checker = ActiveSupport::FileUpdateChecker.new([gem_file, input], { Gem.path.first => ["rb"], css => ["css"] }) {}
show "gem paths filtered", [gem_checker.instance_variable_get(:@files).map { _1.sub(Gem.path.first, "GEM") },
                            gem_checker.instance_variable_get(:@globs)&.size]
show "frozen", [gem_checker.instance_variable_get(:@files).frozen?]

empty = ActiveSupport::FileUpdateChecker.new([]) { :value }
show "empty updated?", empty.updated?
show "execute returns", empty.execute
show "raising block", outcome { ActiveSupport::FileUpdateChecker.new([]) { raise "boom" }.execute }

ms = ActiveSupport::Benchmark.realtime(:float_millisecond) { sleep 0.02 }
show "float_millisecond", [ms.class, ms >= 20, ms < 2000]
s = ActiveSupport::Benchmark.realtime { sleep 0.02 }
show "default", [s.class, s >= 0.02, s < 2]
show "nanosecond", ActiveSupport::Benchmark.realtime(:nanosecond) { }.class
show "no block", outcome { ActiveSupport::Benchmark.realtime }
show "bad unit", outcome { ActiveSupport::Benchmark.realtime(:fortnight) { } }
show "block value discarded", ActiveSupport::Benchmark.realtime { :ignored }.is_a?(Float)
