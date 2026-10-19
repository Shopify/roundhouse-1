//! `Rails.cache.fetch(key, expires_in: …) { … }` and the rest of the
//! Cache API's option-taking methods, on both stores the CRuby tree can
//! carry: the bounded `ActiveSupport::Cache::MemoryStore` of
//! runtime/spinel/active_support_cache_memory.rb, and the ported Rails
//! store of runtime/spinel/active_support_cache.rb under the test
//! overlay's `MemoryStore::Instrumented` (test_support_cruby.rb), whose
//! `fetch(name)` once refused Core's `fetch(key, expires_in: …)`.

use std::path::Path;
use std::process::Command;

fn run_ruby(script: &str) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = Command::new("ruby")
        .arg("-e")
        .arg(script)
        .current_dir(root)
        .output()
        .expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stdout.contains("ALL OK") && out.status.success(),
        "driver failed ({:?})\n=== stdout ===\n{stdout}\n=== stderr ===\n{stderr}",
        out.status.code()
    );
}

const BOUNDED: &str = r##"
require_relative "runtime/ruby/rails"
require_relative "runtime/spinel/active_support_cache_memory"
def refuses(what)
  yield
  raise "#{what} did not raise"
rescue ArgumentError => e
  e.message
end
"##;

#[test]
fn bounded_store_fetch_takes_options_and_a_block() {
    run_ruby(&format!(
        "{BOUNDED}{}",
        r##"
store = ActiveSupport::Cache::MemoryStore.new
calls = 0
# The keyword form Core writes, and the positional Hash Rails also takes.
raise "first" unless store.fetch("k", expires_in: 60) { calls += 1; "v" } == "v"
raise "hit" unless store.fetch("k", expires_in: 60) { calls += 1; "other" } == "v"
raise "positional" unless store.fetch("p", { expires_in: 60 }) { calls += 1; "pv" } == "pv"
raise "calls #{calls}" unless calls == 2
# The block gets the name, as Rails yields it.
raise "yielded" unless store.fetch("named") { |name| name * 2 } == "namednamed"

# expires_in is honored (a Duration-like #to_f works too).
store.write("short", "old", expires_in: 0.05)
raise "live" unless store.read("short") == "old" && store.exist?("short")
sleep 0.12
raise "expired read" unless store.read("short").nil?
raise "expired exist" if store.exist?("short")
raise "refetch" unless store.fetch("short", expires_in: 60) { "new" } == "new"
raise "expired entry freed" unless store.inspect.include?("entries=")
# expires_at is the same, as a Time.
store.write("at", 1, expires_at: Time.now + 0.05)
sleep 0.12
raise "expires_at" unless store.read("at").nil?
refuses("both") { store.write("b", 1, expires_in: 1, expires_at: Time.now + 1) }
refuses("negative") { store.write("n", 1, expires_in: -1) }

# force: recompute and overwrite; without a block it is an error.
store.write("f", "stale")
raise "force" unless store.fetch("f", force: true) { "fresh" } == "fresh"
raise "forced write" unless store.read("f") == "fresh"
refuses("force without block") { store.fetch("f", force: true) }
raise "blockless read" unless store.fetch("f") == "fresh"
raise "blockless miss" unless store.fetch("absent").nil?

# skip_nil: a nil result is returned but not cached.
n = 0
store.fetch("nil", skip_nil: true) { n += 1; nil }
store.fetch("nil", skip_nil: true) { n += 1; nil }
raise "skip_nil recomputes #{n}" unless n == 2
store.fetch("nil2") { nil }
raise "nil cached by default" unless store.exist?("nil2")

# race_condition_ttl: an expired entry stays readable for the TTL while
# one fetch recomputes; a second fetch in that window sees the stale value.
store.write("race", "stale", expires_in: 0.05)
sleep 0.12
seen = nil
fresh = store.fetch("race", expires_in: 60, race_condition_ttl: 10) do
  seen = store.fetch("race", expires_in: 60, race_condition_ttl: 10) { "second recompute" }
  "fresh"
end
raise "race fresh #{fresh}" unless fresh == "fresh"
raise "race window #{seen.inspect}" unless seen == "stale"
raise "race after #{store.read('race').inspect}" unless store.read("race") == "fresh"
# Past the TTL, the stale value is dropped.
store.write("late", "stale", expires_in: 0.05)
sleep 0.2
raise "late" unless store.fetch("late", race_condition_ttl: 0.1) { "fresh" } == "fresh"

# namespace files the entry apart.
store.write("shared", "a", namespace: "one")
store.write("shared", "b", namespace: "two")
raise "ns one" unless store.read("shared", namespace: "one") == "a"
raise "ns two" unless store.read("shared", namespace: "two") == "b"
raise "ns none" unless store.read("shared").nil?
raise "ns fetch" unless store.fetch("shared", namespace: "one") { "x" } == "a"
raise "ns exist" unless store.exist?("shared", namespace: "two")
store.delete("shared", namespace: "one")
raise "ns delete" unless store.read("shared", namespace: "one").nil? && store.read("shared", namespace: "two") == "b"

# An option the store does not carry is refused by name, not dropped.
msg = refuses("version") { store.fetch("v", version: 2) { 1 } }
raise msg unless msg.include?(":version")
refuses("compress") { store.write("c", 1, compress: true) }
refuses("read option") { store.read("c", raw: true) }
refuses("exist option") { store.exist?("c", use_default_route: true) }
refuses("delete option") { store.delete("c", use_default_route: true) }
puts "ALL OK"
"##
    ));
}

#[test]
fn bounded_store_multi_and_counter_methods_take_options() {
    run_ruby(&format!(
        "{BOUNDED}{}",
        r##"
store = ActiveSupport::Cache::MemoryStore.new
store.write_multi({ "a" => 1, "b" => 2 }, expires_in: 0.05, namespace: "m")
raise "multi ns" unless store.read_multi("a", "b", "c", namespace: "m") == { "a" => 1, "b" => 2 }
raise "multi bare" unless store.read_multi("a", "b") == {}
sleep 0.12
raise "multi expired" unless store.read_multi("a", "b", namespace: "m") == {}

calls = []
got = store.fetch_multi("x", "y", expires_in: 60) { |name| calls << name; name.upcase }
raise "fetch_multi #{got}" unless got == { "x" => "X", "y" => "Y" }
got = store.fetch_multi("x", "z", expires_in: 60) { |name| calls << name; name.upcase }
raise "fetch_multi hit #{got} #{calls}" unless got == { "x" => "X", "z" => "Z" } && calls == %w[x y z]
raise "fetch_multi force" unless store.fetch_multi("x", force: true) { "forced" } == { "x" => "forced" }
store.fetch_multi("nilv", skip_nil: true) { nil }
raise "fetch_multi skip_nil" if store.exist?("nilv")
refuses("fetch_multi block") { store.fetch_multi("x") }
refuses("fetch_multi version") { store.fetch_multi("x", version: 1) { 1 } }

# increment / decrement create an absent counter at the amount, advance a
# live one, and keep its expiry.
raise "incr create" unless store.increment("c", 1, expires_in: 0.2) == 1
raise "incr" unless store.increment("c", 4) == 5
raise "decr" unless store.decrement("c", 2) == 3
raise "counter value" unless store.read("c") == 3
sleep 0.3
raise "counter expiry kept" unless store.read("c").nil?
raise "ns counter" unless store.increment("c", 7, namespace: "n") == 7 && store.read("c", namespace: "n") == 7
refuses("increment option") { store.increment("c", 1, version: 1) }

# write unless_exist keeps the live entry.
store.write("u", 1)
raise "unless_exist" unless store.write("u", 2, unless_exist: true) == false && store.read("u") == 1
raise "unless_exist miss" unless store.write("u2", 2, unless_exist: true) == true

# cleanup drops the expired.
store.write("gone", 1, expires_in: 0.05)
sleep 0.12
store.cleanup
raise "cleanup" if store.inspect.include?("gone") || store.read("gone")
puts "ALL OK"
"##
    ));
}

/// The ported store with the overlay's instrumenting prepend, the shape
/// the Core CRuby tree runs. The overlay is sliced out of
/// test_support_cruby.rb (the rest of the file needs the whole harness).
const PORT_WITH_OVERLAY: &str = r##"
require_relative "runtime/spinel/active_support_cache"
overlay = File.read("runtime/spinel/scaffold/ruby_overlay/test/test_support_cruby.rb")
from = overlay.index("module ActiveSupport\n  module Cache\n    class MemoryStore\n      unless private_method_defined?(:instrument)") or raise "overlay moved"
upto = overlay.index("# Rails' collection renderer") or raise "overlay moved"
eval(overlay[from...upto], TOPLEVEL_BINDING)
"##;

#[test]
fn overlay_fetch_forwards_options_and_the_block_to_the_store() {
    run_ruby(&format!(
        "{PORT_WITH_OVERLAY}{}",
        r##"
store = ActiveSupport::Cache::MemoryStore.new
raise "overlay stacked on the port" if ActiveSupport::Cache::MemoryStore.const_defined?(:Instrumented, false)
events = []
ActiveSupport::Notifications.subscribe("cache_read.active_support") { |*args| events << args.last }

# Core's call shape: keywords and a block.
calls = 0
raise "miss" unless store.fetch("k", expires_in: 60) { calls += 1; "v" } == "v"
raise "hit" unless store.fetch("k", expires_in: 60) { calls += 1; "other" } == "v"
raise "calls" unless calls == 1
raise "positional hash" unless store.fetch("p", { expires_in: 60 }) { "pv" } == "pv"
raise "multiple options" unless store.fetch("m", expires_in: 60, race_condition_ttl: 10, skip_nil: true) { "mv" } == "mv"
raise "no options, no block" unless store.fetch("k") == "v"

# The overlay reports the fetch's read with the hit that options decide:
# a namespaced entry is a hit only when asked for under its namespace.
store.write("ns", "x", namespace: "one")
events.clear
store.fetch("ns", namespace: "one") { "never" }
raise "ns hit #{events.inspect}" unless events.any? { |e| e[:super_operation] == :fetch && e[:hit] == true }
events.clear
store.fetch("ns") { "computed" }
raise "ns miss #{events.inspect}" unless events.any? { |e| e[:super_operation] == :fetch && e[:hit] == false }

# expires_in is honored through the overlay.
store.fetch("short", expires_in: 0.05) { "old" }
sleep 0.12
raise "expired" unless store.fetch("short", expires_in: 60) { "new" } == "new"

# force and skip_nil reach the store.
raise "force" unless store.fetch("k", force: true) { "forced" } == "forced"
n = 0
2.times { store.fetch("nil", skip_nil: true) { n += 1; nil } }
raise "skip_nil #{n}" unless n == 2
begin
  store.fetch("k", force: true)
  raise "force without block did not raise"
rescue ArgumentError
end
puts "ALL OK"
"##
    ));
}

#[test]
fn overlay_other_cache_methods_take_options_as_rails_does() {
    run_ruby(&format!(
        "{PORT_WITH_OVERLAY}{}",
        r##"
store = ActiveSupport::Cache::MemoryStore.new
events = []
ActiveSupport::Notifications.subscribe("cache_read_multi.active_support") { |*args| events << args.last }

store.write("a", 1, expires_in: 60, namespace: "n")
raise "read ns" unless store.read("a", namespace: "n") == 1
raise "read bare" unless store.read("a").nil?
raise "exist? ns" unless store.exist?("a", namespace: "n") && !store.exist?("a")
store.write("e", 1, expires_in: 0.05)
sleep 0.12
raise "write expires_in" unless store.read("e").nil?

# read_multi: a trailing options Hash is options, not a key the event names.
store.write_multi({ "x" => 1, "y" => 2 }, expires_in: 60, namespace: "m")
events.clear
raise "read_multi" unless store.read_multi("x", "y", "z", namespace: "m") == { "x" => 1, "y" => 2 }
# One event, the store's own (namespaced keys), not listing the options
# Hash as a key.
raise "read_multi events #{events.inspect}" unless events.length == 1
raise "options listed as a key #{events.inspect}" unless events.all? { |e| e[:key].length == 3 && e[:key].none? { |k| k.include?("namespace") } }
raise "store event" unless events.any? { |e| e[:key] == %w[m:x m:y m:z] && e[:hits] == %w[m:x m:y] }
events.clear
store.read_multi("x", "y")
raise "bare names" unless events.all? { |e| e[:key].length == 2 }

got = store.fetch_multi("p", "q", expires_in: 60) { |name| name * 2 }
raise "fetch_multi #{got}" unless got == { "p" => "pp", "q" => "qq" }
raise "delete ns" unless store.delete("a", namespace: "n") && store.read("a", namespace: "n").nil?

# The port's MemoryStore counts; the overlay does not stand in the way.
raise "incr" unless store.increment("cnt", 2, expires_in: 60) == 2
raise "incr again" unless store.increment("cnt") == 3
raise "decr" unless store.decrement("cnt", 1) == 2
puts "ALL OK"
"##
    ));
}

/// The bounded store with the same overlay: the overlay instruments it
/// (it does not instrument itself), and each operation reports once.
const BOUNDED_WITH_OVERLAY: &str = r##"
require_relative "runtime/ruby/rails"
require_relative "runtime/spinel/active_support_cache_memory"
require_relative "runtime/spinel/active_support_notifications"
overlay = File.read("runtime/spinel/scaffold/ruby_overlay/test/test_support_cruby.rb")
from = overlay.index("module ActiveSupport\n  module Cache\n    class MemoryStore\n      unless private_method_defined?(:instrument)") or raise "overlay moved"
upto = overlay.index("# Rails' collection renderer") or raise "overlay moved"
eval(overlay[from...upto], TOPLEVEL_BINDING)
EVENTS = []
%w[read write read_multi write_multi exist? delete].each do |op|
  ActiveSupport::Notifications.subscribe("cache_#{op}.active_support") { |name, *rest| EVENTS << [name.sub(".active_support", ""), rest.last] }
end
def seen
  EVENTS.clear
  yield
  EVENTS.map(&:first)
end
"##;

#[test]
fn overlay_reports_each_event_once_on_the_bounded_store() {
    run_ruby(&format!(
        "{BOUNDED_WITH_OVERLAY}{}",
        r##"
store = ActiveSupport::Cache::MemoryStore.new
raise "overlay not prepended" unless ActiveSupport::Cache::MemoryStore::Instrumented.instance_method(:read)
raise "overlay not prepended" unless store.singleton_class.ancestors.include?(ActiveSupport::Cache::MemoryStore::Instrumented)

# write: one cache_write, with the key.
raise "write #{seen { store.write("a", 1) }}" unless seen { store.write("a", 1) } == ["cache_write"]
store.write("a", 1)
raise "write payload #{EVENTS.last.inspect}" unless EVENTS.last[1][:key] == "a" && EVENTS.last[1][:store] == "ActiveSupport::Cache::MemoryStore"

# read: one cache_read with hit, hit or miss; the hit probe is no event.
raise "read hit #{seen { store.read("a") }}" unless seen { store.read("a") } == ["cache_read"]
store.read("a")
raise "payload #{EVENTS.last[1].inspect}" unless EVENTS.last[1][:key] == "a" && EVENTS.last[1][:hit] == true
raise "read miss" unless seen { store.read("zz") } == ["cache_read"] && EVENTS.last[1][:hit] == false
raise "no super_operation on read" if EVENTS.last[1].key?(:super_operation)

# exist?: one cache_exist?, nothing more.
raise "exist? #{seen { store.exist?("a") }}" unless seen { store.exist?("a") } == ["cache_exist?"]

# fetch miss: the read (super_operation: :fetch, hit: false), then the write.
names = seen { store.fetch("f", expires_in: 60) { "v" } }
raise "fetch miss #{names}" unless names == ["cache_read", "cache_write"]
raise "fetch miss payload #{EVENTS.inspect}" unless EVENTS[0][1][:super_operation] == :fetch && EVENTS[0][1][:hit] == false && EVENTS[0][1][:key] == "f" && EVENTS[0][1][:expires_in] == 60
# fetch hit: the read alone.
names = seen { store.fetch("f", expires_in: 60) { "other" } }
raise "fetch hit #{names}" unless names == ["cache_read"]
raise "fetch hit payload #{EVENTS.inspect}" unless EVENTS[0][1][:super_operation] == :fetch && EVENTS[0][1][:hit] == true
# blockless fetch is a plain read, once.
raise "blockless fetch #{seen { store.fetch("f") }}" unless seen { store.fetch("f") } == ["cache_read"]
store.fetch("f")
raise "blockless payload" if EVENTS.last[1].key?(:super_operation)
# force: no read, one write. skip_nil with nil: a read, no write.
raise "force #{seen { store.fetch("f", force: true) { "n" } }}" unless seen { store.fetch("f", force: true) { "n" } } == ["cache_write"]
raise "skip_nil #{seen { store.fetch("sn", skip_nil: true) { nil } }}" unless seen { store.fetch("sn", skip_nil: true) { nil } } == ["cache_read"]
# a namespaced entry is a hit under its namespace, keyed with it.
store.write("ns", 1, namespace: "one")
seen { store.fetch("ns", namespace: "one") { 2 } }
raise "ns #{EVENTS.inspect}" unless EVENTS.length == 1 && EVENTS[0][1][:hit] == true && EVENTS[0][1][:key] == "one:ns"

# read_multi: one event, the names (options Hash not among them) and hits.
raise "read_multi #{seen { store.read_multi("a", "f", "zz") }}" unless seen { store.read_multi("a", "f", "zz") } == ["cache_read_multi"]
store.read_multi("a", "f", "zz")
raise "read_multi payload #{EVENTS.last[1].inspect}" unless EVENTS.last[1][:key] == %w[a f zz] && EVENTS.last[1][:hits] == %w[a f]
store.read_multi("a", "zz", expires_in: 60)
raise "options as key #{EVENTS.last[1].inspect}" unless EVENTS.last[1][:key] == %w[a zz]

# write_multi: one cache_write_multi, not one write per name.
raise "write_multi #{seen { store.write_multi({ "m" => 1, "n" => 2 }) }}" unless seen { store.write_multi({ "m" => 1, "n" => 2 }) } == ["cache_write_multi"]
store.write_multi({ "m" => 1, "n" => 2 })
raise "write_multi key #{EVENTS.last[1].inspect}" unless EVENTS.last[1][:key] == %w[m n]
raise "write after write_multi" unless seen { store.write("w", 1) } == ["cache_write"]

# fetch_multi: one read_multi (super_operation: :fetch_multi), one write_multi for the misses.
names = seen { store.fetch_multi("m", "x", "y") { |n| n * 2 } }
raise "fetch_multi #{names}" unless names == ["cache_read_multi", "cache_write_multi"]
raise "fetch_multi payload #{EVENTS.inspect}" unless EVENTS[0][1][:super_operation] == :fetch_multi && EVENTS[0][1][:hits] == %w[m] && EVENTS[1][1][:key] == %w[x y]
raise "fetch_multi all hits #{seen { store.fetch_multi("m", "x") { |n| n } }}" unless seen { store.fetch_multi("m", "x") { |n| n } } == ["cache_read_multi"]

# a failing subscriber surfaces from the write, once.
raised = 0
sub = ActiveSupport::Notifications.subscribe("cache_write.active_support") { raised += 1; raise IOError, "Cache write failed" }
begin
  store.write("boom", 1)
  raise "write did not raise"
rescue IOError
end
ActiveSupport::Notifications.unsubscribe(sub)
raise "raised #{raised}" unless raised == 1
puts "ALL OK"
"##
    ));
}

#[test]
fn overlay_reports_each_event_once_on_the_ported_store() {
    run_ruby(&format!(
        "{PORT_WITH_OVERLAY}{}",
        r##"
store = ActiveSupport::Cache::MemoryStore.new
events = []
%w[read write read_multi write_multi exist? delete].each do |op|
  ActiveSupport::Notifications.subscribe("cache_#{op}.active_support") { |name, *rest| events << [name.sub(".active_support", ""), rest.last] }
end
seen = ->(&blk) { events.clear; blk.call; events.map(&:first) }
raise "overlay stacked on the port" if ActiveSupport::Cache::MemoryStore.const_defined?(:Instrumented, false)
raise "stacked" if store.singleton_class.ancestors.any? { |a| a.name.to_s.end_with?("Instrumented") }

raise "write #{events.inspect}" unless seen.call { store.write("a", 1) } == ["cache_write"]
raise "write key" unless events[0][1][:key] == "a" && events[0][1][:store] == "ActiveSupport::Cache::MemoryStore"
raise "read" unless seen.call { store.read("a") } == ["cache_read"]
raise "read payload #{events.inspect}" unless events[0][1][:key] == "a" && events[0][1][:hit] == true
raise "read miss" unless seen.call { store.read("zz") } == ["cache_read"] && events[0][1][:hit] == false
raise "exist? #{events.inspect}" unless seen.call { store.exist?("a") } == ["cache_exist?"]
names = seen.call { store.fetch("f", expires_in: 60) { "v" } }
raise "fetch miss #{names}" unless names == ["cache_read", "cache_write"]
raise "fetch payload #{events.inspect}" unless events[0][1][:super_operation] == :fetch && events[0][1][:hit] == false
names = seen.call { store.fetch("f", expires_in: 60) { "other" } }
raise "fetch hit #{names}" unless names == ["cache_read"] && events[0][1][:super_operation] == :fetch && events[0][1][:hit] == true
raise "read_multi" unless seen.call { store.read_multi("a", "f", "zz") } == ["cache_read_multi"]
raise "read_multi payload #{events.inspect}" unless events[0][1][:key] == %w[a f zz] && events[0][1][:hits] == %w[a f]
puts "ALL OK"
"##
    ));
}
