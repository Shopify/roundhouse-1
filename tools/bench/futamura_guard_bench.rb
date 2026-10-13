# frozen_string_literal: true

# CPU microbenchmark of the futamura runtime's per-call cost on real ActiveRecord (SQLite, in memory).
#   ruby tools/bench/futamura_guard_bench.rb RUNTIME.rb [--n 20000] [--rounds 3] [--json]
#   FUTAMURA_STATS=/tmp/dir ruby tools/bench/futamura_guard_bench.rb RUNTIME.rb     # + per-phase us/call from Futamura::Stats
#   FUTAMURA_STATS=/tmp/dir FUTAMURA_STATS_HELPERS=1 ...                            # + the guard helpers one by one
#
# Each scenario calls the site both ways, interleaved ABAB: Rails' own chain (what the call site's block runs) and
# `Futamura.records` for the same call. Times are CPU of the calling thread (CLOCK_THREAD_CPUTIME_ID), microseconds per call;
# the database work is the same on both sides. `guard_only` is a call that stops after the guards: a site whose
# statement is built but whose binds are refused is not that, so it is measured as the difference of the two.
require "active_record"
require "concurrent"
require "json"
require "set"
require "tmpdir"

runtime = ARGV.shift or abort "usage: futamura_guard_bench.rb RUNTIME.rb [--n N] [--rounds R] [--json]"
n = 20_000
rounds = 3
json = false
until ARGV.empty?
  case (a = ARGV.shift)
  when "--n" then n = Integer(ARGV.shift)
  when "--rounds" then rounds = Integer(ARGV.shift)
  when "--json" then json = true
  else abort "unknown argument #{a}"
  end
end

ActiveRecord::Base.establish_connection(adapter: "sqlite3", database: ":memory:")
ActiveRecord::Schema.verbose = false
ActiveRecord::Schema.define do
  create_table(:shops) { |t| t.string :handle }
  create_table(:widgets) { |t| t.integer :shop_id; t.string :code; t.integer :owner_id }
  create_table(:owners) { |t| t.string :name }
end
class Current; class << self; attr_accessor :shop_id; end; end
class ApplicationRecord < ActiveRecord::Base; self.abstract_class = true; end
class Shop < ApplicationRecord; end
class Widget < ApplicationRecord
  default_scope { where(shop_id: Current.shop_id) }
  belongs_to :owner, optional: true
end
class Owner < ApplicationRecord
  has_many :widgets
end
Current.shop_id = 1
shop = Shop.create!(handle: "a")
owner = Owner.create!(name: "o")
Widget.insert_all((1..50).map { |i| { shop_id: 1, code: "c#{i}", owner_id: owner.id } })
Widget.unscoped.insert_all([{ shop_id: 2, code: "c1" }])

load runtime
Futamura.define(:find_by_id, "Shop", one: true, via: :find_by) { |p| Shop.all.where(id: p.bind).limit(1) }
Futamura.define(:find_by_scoped, "Widget", one: true, via: :find_by) { |p| Widget.all.where(code: p.bind).limit(1) }
Futamura.define(:where_scoped, "Widget", via: :where) { |p| Widget.all.where(code: p.bind) }
Futamura.define(:owner_widgets, "Widget", one: true, relation: true, via: :where, owner_keys: ["owner_id"], owner_columns: ["owner_id"]) { |p| Widget.all.where(owner_id: p.bind).where(code: p.bind).limit(1) }

scenarios = {
  "find_by plain" => [-> { Shop.find_by(id: shop.id) }, -> { Futamura.records(:find_by_id, [Futamura.find_by_value(shop.id)]) { Shop.find_by(id: shop.id) } }],
  "find_by default scope" => [-> { Widget.find_by(code: "c7") }, -> { Futamura.records(:find_by_scoped, [Futamura.find_by_value("c7")]) { Widget.find_by(code: "c7") } }],
  "where default scope (1 row)" => [-> { Widget.where(code: "c7").to_a }, -> { Futamura.records(:where_scoped, ["c7"]) { Widget.where(code: "c7").to_a } }],
  "association reader chain" => [-> { owner.widgets.where(code: "c9").first }, -> { Futamura.records(:owner_widgets, -> { [owner.id, "c9"] }, owner: owner, association: :widgets) { owner.widgets.where(code: "c9").first } }],
}

def cpu = Process.clock_gettime(Process::CLOCK_THREAD_CPUTIME_ID)

def time(n, call)
  started = cpu
  n.times { call.call }
  (cpu - started) / n * 1e6
end

# A call whose records are known, so that the two paths are not only fast but equal.
scenarios.each do |name, (rails, futamura)|
  a = Array(rails.call).map(&:id)
  b = Array(futamura.call).map(&:id)
  abort "#{name}: Rails #{a.inspect} != futamura #{b.inspect}" unless a == b
  2000.times { rails.call; futamura.call }
end

results = {}
scenarios.each do |name, (rails, futamura)|
  samples = { rails: [], futamura: [] }
  rounds.times do
    samples[:rails] << time(n, rails)
    samples[:futamura] << time(n, futamura)
  end
  med = ->(xs) { xs.sort[xs.size / 2] }
  results[name] = { rails_us: med.(samples[:rails]).round(2), futamura_us: med.(samples[:futamura]).round(2) }
  results[name][:delta_us] = (results[name][:futamura_us] - results[name][:rails_us]).round(2)
end

if defined?(Futamura::Stats)
  Futamura::Stats.sites.each do |key, s|
    calls = s.calls.to_f
    results["phases: #{key}"] = {
      calls: s.calls, specialized: s.specialized, guard_us: (s.guard_cpu / calls * 1e6).round(2),
      exec_us: (s.exec_cpu / [s.specialized, 1].max * 1e6).round(2), fallback_us: (s.fallback_cpu / [s.fallback.values.sum, 1].max * 1e6).round(2),
      compile_us_total: (s.compile_cpu * 1e6).round(0)
    }
  end
  results["clock_read_ns"] = Futamura::Stats.clock_read_ns
  results["helpers"] = Futamura::Stats.helpers.transform_values { |c, inclusive, own| { calls: c, inclusive_us: (inclusive / c * 1e6).round(3), self_us: (own / c * 1e6).round(3) } }
end

if json
  puts JSON.pretty_generate(results)
else
  results.each do |name, r|
    next if name == "helpers"
    puts format("%-40s %s", name, r.is_a?(Hash) ? r.map { |k, v| "#{k}=#{v}" }.join(" ") : r)
  end
  (results["helpers"] || {}).sort_by { |_, r| -r[:self_us] * r[:calls] }.first(14).each { |name, r| puts format("  helper %-26s calls=%d incl=%.3fus self=%.3fus", name, r[:calls], r[:inclusive_us], r[:self_us]) }
end
