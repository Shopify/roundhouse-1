# Runtime probe for tests/futamura.rs (composite_key_preload): real ActiveRecord on SQLite.
# usage: ruby futamura_cpk_preload_runtime.rb futamura.rb   (FUTAMURA_VERIFY=0|1, FUTAMURA_STATS=<dir>)
#
# Preloads whose reflections compare several columns (a has_many/belongs_to with array
# `primary_key:`/`foreign_key:`; Preloader::Association reads one key per column, and
# LoaderQuery#load_records_for_keys builds `where(column => values)` per column). Each case runs the
# same preload through Rails' own Preloader and through the runtime's, and prints whether the queries
# and the loaded targets are the same; under FUTAMURA_VERIFY=1 the runtime also checks itself.
require "active_record"
require "concurrent"
require "set"

fixture = File.expand_path("fixtures/futamura_cpk_app", __dir__)
ActiveRecord::Base.establish_connection(adapter: "sqlite3", database: ":memory:")
ActiveRecord::Schema.verbose = false
load File.join(fixture, "db/schema.rb")
%w[application_record order line parcel crate].each { |m| load File.join(fixture, "app/models/#{m}.rb") }
load ARGV[0]

KEYS = { primary_key: %w[region_id id shop_id], foreign_key: %w[region_id order_id shop_id] }.freeze
Order._reflect_on_association(:lines).options.merge!(KEYS)
Order._reflect_on_association(:gifts).options.merge!(KEYS)
Order._reflect_on_association(:crates).options.merge!(KEYS)
Line._reflect_on_association(:order).options.merge!(foreign_key: KEYS[:foreign_key], primary_key: KEYS[:primary_key])
# Lists of different lengths: the owner's side names two columns, the record's three.
Order._reflect_on_association(:parcels).options.merge!(primary_key: %w[id shop_id], foreign_key: %w[order_id shop_id region_id])

Futamura::Preload.static_scopes("Order#gifts")
Futamura.define(:orders_lines, Order, via: :where) { |p| Order.all.preload(:lines).where(shop_id: p.bind) }
Futamura.define(:orders_gifts, Order, via: :where) { |p| Order.all.preload(:gifts).where(shop_id: p.bind) }
Futamura.define(:orders_crates, Order, via: :where) { |p| Order.all.preload(:crates).where(shop_id: p.bind) }
Futamura.define(:orders_parcels, Order, via: :where) { |p| Order.all.preload(:parcels).where(shop_id: p.bind) }
Futamura.define(:lines_order, Line, via: :where) { |p| Line.all.preload(:order).where(shop_id: p.bind) }
Futamura.define(:orders_lines_order, Order, via: :where) { |p| Order.all.preload(lines: :order).where(shop_id: p.bind) }

one = Order.create!(shop_id: 1, region_id: 9)
two = Order.create!(shop_id: 1, region_id: 8)
# The same id can exist in two regions' keys: only the whole key matches an owner.
bare = Order.create!(shop_id: 1, region_id: nil)
[[one, 1, 9, "a", false], [one, 1, 9, "b", true], [one, 1, 8, "wrong region", false], [one, 2, 9, "wrong shop", false],
 [two, 1, 8, "c", true], [two, 1, 9, "wrong region", false], [one, 1, nil, "no region", false], [bare, 1, nil, "orphan of nil", false]].each do |o, shop, region, sku, gift|
  Line.unscoped { Line.create!(order_id: o.id, shop_id: shop, region_id: region, sku: sku, gift: gift) }
end
Crate.unscoped { Crate.create!(order_id: one.id, shop_id: 1, region_id: 9, sku: "x") }
Parcel.create!(order_id: one.id, shop_id: 1, region_id: 9)

$queries = []
ActiveSupport::Notifications.subscribe("sql.active_record") do |*, payload|
  $queries << payload[:sql] if payload[:name].to_s.end_with?(" Load")
end

def targets(owners, names)
  owners.map { |o| [o.id, names.map { |n| a = o.association(n); a.loaded? ? Array.wrap(a.target).map(&:id) : :not_loaded }] }
end

def compare(name, key, names, &rails)
  $queries.clear
  expected = rails.call
  expected_queries = $queries.drop(1) # the owners' own query comes first
  expected_targets = targets(expected, names)
  $queries.clear
  actual = Futamura.records(key, [1]) { rails.call }
  actual_queries = $queries.drop(1)
  same = expected_targets == targets(actual, names)
  puts "#{name}=#{same ? "same" : "DIFFERENT #{expected_targets.inspect} vs #{targets(actual, names).inspect}"} queries=#{actual_queries.size}/#{expected_queries.size} sql=#{actual_queries == expected_queries}"
  puts "#{name}_targets=#{expected_targets.inspect}"
  puts "#{name}_sql=#{actual_queries.first}"
end

compare("lines", :orders_lines, [:lines]) { Order.where(shop_id: 1).preload(:lines).to_a }
compare("gifts", :orders_gifts, [:gifts]) { Order.where(shop_id: 1).preload(:gifts).to_a }
compare("crates", :orders_crates, [:crates]) { Order.where(shop_id: 1).preload(:crates).to_a }
compare("lines_order", :lines_order, [:order]) { Line.where(shop_id: 1).preload(:order).to_a }
compare("nested", :orders_lines_order, [:lines]) { Order.where(shop_id: 1).preload(lines: :order).to_a }
compare("parcels", :orders_parcels, [:parcels]) { Order.where(shop_id: 1).preload(:parcels).to_a }

plans = Futamura::Preload.instance_variable_get(:@plans)
shape = plans.each_pair.map { |(spec, root), plan| [spec, root.name, String === plan ? plan : plan.map { |n| [n.name, n.children.map(&:name)] }] }.sort_by(&:inspect)
puts "plans=#{shape.inspect}"
puts "refusals=#{Futamura::Stats.preload_refusals.sort.to_h.inspect}" if defined?(Futamura::Stats)
