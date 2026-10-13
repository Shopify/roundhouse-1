# Runtime probe for tests/futamura.rs (composite_keys_and_scoping_blocks): the emitted
# tree on real ActiveRecord (SQLite). usage: ruby probe.rb EMITTED_DIR
# Every call goes through Futamura.records, which under FUTAMURA_VERIFY=1 raises
# when the specialized statement and Rails' own chain disagree.
require "active_record"
require "concurrent"
require "set"
require "tmpdir"

dir = ARGV[0]
ENV["FUTAMURA_STATS"] = Dir.mktmpdir
ActiveRecord::Base.establish_connection(adapter: "sqlite3", database: ":memory:")
ActiveRecord::Schema.verbose = false
load File.join(dir, "db/schema.rb")
%w[application_record order line parcel].each { |m| load File.join(dir, "app/models/#{m}.rb") }
module Rails
  Config = Struct.new(:unused) { def to_prepare(&block) = block.call }
  def self.application = Struct.new(:config).new(Config.new)
end
load File.join(dir, "config/initializers/futamura.rb")
load File.join(dir, "app/services/finder.rb")

# A reader whose reflection compares more columns than the one the source names: a macro that
# widens the keys (a has_many with an array `foreign_key:`/`primary_key:`, which Rails turns
# into one equality per pair, association_scope.rb `last_chain_scope`).
Order._reflect_on_association(:lines).options.merge!(primary_key: %w[region_id id shop_id], foreign_key: %w[region_id order_id shop_id])
Order._reflect_on_association(:gifts).options.merge!(primary_key: %w[region_id id shop_id], foreign_key: %w[region_id order_id shop_id])

order = Order.create!(shop_id: 1, region_id: 9)
other = Order.create!(shop_id: 1, region_id: 8)
[[order, 1, 9, "a", false], [order, 1, 9, "b", true], [order, 1, 8, "wrong region", false], [order, 2, 9, "wrong shop", false],
 [other, 1, 8, "c", true], [other, 1, 9, "wrong region", false], [order, 1, nil, "no region", false], [order, 1, 9, "hidden", false]].each do |o, shop, region, sku, gift|
  Line.unscoped { Line.create!(order_id: o.id, shop_id: shop, region_id: region, sku: sku, gift: gift) }
end
Parcel.create!(order_id: order.id, shop_id: 1, region_id: 9)
Parcel.create!(order_id: order.id, shop_id: 1, region_id: 7)

def totals
  sites = Futamura::Stats.sites.values
  [sites.sum(&:specialized), sites.sum { |s| s.fallback.values.sum }]
end

def show(name)
  before = totals
  result = yield
  after = totals
  ids = Array(result).map { |r| r.respond_to?(:id) ? r.id : r }
  puts "#{name}=#{ids.inspect} spec=#{after[0] - before[0]} fall=#{after[1] - before[1]}"
end

show("lines") { Finder.lines(order) }
show("lines_other") { Finder.lines(other) }
show("gifts") { Finder.gifts(order) }
show("parcels") { Finder.parcels(order) }
show("by_sku") { Finder.by_sku("a") }
show("of_order") { Finder.of_order(order.id) }
# Model.scoping: the chain is on the scope's clone, with no default scope added.
show("scoped_sku") { Line.where(sku: "a").scoping { Finder.of_order(order.id) } }
show("scoped_sku_other") { Line.where(sku: "b").scoping { Finder.of_order(order.id) } }
show("scoped_unscoped") { Line.unscoped.where(shop_id: 2).scoping { Finder.of_order(order.id) } }
show("scoped_ordered") { Line.order(id: :desc).scoping { Finder.of_order(order.id) } }
show("scoped_in") { Line.where(sku: %w[a b]).scoping { Finder.of_order(order.id) } }
show("scoped_reader") { Line.where(sku: "a").scoping { Finder.lines(order) } }
show("scoped_assoc_reader_ids") { Line.where(sku: "a").scoping { Finder.by_sku("b") } }
# A nil in one of the owner's key columns is `column IS NULL`, not a placeholder: Rails' chain.
bare = Order.create!(shop_id: 1, region_id: nil)
Line.unscoped { Line.create!(order_id: bare.id, shop_id: 1, region_id: nil, sku: "d") }
show("lines_nil_region") { Finder.lines(bare) }
