# Native ActiveSupport 8.1.4 oracle for ActiveSupport::Cache (Store,
# Entry, Coder, SerializerWithFallback, Strategy::LocalCache, MemoryStore,
# NullStore, WriteOptions, IsolatedExecutionState, Digest) against
# runtime/spinel/active_support_cache.rb. Each mode prints one line per
# observation; `compare` requires them equal.
#
# The surface is what Core's in-process stores (TestCacheStore,
# TestLocalMemcacheStore, ThreadLocalTtl, HighThroughputLocalCache) call:
# MemoryStore's public API and the private Store hooks they override or
# reach (read_entry / write_entry / delete_entry, normalize_key,
# merged_options, instrument, serialize_entry / deserialize_entry,
# local_cache, use_temporary_local_cache).
require "json"

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
    abort "caches differ:\n" + diff.map { |(x, y), i| "line #{i + 1}\n  native:  #{x}  emitted: #{y}" }.join("\n")
  end
  puts outputs.first
  puts "Native ActiveSupport and the emitted ActiveSupport::Cache agree"
  exit
end

require "rack"
if ARGV.first == "native"
  gem "activesupport", "8.1.4"
  require "active_support"
  require "active_support/cache"
  require "active_support/cache/memory_store"
  require "active_support/cache/null_store"
  require "active_support/cache/strategy/local_cache_middleware"
  require "active_support/isolated_execution_state"
  require "active_support/digest"
  require "active_support/notifications"
  # Every Rails process has Object#blank?; the cache's debug log calls it.
  require "active_support/core_ext/object/blank"
else
  require_relative "../runtime/ruby/rails"
  require_relative "../runtime/spinel/active_support_notifications"
  require_relative "../runtime/spinel/active_support_cache"
end

def show(label, value)
  puts "#{label}: #{value.inspect}"
end

def outcome
  yield.inspect
rescue Exception => e
  "#{e.class}: #{e.message.lines.first.to_s.strip}"
end

# Cache events as subscribers see them: the name and the payload, minus
# what differs run to run.
EVENTS = []
ActiveSupport::Notifications.subscribe(/cache_/) do |name, _start, _finish, _id, payload|
  EVENTS << [name, payload.reject { |k, _| k == :event }.sort_by { |k, _| k.to_s }]
end

def events
  out = EVENTS.dup
  EVENTS.clear
  out
end

class Record
  def initialize(id, version)
    @id = id
    @version = version
  end

  def cache_key = "records/#{@id}"
  def cache_version = @version
end

class Versioned
  def cache_key_with_version = "v/1-2"
end

class Lines
  attr_reader :lines

  def initialize
    @lines = []
  end

  def debug? = true
  def debug(message) = @lines << [:debug, message]
  def error(message) = @lines << [:error, message]
  def warn(message) = @lines << [:warn, message]
end

# A coder of the shape Core's VersionedMemcacheCoder has: dump an Entry
# to a String, load it back.
module PackCoder
  def self.dump(entry) = Marshal.dump(entry.pack)
  def self.load(payload) = ActiveSupport::Cache::Entry.unpack(Marshal.load(payload))
end

# A MemoryStore subclass that overrides the private hooks the way
# TestCacheStore does.
class HookedStore < ActiveSupport::Cache::MemoryStore
  attr_reader :calls

  def initialize(**options)
    super(coder: PackCoder, **options)
    @calls = []
  end

  def read_entry(key, **options)
    @calls << [:read_entry, key, options.keys.sort]
    super
  end

  def write_entry(key, entry, **options)
    @calls << [:write_entry, key, entry.value, entry.version]
    ActiveSupport::Cache::MemoryStore.instance_method(:write_entry).bind_call(self, key, entry, **options)
  end

  def delete_entry(key, **options)
    @calls << [:delete_entry, key]
    super
  end

  def normalize_key(key, options = nil)
    namespace = (options || {})[:namespace] || @options[:namespace]
    if namespace && key.is_a?(String) && key.start_with?("#{namespace}:")
      truncate_key(key)
    else
      super
    end
  end

  def peek(name, **options)
    options = merged_options(options)
    key = normalize_key(name, options)
    entry = read_entry(key, **options)
    [key, entry&.value, entry&.version, entry&.expires_at.nil?]
  end

  def local?
    use_temporary_local_cache(nil) { local_cache.nil? }
  end
end

# --- MemoryStore: the public API -----------------------------------------
store = ActiveSupport::Cache::MemoryStore.new
show("options", store.options)
show("write", store.write("a", "x"))
show("read", store.read("a"))
show("read miss", store.read("missing"))
show("exist?", [store.exist?("a"), store.exist?("missing")])
show("fetch hit", store.fetch("a") { "unused" })
show("fetch miss", store.fetch("b") { |key, opts| [key, opts.class.name] })
show("fetch force", store.fetch("a", force: true) { "forced" })
show("fetch skip_nil", [store.fetch("n", skip_nil: true) { nil }, store.exist?("n")])
show("fetch nil cached", [store.fetch("n2") { nil }, store.exist?("n2")])
show("fetch no block", store.fetch("a"))
show("fetch force no block", outcome { store.fetch("a", force: true) })
show("fetch write options", store.fetch("wo") { |_, opts| opts.expires_in = 60; opts.version = "v9"; [opts.expires_in, opts.version, opts.expires_at] })
show("version after fetch options", [store.read("wo"), store.read("wo", version: "v9")])
show("events", events)

value = +"mutable"
store.write("dup", value)
value << "!"
read = store.read("dup")
read << "?"
show("dup coder isolates values", [store.read("dup"), value])
store.write("obj", { list: [1, 2] })
obj = store.read("obj")
obj[:list] << 3
show("dup coder isolates objects", store.read("obj"))
show("numbers and booleans", [store.write("i", 3), store.read("i"), store.write("t", true), store.read("t"), store.write("f", false), store.read("f")])

# Keys: symbols, numbers, arrays, hashes, objects that name their key.
show("symbol key", [store.write(:sym, 1), store.read("sym")])
show("integer key", [store.write(42, 1), store.read("42")])
show("array key", [store.write(["a", "b", 3], 1), store.read("a/b/3")])
show("one-element array key", [store.write(["solo"], 1), store.read("solo")])
show("hash key", [store.write({ b: 2, a: 1 }, 1), store.read("a=1/b=2")])
show("cache_key object", [store.write(Record.new(7, "v1"), "rec"), store.read("records/7"), store.read("records/7", version: "v1")])
show("cache_version mismatch", [store.read(Record.new(7, "v1")), store.read(Record.new(7, "v2"))])
show("array version", [store.write([Record.new(1, "a"), Record.new(2, "b")], "pair"), store.read([Record.new(1, "a"), Record.new(2, "b")]), store.read([Record.new(1, "a"), Record.new(2, "c")])])
show("blank key", outcome { store.write("", 1) })
show("nil key", outcome { store.read(nil) })
show("explicit version", [store.write("ver", 1, version: 2), store.read("ver", version: 2), store.read("ver", version: 3), store.read("ver")])
show("events", events)

long = "k" * 300
show("long key", [store.write(long, 1), store.read(long), store.send(:normalize_key, long, {})])
ActiveSupport::Digest.hash_digest_class = OpenSSL::Digest::SHA1
show("long key sha1", store.send(:normalize_key, long, {}))
ActiveSupport::Digest.hash_digest_class = OpenSSL::Digest::MD5
show("digest", ActiveSupport::Digest.hexdigest("abc"))
show("digest class check", outcome { ActiveSupport::Digest.hash_digest_class = Object })
show("max_key_size false", ActiveSupport::Cache::MemoryStore.new(max_key_size: false).send(:normalize_key, long, {}).bytesize)

# Expiry, aliases, race condition TTL.
show("expires_in", [store.write("e", 1, expires_in: 0.05), store.read("e")])
show("expire_in alias", [store.write("e2", 1, expire_in: 0.05), store.read("e2")])
show("expired_in alias", [store.write("e3", 1, expired_in: 0.05), store.send(:merged_options, expired_in: 5)])
sleep 0.1
show("expired", [store.read("e"), store.read("e2"), store.exist?("e3"), store.fetch("e3") { "regenerated" }])
show("expires_at", [store.write("ea", 1, expires_at: Time.now + 60), store.read("ea")])
show("both expiries", outcome { store.write("x", 1, expires_in: 1, expires_at: Time.now) })
show("expires_in as Time", outcome { store.write("x", 1, expires_in: Time.now) }.sub(/Got: .*/, "Got: T"))
logger = Lines.new
ActiveSupport::Cache::Store.logger = logger
show("negative expires_in", [store.write("neg", 1, expires_in: -1), store.read("neg")])
ActiveSupport::Cache::Store.raise_on_invalid_cache_expiration_time = true
show("negative expires_in raises", outcome { store.write("neg", 1, expires_in: -1) })
ActiveSupport::Cache::Store.raise_on_invalid_cache_expiration_time = false
show("logger lines", logger.lines.map { |level, line| [level, line.sub(/0x\h+/, "0x")] })
ActiveSupport::Cache::Store.logger = nil
store.write("race", "old", expires_in: 0.05, race_condition_ttl: 10)
sleep 0.1
show("race condition ttl", [store.fetch("race", race_condition_ttl: 10) { "new" }, store.read("race")])
show("events", events.map { |name, payload| [name, payload.map(&:first)] })

# Multi forms.
show("write_multi", store.write_multi({ "m1" => 1, "m2" => 2 }).transform_values { |e| e.respond_to?(:value) ? [e.class.name, e.value] : e })
show("read_multi", store.read_multi("m1", "m2", "m3"))
show("read_multi empty", store.read_multi)
show("fetch_multi", store.fetch_multi("m1", "m3", "m4") { |name| "#{name}!" })
show("fetch_multi skip_nil", [store.fetch_multi("m5", "m1", skip_nil: true) { nil }, store.exist?("m5")])
show("fetch_multi force", store.fetch_multi("m1", force: true) { |name| "#{name}?" })
show("fetch_multi no block", outcome { store.fetch_multi("m1") })
show("delete_multi", [store.delete_multi(["m1", "m2", "zz"]), store.read_multi("m1", "m2", "m3")])
show("delete_multi empty", store.delete_multi([]))
show("events", events)

# Counters, delete, matching, cleanup, clear, pruning.
show("increment", [store.increment("c"), store.increment("c", 5), store.decrement("c", 2), store.read("c", raw: true)])
show("counter api", [store.write_counter("wc", "7"), store.read_counter("wc"), store.increment("wc")])
store.write("ce", 9, expires_in: 0.05)
sleep 0.1
show("increment expired", [store.increment("ce"), store.read("ce", raw: true)])
show("delete", [store.delete("c"), store.delete("c")])
store.write("match:1", 1)
store.write("match:2", 2)
store.write("other", 3)
show("delete_matched", [store.delete_matched(/match/), store.read("match:1"), store.read("other")])
store.write("old", 1, expires_in: 0.05)
sleep 0.1
show("cleanup", [store.cleanup, store.send(:read_entry, "old")&.value])
show("inspect", store.inspect.sub(/size=\d+/, "size=N"))
show("clear", [store.clear, store.read("other"), store.inspect])
show("events", events.map(&:first))
small = ActiveSupport::Cache::MemoryStore.new(size: 1000)
10.times { |i| small.write("p#{i}", "v" * 50) }
show("pruned", (0...10).map { |i| small.exist?("p#{i}") })
show("pruning?", small.pruning?)
show("prune events", events.map { |name, payload| [name, payload.to_h.slice(:key, :from).keys] }.uniq)
show("supports versioning", [ActiveSupport::Cache::MemoryStore.supports_cache_versioning?, ActiveSupport::Cache::NullStore.supports_cache_versioning?])

# Namespaces.
ns = ActiveSupport::Cache::MemoryStore.new(namespace: "app")
show("namespace", [ns.namespace, ns.write("k", 1), ns.read("k"), ns.send(:normalize_key, "k", ns.options)])
show("namespace override", [ns.read("k", namespace: "other"), ns.write("k", 2, namespace: nil), ns.read("k", namespace: nil)])
ns.namespace = -> { "lazy" }
show("proc namespace", [ns.write("k", 3), ns.read("k"), ns.send(:normalize_key, "k", ns.options)])
show("namespaced delete_matched", [ns.write("dm1", 1), ns.delete_matched(/^dm/), ns.read("dm1")])
show("key matcher", [ns.send(:key_matcher, /^a/, namespace: "n"), ns.send(:key_matcher, /a/, namespace: "n"), ns.send(:key_matcher, /a/, {})])
show("events", events.map(&:first))

# Silence and the debug log.
log_store = ActiveSupport::Cache::MemoryStore.new
log = Lines.new
log_store.logger = log
log_store.write("lg", 1, expires_in: 60)
log_store.read_multi("lg", "lh")
log_store.mute { log_store.read("lg") }
show("silence?", [log_store.silence?, log_store.silence!.equal?(log_store), log_store.silence?])
log_store.read("lg")
show("debug log", log.lines)
log_store.logger = nil
events

# --- Entry and the coders ------------------------------------------------
entry = ActiveSupport::Cache::Entry.new("value", version: "v1", expires_at: 1_000_000_000.5)
show("entry", [entry.value, entry.version, entry.expires_at, entry.expired?, entry.mismatched?("v1"), entry.mismatched?("v2"), entry.mismatched?(nil), entry.local?])
show("entry pack", [entry.pack, ActiveSupport::Cache::Entry.new(1).pack, ActiveSupport::Cache::Entry.unpack(["x", nil, "v"]).pack])
entry.expires_at = nil
show("entry expires_at=", [entry.expires_at, entry.expired?])
show("entry bytesize", [ActiveSupport::Cache::Entry.new(nil).bytesize, ActiveSupport::Cache::Entry.new("abc").bytesize, ActiveSupport::Cache::Entry.new([1, 2]).bytesize])
big = ActiveSupport::Cache::Entry.new("z" * 2000)
compressed = big.compressed(1024)
show("entry compressed", [compressed.compressed?.inspect, compressed.value.bytesize, ActiveSupport::Cache::Entry.new("short").compressed(1024).compressed?.inspect, ActiveSupport::Cache::Entry.new(5).compressed(0).compressed?.inspect])
dup_entry = ActiveSupport::Cache::Entry.new({ a: [1] })
held = dup_entry.value
dup_entry.dup_value!
show("dup_value!", [dup_entry.value.equal?(held), dup_entry.value])

[7.0, 7.1].each do |format|
  ActiveSupport::Cache.format_version = format
  plain = ActiveSupport::Cache::MemoryStore.new(compress: true)
  plain_coder = plain.instance_variable_get(:@coder)
  show("format #{format} coder", [plain_coder.class.name, plain_coder.instance_variable_get(:@legacy_serializer)])
  wire = ActiveSupport::Cache::NullStore.new
  show("format #{format} null coder", [wire.instance_variable_get(:@coder).class.name, wire.instance_variable_get(:@legacy_serializer)])
  plain = wire
  bytes = plain.send(:serialize_entry, ActiveSupport::Cache::Entry.new("x" * 2000, version: "v"), compress: true)
  obj_bytes = plain.send(:serialize_entry, ActiveSupport::Cache::Entry.new({ a: 1 }, version: "v", expires_at: 2_000_000_000.0))
  show("format #{format}", [bytes.class.name, bytes.bytesize, bytes.byteslice(0, 2).bytes, obj_bytes.bytes.sum, obj_bytes.bytesize])
  loaded = plain.send(:deserialize_entry, obj_bytes)
  show("format #{format} load", [loaded.value, loaded.version, loaded.expires_at, loaded.mismatched?("v")])
  plain = ActiveSupport::Cache::MemoryStore.new(compress: true)
  show("format #{format} roundtrip", [plain.write("big", "x" * 2000), plain.read("big").bytesize, plain.write("u", "ünï"), plain.read("u"), plain.read("u").encoding.to_s, plain.write("bin", "\xFF".b), plain.read("bin").encoding.to_s])
  default = ActiveSupport::Cache::NullStore.new
  show("format #{format} default coder", default.instance_variable_get(:@coder).class.name)
end
ActiveSupport::Cache.format_version = 7.1
coder = ActiveSupport::Cache::Coder.new(ActiveSupport::Cache::SerializerWithFallback[:marshal_7_1], Zlib)
show("coder legacy load", coder.load(ActiveSupport::Cache::SerializerWithFallback[:marshal_7_0].dump(ActiveSupport::Cache::Entry.new("old"))).value)
show("coder garbage", outcome { coder.load("\x05garbage") })
show("serializer passthrough", ActiveSupport::Cache::SerializerWithFallback[:passthrough].dump(entry).equal?(entry))
show("serializer unknown", outcome { ActiveSupport::Cache::SerializerWithFallback[:nope] })
show("serializer and coder", outcome { ActiveSupport::Cache::MemoryStore.new(coder: PackCoder, serializer: :marshal_7_1) })
show("compressor and coder", outcome { ActiveSupport::Cache::MemoryStore.new(coder: PackCoder, compressor: Zlib) })
ActiveSupport::Cache.format_version = 7.0
show("compressor on 7.0", outcome { ActiveSupport::Cache::NullStore.new(compressor: Zlib) })
ActiveSupport::Cache.format_version = 6.1
show("unknown format", outcome { ActiveSupport::Cache::NullStore.new })
ActiveSupport::Cache.format_version = 7.0
show("coder nil", ActiveSupport::Cache::NullStore.new(coder: nil).instance_variable_get(:@coder).name)
events

# --- Overriding the private hooks, as TestCacheStore does -----------------
hooked = HookedStore.new(namespace: "ns")
show("hooked write", hooked.write("h", "v", version: "1"))
show("hooked read", [hooked.read("h", version: "1"), hooked.read("h", version: "2")])
show("hooked peek", hooked.peek("h"))
show("hooked prenamespaced key", hooked.send(:normalize_key, "ns:h", hooked.options))
show("hooked fetch", hooked.fetch("h2", expires_in: 60) { "made" })
show("hooked unless_exist", [hooked.write("h", "again", unless_exist: true), hooked.read("h")])
show("hooked increment", [hooked.increment("hc"), hooked.increment("hc")])
show("hooked delete", hooked.delete("h"))
show("hooked calls", hooked.calls)
show("hooked local", hooked.local?)
show("hooked events", events.map { |name, payload| [name, payload.to_h[:key], payload.to_h[:store]] })

# --- LocalCache ----------------------------------------------------------
local = ActiveSupport::Cache::MemoryStore.new
show("no local cache", local.local_cache.inspect)
local.write("l", 1)
local.with_local_cache do
  show("local read", [local.read("l"), local.read("l"), local.local_cache.class.name])
  show("local write", [local.write("l2", 2), local.read("l2"), local.local_cache.read_entry("l2").class.name])
  show("local read_multi", local.read_multi("l", "l2", "l3"))
  show("local fetch_multi", local.fetch_multi("l", "l4") { |name| "#{name}~" })
  show("local increment", [local.increment("lc"), local.increment("lc", 2), local.read("lc", raw: true)])
  local.send(:bypass_local_cache) { show("bypassed", local.local_cache.inspect) }
  show("local delete", [local.delete("l"), local.read("l")])
  show("local clear", [local.clear, local.read("l2")])
end
show("local cache unset", [local.local_cache.inspect, local.read("lc")])
show("local events", events.map { |name, payload| [name, payload.to_h[:store]] })
show("local cache key", local.send(:local_cache_key).to_s.sub(/\d+\z/, "N"))
show("hooked local cache key", hooked.send(:local_cache_key).to_s.sub(/\d+\z/, "N"))
local.new_local_cache
show("new_local_cache", local.local_cache.class.name)
local.unset_local_cache
show("unset_local_cache", local.local_cache.inspect)
middleware = local.middleware
show("middleware", [middleware.name, middleware.cache.equal?(local), middleware.equal?(local.middleware)])
app = middleware.new(->(_env) { [200, {}, ["body"]] })
response = app.call({})
show("middleware call", [response[0], response[2].class.name, local.local_cache.class.name])
response[2].close
show("middleware close", local.local_cache.inspect)
raising = middleware.new(->(_env) { raise Rack::Utils::InvalidParameterError, "bad" })
show("middleware invalid params", [raising.call({}), local.local_cache.inspect])
events

# --- NullStore and lookup_store -----------------------------------------
null = ActiveSupport::Cache::NullStore.new
show("null store", [null.write("k", 1), null.read("k"), null.exist?("k"), null.increment("k"), null.delete("k"), null.clear, null.inspect])
null.with_local_cache { show("null local", [null.write("k", 1), null.read("k"), null.fetch("k2") { 2 }, null.read("k2")]) }
show("lookup nil", ActiveSupport::Cache.lookup_store.class.name)
show("lookup symbol", [ActiveSupport::Cache.lookup_store(:memory_store, size: 10).class.name, ActiveSupport::Cache.lookup_store(:memory_store, size: 10).instance_variable_get(:@max_size)])
show("lookup array", ActiveSupport::Cache.lookup_store([:null_store, { namespace: "x" }]).namespace)
show("lookup instance", ActiveSupport::Cache.lookup_store(local).equal?(local))
show("unimplemented", [outcome { ActiveSupport::Cache::Store.new.increment("x") }, outcome { ActiveSupport::Cache::Store.new.read("x") }, outcome { ActiveSupport::Cache::Store.new.clear }])
show("new_entry", null.new_entry("v", expires_in: 60).expires_at.nil?)
show("expand_cache_key", [ActiveSupport::Cache.expand_cache_key("a"), ActiveSupport::Cache.expand_cache_key([:a, "b", 1], "ns"), ActiveSupport::Cache.expand_cache_key(Record.new(3, "v")), ActiveSupport::Cache.expand_cache_key(Versioned.new), ActiveSupport::Cache.expand_cache_key([nil, true, false]), ActiveSupport::Cache.expand_cache_key({ b: [1, 2], a: { c: "d e" } })])
ENV["RAILS_CACHE_ID"] = "build7"
show("expand_cache_key with cache id", ActiveSupport::Cache.expand_cache_key("a", "ns"))
ENV.delete("RAILS_CACHE_ID")
show("constants", [ActiveSupport::Cache::UNIVERSAL_OPTIONS, ActiveSupport::Cache::OPTION_ALIASES, ActiveSupport::Cache::DEFAULT_COMPRESS_LIMIT, ActiveSupport::Cache::Store::MAX_KEY_SIZE, ActiveSupport::Cache::Store::DEFAULT_POOL_OPTIONS])
show("pool options", [ActiveSupport::Cache::Store.send(:retrieve_pool_options, {}), ActiveSupport::Cache::Store.send(:retrieve_pool_options, { pool: false }), ActiveSupport::Cache::Store.send(:retrieve_pool_options, { pool: { size: "2" } }), outcome { ActiveSupport::Cache::Store.send(:retrieve_pool_options, { pool: 1 }) }])
show("ancestors", ActiveSupport::Cache::MemoryStore.ancestors.take_while { |m| m != Object }.map(&:name))
events

# --- IsolatedExecutionState ---------------------------------------------
state = ActiveSupport::IsolatedExecutionState
state[:k] = 1
show("state", [state[:k], state.key?(:k), state.key?(:zz), state.isolation_level])
show("state thread", Thread.new { [state[:k], state.key?(:k)] }.value)
show("state fiber", Fiber.new { state[:k] }.resume)
other = Thread.new { state[:t] = 2; state[:drop] = 3; Thread.current }.tap(&:join).value
state.share_with(other, except: :drop) { show("share_with", [state[:t], state[:drop], state[:k]]) }
show("after share_with", [state[:t], state[:k]])
show("state delete", [state.delete(:k), state[:k]])
state.isolation_level = :fiber
state[:f] = 1
show("fiber isolation", [Fiber.new { state[:f] }.resume, state[:f], state.isolation_level, state.scope.name])
show("bad isolation", outcome { state.isolation_level = :process })
state.isolation_level = :thread
show("back to thread", [state[:f], state.isolation_level])
state.clear
show("events at end", events)
