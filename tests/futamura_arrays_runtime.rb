# Runtime probe for tests/futamura.rs (list_binds): the emitted tree on real ActiveRecord (SQLite).
# usage: ruby probe.rb EMITTED_DIR
# Every call goes through Futamura.records, which under FUTAMURA_VERIFY=1 raises when the specialized
# statement and Rails' own chain disagree. An Array bind is the IN list `where(column: [...])` builds
# (PredicateBuilder::ArrayHandler, Arel HomogeneousIn): one statement per length.
require "active_record"
require "concurrent"
require "set"
require "tmpdir"

dir = ARGV[0]
ENV["FUTAMURA_STATS"] = Dir.mktmpdir
ActiveRecord::Base.establish_connection(adapter: "sqlite3", database: ":memory:")
ActiveRecord::Schema.verbose = false
load File.join(dir, "db/schema.rb")
%w[application_record widget gadget].each { |m| load File.join(dir, "app/models/#{m}.rb") }
module Rails
  Config = Struct.new(:unused) { def to_prepare(&block) = block.call }
  def self.application = Struct.new(:config).new(Config.new)
end
load File.join(dir, "config/initializers/futamura.rb")
load File.join(dir, "app/services/finder.rb")

T0 = Time.utc(2020, 1, 1)
widgets = (1..6).map do |i|
  Widget.create!(shop_id: i.odd? ? 1 : 2, code: "c#{i}", rank: i % 3, stamp: T0 + i * 60, state: nil)
end
widgets.each { |w| Gadget.unscoped { 3.times { |t| Gadget.create!(widget_id: w.id, tenant_id: t + 1, name: "g#{w.id}-#{t}") } } }
Gadget.unscoped { Gadget.create!(widget_id: widgets[0].id, tenant_id: 9, name: "other tenant") }
ids = widgets.map(&:id)

def totals
  sites = Futamura::Stats.sites.values
  [sites.sum(&:specialized), sites.sum { |s| s.fallback.values.sum }]
end

def show(name)
  before = totals
  result = yield
  after = totals
  out = Array(result).map { |r| r.respond_to?(:id) ? r.id : r }
  puts "#{name}=#{out.inspect} spec=#{after[0] - before[0]} fall=#{after[1] - before[1]}"
end

# Lengths 1..4 (a length of one is an equality, ArrayHandler: `build(attribute, values.first)`), repeated so the
# second call of each length runs the statement the first built.
2.times do
  show("ids1") { Finder.by_ids([ids[2]]) }
  show("ids2") { Finder.by_ids([ids[4], ids[1]]) }
  show("ids3") { Finder.by_ids([ids[0], ids[5], ids[3]]) }
  show("ids4") { Finder.by_ids([ids[0], ids[1], ids[2], ids[3]]) }
end
show("ids_missing") { Finder.by_ids([ids[0], 999, 1000]) }
show("ids_dup") { Finder.by_ids([ids[0], ids[0], ids[1]]) }
show("ids_strings") { Finder.by_ids([ids[0].to_s, ids[1].to_s]) }
show("shop_ids") { Finder.shop_ids(1, ids) }
show("codes") { Finder.by_codes(%w[c1 c3 nope]) }
show("codes_symbols") { Finder.by_codes(%i[c2 c4]) }
show("ranks") { Finder.by_ranks([0, 2]) }
show("stamps") { Finder.by_stamps([T0 + 60, T0 + 180, T0 + 999]) }
show("not_ids") { Finder.not_ids([ids[0], ids[1]]) }
show("not_ids1") { Finder.not_ids([ids[0]]) }
show("unscoped_ids") { Finder.unscoped_ids([ids[1], ids[2]]) }
show("first_of") { Finder.first_of([ids[2], ids[0]], 1) }
show("two_lists") { Finder.two_lists([ids[0], ids[1], ids[2]], %w[c1 c3]) }
show("two_lists_mixed") { Finder.two_lists([ids[0]], %w[c1 c2]) }
# The default scope is an IN list too (tenant_id IN (1, 2, 3)).
show("gadgets") { Finder.gadgets_of(widgets[0], Gadget.unscoped.where(widget_id: widgets[0].id).order(:id).pluck(:id)) }

# What Rails builds differently is Rails' to run: an empty list (`1=0`), a nil element (`IS NULL` beside the
# list), a Range element, a list longer than the statements built, a record, a value the column cannot
# serialize (an integer out of range: HomogeneousIn drops it and the list is shorter).
show("empty") { Finder.by_ids([]) }
show("nil_element") { Finder.by_ids([ids[0], nil]) }
show("nil_only") { Finder.by_ids([nil]) }
show("range_element") { Finder.by_ids([ids[0], (ids[1]..ids[2])]) }
show("long") { Finder.by_ids(ids + ids) }
show("records") { Finder.by_ids([widgets[0], widgets[1]]) }
show("out_of_range") { Finder.by_ids([ids[0], 2**70, ids[1]]) }
show("out_of_range_only") { Finder.by_ids([2**70, 2**71]) }
show("not_numeric") { Finder.by_ids(["abc", ids[0].to_s]) }
show("set") { Finder.by_ids(Set[ids[0], ids[1]]) }
show("nested") { Finder.by_ids([[ids[0], ids[1]]]) }
# A list that came after a nil in the same call, and a nil beside lists.
show("nil_and_list") { Finder.two_lists([ids[0], ids[1]], nil) }
reasons = Futamura::Stats.sites.values.flat_map { |s| s.fallback.keys }.map { |r| r[/\Abind_array\[\w+\]/] }.compact.sort.uniq
puts "reasons=#{reasons.inspect}"
