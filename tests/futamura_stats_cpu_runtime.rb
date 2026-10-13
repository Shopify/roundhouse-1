# Runtime probe for tests/futamura.rs (stats_cpu_runtime): the CPU accounting of the FUTAMURA_STATS block of futamura.rb on real
# ActiveRecord (SQLite). usage: ruby futamura_stats_cpu_runtime.rb futamura.rb   (FUTAMURA_VERIFY=1 optional)
#
# FUTAMURA_STATS_AB=1 and FUTAMURA_STATS_HELPERS=1 are set here: every specialized call after a site's first is timed against Rails' chain,
# and each guard helper is timed. Prints one line per fact the test reads.
require "active_record"
require "concurrent"
require "json"
require "set"
require "tmpdir"
dir = Dir.mktmpdir
ENV["FUTAMURA_STATS"] = dir
ENV["FUTAMURA_STATS_AB"] = "1"
ENV["FUTAMURA_STATS_HELPERS"] = "1"
ActiveRecord::Base.establish_connection(adapter: "sqlite3", database: ":memory:")
ActiveRecord::Schema.verbose = false
ActiveRecord::Schema.define do
  create_table(:things) { |t| t.string :code }
end
class ApplicationRecord < ActiveRecord::Base; self.abstract_class = true; end
class Thing < ApplicationRecord; end
load ARGV[0]
Futamura.define(:thing, "Thing", one: true, via: :where) { |p| Thing.all.where(code: p.bind).limit(1) }
Thing.create!(code: "x")

chain_runs = 0
8.times { Futamura.records(:thing, ["x"]) { chain_runs += 1; Thing.where(code: "x").first } }
# A call a statement cannot serve (an Array of nine values): Rails' chain, once, after the guards.
Futamura.records(:thing, [%w[a b c d e f g h i]]) { chain_runs += 1; Thing.where(code: %w[a b c d e f g h i]).first }
Futamura::Stats.dump
doc = JSON.parse(File.read(Dir[File.join(dir, "futamura-stats-*.json")].first))
site = doc["sites"]["thing"]
puts "calls=#{site["calls"]} specialized=#{site["specialized"]} fallback=#{site["fallback"].values.sum}"
# AB: the chain runs again beside each specialized call after the first (alternating before and after): 7 samples.
puts "ab_calls=#{site["ab_calls"]} chain_runs=#{chain_runs}"
puts "guard_cpu_positive=#{site["guard_cpu"].positive?} exec_cpu_positive=#{site["exec_cpu"].positive?} fallback_cpu_positive=#{site["fallback_cpu"].positive?}"
puts "ab_cpu_positive=#{site["ab_spec_cpu"].positive? && site["ab_orig_cpu"].positive?} compile_cpu_positive=#{site["compile_cpu"].positive?}"
# The phases are disjoint parts of the calls: together they are less than the wall time of the specialized and fallback paths.
puts "phases_within_wall=#{site["guard_cpu"] + site["exec_cpu"] + site["fallback_cpu"] < site["specialized_s"] + site["fallback_s"] + site["guard_s"] + 0.05}"
puts "clock_read_ns=#{doc["clock_read_ns"].positive?}"
helpers = doc["helpers"]
puts "helpers=#{%w[bind_shape redefined_class_method default_binds].all? { |name| helpers[name] && helpers[name]["calls"] >= 8 }}"
puts "helper_self_le_inclusive=#{helpers.values.all? { |h| h["self_cpu"] <= h["inclusive_cpu"] + 1e-9 }}"
puts "compiles=#{doc["compiles"].size} compile_cpu=#{doc["compiles"].all? { |c| c["cpu"] }}"
