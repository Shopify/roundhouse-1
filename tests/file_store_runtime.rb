# Native ActiveSupport oracle (the Rails native Core runs; tests/support/
# rails_oracle.rb) for ActiveSupport::Cache::FileStore against
# runtime/spinel/active_support_file_store.rb. Each mode works in its own
# temporary directory and prints one line per observation, with that
# directory shown as DIR; `compare` requires them equal.
#
# The surface is what Core's cardserver simulator calls (`FileStore.new(path)`,
# `write`, `read`, `fetch`, `delete` with `expires_in:`), plus the rest of
# the store: the key-to-path mapping (long keys split, very long keys
# digested), unless_exist, versions, counters, delete_matched, cleanup of
# expired entries, clear keeping .keep files, lookup_store and the
# instrumentation payloads.
require "json"
require "tmpdir"
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
    abort "stores differ:\n" + diff.map { |(x, y), i| "line #{i + 1}\n  native:  #{x}  emitted: #{y}" }.join("\n")
  end
  puts outputs.first
  puts "Native ActiveSupport and the emitted FileStore agree (#{RailsOracle.label})"
  exit
end

require "rack"
if ARGV.first == "native"
  RailsOracle.activate("activesupport")
  require "active_support"
  require "active_support/cache"
  require "active_support/cache/file_store"
  require "active_support/notifications"
  # A Rails app has it loaded (active_support/all); file_store.rb calls
  # String#blank? without requiring it.
  require "active_support/core_ext/object/blank"
else
  require_relative "../runtime/ruby/rails"
  # In a tree rails_configuration.rb sits beside rails.rb and requires it
  # by that name; here rails.rb is already loaded from runtime/ruby.
  $LOADED_FEATURES << File.expand_path("../runtime/spinel/rails.rb", __dir__)
  require_relative "../runtime/spinel/rails_configuration"
  require_relative "../runtime/spinel/active_support_notifications"
  require_relative "../runtime/spinel/active_support_cache"
  require_relative "../runtime/spinel/active_support_file_store"
end

ROOT = Dir.mktmpdir("rh-file-store-")
at_exit { FileUtils.rm_rf(ROOT) }

def stable(text) = text.gsub(ROOT, "DIR").gsub(/0x\h+/, "0x")

def show(label, value)
  puts "#{label}: #{stable(value.inspect)}"
end

def outcome
  stable(yield.inspect)
rescue Exception => e
  "#{e.class}: #{stable(e.message.lines.first.to_s.strip)}"
end

EVENTS = []
ActiveSupport::Notifications.subscribe(/cache_/) do |name, _start, _finish, _id, payload|
  EVENTS << [name, payload.reject { |k, _| k == :event || k == :store }.sort_by { |k, _| k.to_s }]
end

def events
  out = EVENTS.dup
  EVENTS.clear
  stable(out.inspect)
end

# Every file under the store, relative to it.
def tree(store)
  Dir.glob("**/*", File::FNM_DOTMATCH, base: store.cache_path).reject { |p| File.basename(p) == "." }.sort
end

dir = File.join(ROOT, "sessions")
store = ActiveSupport::Cache::FileStore.new(dir)
show "class", store.class.name
show "cache_path", store.cache_path
show "versioning", ActiveSupport::Cache::FileStore.supports_cache_versioning?
show "constants", [ActiveSupport::Cache::FileStore::DIR_FORMATTER, ActiveSupport::Cache::FileStore::FILENAME_MAX_SIZE,
                   ActiveSupport::Cache::FileStore::FILEPATH_MAX_SIZE, ActiveSupport::Cache::FileStore::GITKEEP_FILES]
show "inspect", store.inspect
show "options", store.options
show "pathname", ActiveSupport::Cache::FileStore.new(Pathname.new(dir)).cache_path
show "options with namespace", ActiveSupport::Cache::FileStore.new(dir, namespace: "ns", expires_in: 60).options
show "missing dir read", store.read("absent")
show "missing dir exist", store.exist?("absent")
show "missing dir cleanup", outcome { store.cleanup }
show "missing dir clear", outcome { store.clear }
events

show "write", store.write("session:abc", "a=1&b=2")
show "tree", tree(store)
show "read", store.read("session:abc")
show "exist", store.exist?("session:abc")
show "key path", store.send(:normalize_key, "session:abc", {}).delete_prefix(ROOT)
show "file_path_key", store.send(:file_path_key, store.send(:normalize_key, "session:abc", {}))
show "events", events
show "write hash", store.write("card_credentials:k1", { "number" => "4242" }.to_json, expires_in: 600)
show "read hash", store.read("card_credentials:k1")
show "fetch hit", store.fetch("session:abc") { "never" }
show "fetch miss", store.fetch("session:new") { "computed" }
show "fetch stored", store.read("session:new")
show "fetch force", store.fetch("session:new", force: true) { "forced" }
show "events", events
show "unless_exist", store.write("session:abc", "other", unless_exist: true)
show "kept", store.read("session:abc")
show "symbol and array keys", [store.write([:a, "b", 1], "arr"), store.read([:a, "b", 1]), store.write(:sym, 5), store.read(:sym)]
show "unicode key", [store.write("ключ/ü", "u"), store.read("ключ/ü"),
                     store.send(:file_path_key, store.send(:normalize_key, "ключ/ü", {}))]
show "nil and false", [store.write("nil", nil), store.read("nil"), store.exist?("nil"), store.write("false", false), store.read("false")]
show "objects", [store.write("obj", [1, { x: :y }, 2.5]), store.read("obj")]

long = "k" * 300
show "long key", [store.write(long, "long"), store.read(long)]
show "long key path", store.send(:normalize_key, long, {}).delete_prefix(ROOT)
show "long key round trip", store.send(:file_path_key, store.send(:normalize_key, long, {})) == long
huge = "h" * 1000
show "huge key", [store.write(huge, "huge"), store.read(huge)]
show "huge key path", store.send(:normalize_key, huge, {}).delete_prefix(ROOT)
events

show "versioned write", store.write("v", "one", version: 1)
show "versioned read", [store.read("v", version: 1), store.read("v", version: 2), store.read("v")]
show "increment new", store.increment("count")
show "increment", store.increment("count", 5)
show "decrement", store.decrement("count", 2)
show "decrement new", store.decrement("down")
show "counter raw", store.read("count")
show "increment versioned mismatch", [store.write("vc", 10, version: "a"), store.increment("vc", 1, version: "b"), store.read("vc", version: "b")]
show "increment string", outcome { store.increment("count", "x") }
show "counter events", events

show "delete", store.delete("session:new")
show "delete again", store.delete("session:new")
show "deleted", store.read("session:new")
show "delete events", events
show "multi", [store.write_multi("m1" => 1, "m2" => 2), store.read_multi("m1", "m2", "m3")]
show "fetch_multi", store.fetch_multi("m1", "m4") { |k| "made #{k}" }
show "delete_multi", store.delete_multi(["m1", "m2", "zz"])
events

show "delete_matched", outcome { store.delete_matched(/\Asession:/) }
show "after delete_matched", [store.read("session:abc"), store.read("card_credentials:k1")]
show "delete_matched string", outcome { store.delete_matched("card") }
show "delete_matched events", events

store.write("short", "gone soon", expires_in: 0.05)
store.write("lasting", "stays", expires_in: 600)
sleep 0.1
show "expired read", store.read("short")
store.write("short2", "gone soon", expires_in: 0.05)
sleep 0.1
show "cleanup", outcome { store.cleanup }
show "after cleanup", [store.exist?("short2"), store.read("lasting")]
events

show "namespaced", begin
  ns = ActiveSupport::Cache::FileStore.new(dir, namespace: "tenant")
  [ns.write("k", "v"), ns.read("k"), store.read("tenant:k"), store.read("k"), ns.send(:normalize_key, "k", ns.options).delete_prefix(ROOT)]
end

File.write(File.join(dir, ".keep"), "")
File.write(File.join(dir, ".gitkeep"), "")
# rm_r answers the removed paths in directory order, which the two
# temporary directories need not share.
show "clear", outcome { Array(store.clear).sort }
show "after clear", tree(store)
show "read after clear", store.read("lasting")
show "clear events", events

show "lookup_store", outcome { ActiveSupport::Cache.lookup_store(:file_store, File.join(ROOT, "looked")).cache_path }
show "lookup_store options", outcome { ActiveSupport::Cache.lookup_store(:file_store, File.join(ROOT, "looked"), expires_in: 5).options }
show "lookup_store array", outcome { ActiveSupport::Cache.lookup_store([:file_store, File.join(ROOT, "arr")]).class.name }

show "corrupt entry", begin
  path = store.send(:normalize_key, "corrupt", {})
  FileUtils.mkdir_p(File.dirname(path))
  File.binwrite(path, "not a cache entry")
  outcome { store.read("corrupt") }
end
show "unreadable entry", begin
  path = store.send(:normalize_key, "dir_entry", {})
  FileUtils.mkdir_p(path)
  outcome { store.read("dir_entry") }
end
show "rewrite keeps one file", begin
  store.write("again", 1)
  store.write("again", 2)
  [store.read("again"), Dir.children(File.dirname(store.send(:normalize_key, "again", {}))).size]
end
