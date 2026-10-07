# Native ActiveSupport oracle (the Rails native Core runs; tests/support/
# rails_oracle.rb) for ActiveSupport::Cache::MemCacheStore
# against runtime/spinel/active_support_mem_cache_store.rb. Both sides load
# one dalli: Shopify's fork from Core's pinned gem home (CORE_GEM_HOME,
# bundler/gems/dalli-*), and that gem home's connection_pool. Each mode
# prints one line per observation; `compare` requires them equal.
#
# Construction, option handling, key normalization, the entry coder and the
# error fallback need no server. The live part (reads, writes, multi
# operations, counters, expiry, local cache, clear, stats) runs against a
# memcached `compare` starts on a free localhost port, when MEMCACHED, PATH
# or the nix store has a binary; otherwise it prints "live: untested" in both modes.
require "json"
require "socket"
require_relative "support/rails_oracle"

CORE_GEM_HOME = ENV.fetch("CORE_GEM_HOME", "/nix/store/kr4swvh5mw01fs3kixfa9afy57zsyihg-ruby-gems-dev/lib/ruby/gems/4.0.0")

def memcached_binary
  candidates = [ENV["MEMCACHED"]]
  candidates += ENV.fetch("PATH", "").split(File::PATH_SEPARATOR).map { |dir| File.join(dir, "memcached") }
  # The memcached this host's nix store carries, when PATH has none.
  candidates += Dir["/nix/store/*-memcached-*/bin/memcached"].sort
  candidates.compact.find { |path| File.executable?(path) }
end

if ARGV.first == "compare"
  require "open3"
  # Both modes load dalli and its dependencies from the pinned gem home.
  env = RailsOracle.core_gems_env
  server = nil
  if (binary = memcached_binary)
    probe = TCPServer.new("127.0.0.1", 0)
    port = probe.addr[1]
    probe.close
    server = Process.spawn(binary, "-l", "127.0.0.1", "-p", port.to_s, "-U", "0", "-m", "16", out: File::NULL, err: File::NULL)
    deadline = Time.now + 10
    begin
      TCPSocket.new("127.0.0.1", port).close
    rescue Errno::ECONNREFUSED
      abort "memcached did not listen on #{port}" if Time.now > deadline
      sleep 0.05
      retry
    end
    env["MEMCACHE_TEST_PORT"] = port.to_s
  end
  begin
    outputs = ["native", "emitted"].map do |mode|
      stdout, stderr, status = Open3.capture3(env, RbConfig.ruby, __FILE__, mode)
      abort "#{mode}: #{stderr}" unless status.success?
      stdout
    end
  ensure
    if server
      Process.kill("TERM", server)
      Process.wait(server)
    end
  end
  unless outputs.uniq.length == 1
    a, b = outputs.map(&:lines)
    diff = a.zip(b).each_with_index.reject { |(x, y), _| x == y }.first(5)
    abort "stores differ:\n" + diff.map { |(x, y), i| "line #{i + 1}\n  native:  #{x}  emitted: #{y}" }.join("\n")
  end
  puts outputs.first
  puts "Native ActiveSupport and the emitted MemCacheStore agree (#{RailsOracle.label})"
  exit
end

dalli = Dir["#{CORE_GEM_HOME}/bundler/gems/dalli-*/lib"].first or abort "no dalli under #{CORE_GEM_HOME}"
pool = Dir["#{CORE_GEM_HOME}/gems/connection_pool-*/lib"].first or abort "no connection_pool under #{CORE_GEM_HOME}"
$LOAD_PATH.unshift(dalli, pool)

require "rack"
if ARGV.first == "native"
  RailsOracle.activate("activesupport")
  require "active_support"
  require "active_support/cache"
  require "active_support/cache/mem_cache_store"
  require "active_support/notifications"
  require "active_support/core_ext/object/blank"
  # Without Rails, dalli logs to $stdout; the emitted side has Rails and
  # logs through Rails.logger (stderr). Keep both off the observations.
  require "logger"
  Dalli.logger = Logger.new($stderr)
else
  require_relative "../runtime/ruby/rails"
  # In a tree rails_configuration.rb sits beside rails.rb and requires it
  # by that name; here rails.rb is already loaded from runtime/ruby.
  $LOADED_FEATURES << File.expand_path("../runtime/spinel/rails.rb", __dir__)
  require_relative "../runtime/spinel/rails_configuration"
  require_relative "../runtime/spinel/active_support_notifications"
  require_relative "../runtime/spinel/active_support_cache"
  require_relative "../runtime/spinel/active_support_mem_cache_store"
end

# Object addresses and absolute expiry times differ run to run.
def stable(text) = text.gsub(/0x\h+/, "0x").gsub(/\b1\d{9}\.\d+/, "T")

def show(label, value)
  puts "#{label}: #{stable(value.inspect)}"
end

def outcome
  stable(yield.inspect)
rescue Exception => e
  "#{e.class}: #{e.message.lines.first.to_s.strip}"
end

EVENTS = []
ActiveSupport::Notifications.subscribe(/cache_/) do |name, _start, _finish, _id, payload|
  EVENTS << [name, payload.reject { |k, _| k == :event || k == :store }.sort_by { |k, _| k.to_s }]
end

def events
  out = EVENTS.dup
  EVENTS.clear
  out
end

module PackCoder
  def self.dump(entry) = Marshal.dump(entry.pack)
  def self.load(payload) = ActiveSupport::Cache::Entry.unpack(Marshal.load(payload))
end

class Lines
  attr_reader :lines

  def initialize
    @lines = []
  end

  # ::Logger at :info: debug is dropped, its block not called.
  def debug? = false
  def debug(message = nil, &block) = (@lines << [:debug, block ? block.call : message] if debug?)
  def error(message = nil, &block) = @lines << [:error, block ? block.call : message]
  def warn(message = nil, &block) = @lines << [:warn, block ? block.call : message]
end

Store = ActiveSupport::Cache::MemCacheStore
show "versioning", Store.supports_cache_versioning?
show "overridden", Store::OVERRIDDEN_OPTIONS
# 8.2 dropped the constant and writes the pattern inline.
show "escape", outcome { Store::ESCAPE_KEY_CHARS }
# Which multi-key hooks the store answers itself, given this dalli
# (set_multi: yes; delete_multi only from dalli 5.0.6).
show "multi hooks", %i[write_multi_entries delete_multi_entries read_multi_entries].map { |m| Store.instance_method(m).owner.name }

# --- construction and options, no server -----------------------------------
offline = Store.new("127.0.0.1:1", namespace: "rh", expires_in: 60, compress_threshold: 10, pool: false)
show "options", offline.options
show "client", offline.instance_variable_get(:@data).class
show "mem_cache_options", offline.instance_variable_get(:@mem_cache_options)
show "inspect", offline.inspect
show "silence", offline.silence?
show "cache_nils", Store.new(nil, cache_nils: false, pool: false).options
show "cache_nils true", Store.new(nil, cache_nils: true, pool: false).options
show "max_key_size", Store.new(nil, max_key_size: 40, pool: false).options
show "array addresses", Store.new(["127.0.0.1:1", "127.0.0.1:2"], pool: false).instance_variable_get(:@data).instance_variable_get(:@normalized_servers)
show "no addresses", Store.new(pool: false).instance_variable_get(:@data).instance_variable_get(:@normalized_servers)
show "bad address", outcome { Store.new(1) }
show "symbol address", outcome { Store.new(:x) }
# 8.1 took a Dalli::Client as the first address; 8.2 refuses it.
show "client address", outcome { Store.new(Dalli::Client.new("127.0.0.1:1"), pool: false).instance_variable_get(:@data).class }
pooled = Store.new("127.0.0.1:1", pool: { size: 2, timeout: 1 })
show "pooled", pooled.instance_variable_get(:@data).class
show "pool size", pooled.instance_variable_get(:@data).size
show "default pool", Store.new("127.0.0.1:1").instance_variable_get(:@data).class
show "pool true", Store.new("127.0.0.1:1", pool: true).instance_variable_get(:@data).class
show "bad pool", outcome { Store.new("127.0.0.1:1", pool: 3) }
show "build_mem_cache", Store.build_mem_cache("127.0.0.1:1", pool: false).class
show "build_mem_cache options", Store.build_mem_cache("127.0.0.1:1", { pool: false, namespace: "x" }).instance_variable_get(:@options)
show "build_mem_cache none", Store.build_mem_cache(pool: false).instance_variable_get(:@normalized_servers)
show "lookup", ActiveSupport::Cache.lookup_store(:mem_cache_store, "127.0.0.1:1", namespace: "l").class
show "lookup options", ActiveSupport::Cache.lookup_store(:mem_cache_store, "127.0.0.1:1", namespace: "l").options

# --- keys -------------------------------------------------------------------
["plain", "with space", "tab\tand\nnewline", "pct%", "café", "k" * 300, :sym, ["a", 1], { b: 2 }].each do |key|
  normalized = offline.send(:normalize_key, key, offline.options)
  show "key #{key.inspect[0, 20]}", [normalized, normalized&.encoding, normalized&.bytesize]
end
show "key namespace option", offline.send(:normalize_key, "k", namespace: "other")
show "key namespace proc", offline.send(:normalize_key, "k", namespace: -> { "dyn" })
show "key no namespace", Store.new(pool: false).send(:normalize_key, "k", {})

# --- the entry coder --------------------------------------------------------
entry = ActiveSupport::Cache::Entry.new("value" * 10, version: "v1", expires_in: 30)
payload = offline.send(:serialize_entry, entry)
show "payload", [payload.encoding, payload.byteslice(0, 2).bytes]
lasting = ActiveSupport::Cache::Entry.new("value" * 10, version: "v1")
show "payload size", offline.send(:serialize_entry, lasting).bytesize
back = offline.send(:deserialize_entry, payload)
show "roundtrip", [back.class, back.value, back.version, back.expired?]
show "raw serialize", offline.send(:serialize_entry, ActiveSupport::Cache::Entry.new(5), raw: true)
show "raw deserialize", [offline.send(:deserialize_entry, "5", raw: true).value]
show "raw nil", offline.send(:deserialize_entry, nil, raw: true)
show "deserialize nil", offline.send(:deserialize_entry, nil)
coded = Store.new("127.0.0.1:1", coder: PackCoder, pool: false)
packed = coded.send(:serialize_entry, entry)
show "coder payload", Marshal.load(packed)
show "coder roundtrip", coded.send(:deserialize_entry, packed).value
show "coder and serializer", outcome { Store.new("127.0.0.1:1", coder: PackCoder, serializer: :marshal_7_1, pool: false) }
show "serializer 7.1", Store.new("127.0.0.1:1", serializer: :marshal_7_1, pool: false).send(:serialize_entry, entry).byteslice(0, 2).bytes
compressed = Store.new("127.0.0.1:1", compress: true, compress_threshold: 8, pool: false)
show "compressed payload size", compressed.send(:serialize_entry, ActiveSupport::Cache::Entry.new("x" * 400)).bytesize
show "compressed roundtrip", compressed.send(:deserialize_entry, compressed.send(:serialize_entry, ActiveSupport::Cache::Entry.new("x" * 400))).value.size

# --- a server that is not there: every operation falls back -----------------
logger = Lines.new
offline.logger = logger
show "down read", offline.read("k")
show "down write", offline.write("k", 1)
show "down fetch", offline.fetch("k") { :computed }
show "down delete", offline.delete("k")
show "down exist", offline.exist?("k")
show "down increment", offline.increment("n")
show "down decrement", offline.decrement("n")
show "down read_multi", offline.read_multi("a", "b")
show "down write_multi", offline.write_multi("a" => 1)
show "down clear", offline.clear
show "down stats", outcome { offline.stats }
show "down logged", logger.lines.map { |level, message| [level, message.sub(/\(.*?\)/, "()")[0, 40]] }.uniq
offline.logger = nil
show "down events", events.map(&:first).tally

# --- live, against a local memcached ---------------------------------------
port = ENV["MEMCACHE_TEST_PORT"]
unless port
  puts "live: untested (no memcached to start)"
  exit
end

store = Store.new("127.0.0.1:#{port}", namespace: "rh", expires_in: 60)
store.clear
events
show "write", store.write("k", { a: 1 })
show "read", store.read("k")
show "read events", events
show "exist", [store.exist?("k"), store.exist?("missing")]
show "fetch hit", store.fetch("k") { :no }
show "fetch miss", store.fetch("f") { :computed }
show "fetch force", store.fetch("f", force: true) { :forced }
show "fetch events", events.map(&:first)
show "unless_exist", [store.write("k", 2, unless_exist: true), store.read("k")]
show "nil", [store.write("nil", nil), store.read("nil"), store.exist?("nil")]
skip_nil = Store.new("127.0.0.1:#{port}", namespace: "rh", skip_nil: true)
show "skip_nil", [skip_nil.fetch("sn") { nil }, skip_nil.exist?("sn")]
show "versioned", [store.write("v", 1, version: "a"), store.read("v", version: "a"), store.read("v", version: "b")]
show "delete", [store.delete("k"), store.read("k"), store.delete("k")]
show "write_multi", store.write_multi("m1" => 1, "m2" => [2])
show "write_multi expiring", [store.write_multi({ "w1" => "a", "w2" => "b" }, expires_in: 1, race_condition_ttl: 2), store.read_multi("w1", "w2"), store.write_multi({})]
show "write_multi raw", [store.write_multi({ "wr" => 7 }, raw: true), store.read("wr", raw: true), store.increment("wr")]
show "read_multi", store.read_multi("m1", "m2", "m3")
show "fetch_multi", store.fetch_multi("m1", "m4") { |key| "#{key}!" }
show "read_multi versions", store.read_multi("v", version: "a")
show "multi events", events.map(&:first)
show "raw write", store.write("count", 5, raw: true)
show "raw read", store.read("count", raw: true)
show "increment", [store.increment("count"), store.increment("count", 10)]
show "decrement", [store.decrement("count", 3), store.decrement("count", 100)]
show "increment unset", store.increment("unset", 2)
show "decrement unset", store.decrement("unset2", 2)
show "increment non-raw", (store.write("boxed", 1); store.increment("boxed"))
show "counter events", events.map { |name, payload| [name, payload.to_h[:amount]] }
show "expired", [store.write("e", 1, expires_in: 0.001), (sleep 0.01; store.read("e"))]
show "race ttl", [store.write("r", 1, expires_in: 1, race_condition_ttl: 5), store.read("r")]
show "local cache", store.with_local_cache { store.write("lc", 1); [store.read("lc"), store.send(:local_cache).class] }
show "local cache gone", store.send(:local_cache)
show "big", store.write("big", "x" * 2_000_000)
show "big read", store.read("big")&.size
show "long key", [store.write("k" * 300, 1), store.read("k" * 300)]
show "unicode key", [store.write("café key", 1), store.read("café key")]
show "stats", store.stats.map { |server, stats| [server.sub(port, "PORT"), stats.key?("pid"), stats.key?("curr_items")] }
show "clear", [store.clear.class, store.read("m1")]
show "pooled live", (pooled_live = Store.new("127.0.0.1:#{port}", pool: { size: 2 }); pooled_live.write("p", 1); pooled_live.read("p"))
events
