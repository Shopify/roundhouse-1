# Native Rails oracle (the Rails native Core runs; tests/support/
# rails_oracle.rb) for File.atomic_write (core_ext/file/atomic) against
# runtime/spinel/active_support_file_atomic.rb.
#
#   ruby tests/active_support_file_atomic_runtime.rb compare
#
# runs this script twice: `native` requires the pinned activesupport's
# feature; `emitted` loads the port as the emitted PostgreSQL tree's
# runtime/ does. Each prints one line per observation; `compare` requires
# them equal.
require_relative "support/rails_oracle"
require "tmpdir"
require "fileutils"

ROOT = File.expand_path("..", __dir__)

if ARGV.first == "compare"
  require "open3"
  outputs = ["native", "emitted"].map do |mode|
    env = mode == "emitted" ? RailsOracle.core_gems_env : {}
    stdout, stderr, status = Open3.capture3(env, RbConfig.ruby, File.expand_path(__FILE__), mode)
    abort "#{mode}: #{stderr}" unless status.success?
    stdout
  end
  native, emitted = outputs.map(&:lines)
  unless outputs.uniq.length == 1
    diff = native.zip(emitted).each_with_index.reject { |(x, y), _| x == y }.first(8)
    abort "ports differ:\n" + diff.map { |(x, y), i| "line #{i + 1}\n  native:  #{x}  emitted: #{y}" }.join("\n")
  end
  puts outputs.first
  puts "observations: #{native.length}"
  puts "Native Rails and emitted File.atomic_write agree (#{RailsOracle.label})"
  exit
end

MODE = ARGV.first
abort "usage: #{$PROGRAM_NAME} compare | native | emitted" unless %w[native emitted].include?(MODE)

if MODE == "native"
  RailsOracle.activate("activesupport")
  require "active_support"
  require "active_support/core_ext/file/atomic"
else
  abort "File.atomic_write defined before the port loads" if File.respond_to?(:atomic_write)
  require File.join(ROOT, "runtime/spinel/active_support_file_atomic")
end

def show(label, value)
  puts "#{label}: #{value.inspect}"
end

def outcome
  yield.inspect
rescue Exception => e
  "#{e.class}: #{e.message.lines.first.chomp.sub(/\.tmp\.\h+/, ".tmp.HEX").inspect}"
end

Dir.mktmpdir("rh-atomic-write") do |dir|
  listing = -> { Dir.children(dir).sort }
  path = File.join(dir, "verdict.json")
  show("parameters", File.method(:atomic_write).parameters)
  show("new file returns block value", File.atomic_write(path) { |f| f.write("one"); :written })
  show("new file content", File.read(path))
  show("new file binmode", File.atomic_write(path) { |f| [f.binmode?, f.path.start_with?(File.join(dir, ".verdict.json.tmp."))] })
  File.chmod(0o640, path)
  File.atomic_write(path) { |f| f.write("two") }
  show("overwrite content", File.read(path))
  show("overwrite keeps mode", format("%o", File.stat(path).mode & 0o777))
  show("raise inside block", outcome { File.atomic_write(path) { |f| f.write("half"); raise ArgumentError, "boom" } })
  show("content after raise", File.read(path))
  show("no temp files left", listing.call)
  tmp = File.join(dir, "tmp")
  FileUtils.mkdir_p(tmp)
  File.atomic_write(File.join(dir, "other.yml"), tmp) { |f| f.write("x: 1\n") }
  show("temp_dir argument", [File.read(File.join(dir, "other.yml")), Dir.children(tmp)])
  show("missing directory", outcome { File.atomic_write(File.join(dir, "nope", "a.txt")) { |f| f.write("a") } })
  long = File.join(dir, "#{"n" * 250}.txt")
  show("long name", outcome { File.atomic_write(long) { |f| f.write("l"); f.path.bytesize - dir.bytesize } })
  show("final listing size", listing.call.size)
end
