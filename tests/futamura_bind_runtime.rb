# Runtime probe for tests/futamura.rs (bind_runtime): real ActiveRecord on SQLite.
# usage: ruby bind_probe.rb futamura.rb   (FUTAMURA_VERIFY=1 optional)
#
# A StatementCache placeholder stands for one scalar value. A value Rails' predicate builder
# expands (an Array, a Set, a Range, a Relation, a Hash, a record, a composite association key)
# or one a test double stands for must never reach it: Rails' own chain runs instead.
require "active_record"
require "concurrent"
require "set"
require "bigdecimal"
ActiveRecord::Base.establish_connection(adapter: "sqlite3", database: ":memory:")
ActiveRecord::Schema.verbose = false
ActiveRecord::Schema.define do
  create_table(:crates) { |t| t.string :code; t.integer :zone; t.boolean :open; t.date :day; t.datetime :at; t.decimal :weight }
  create_table(:docks, primary_key: [:shop_id, :id]) { |t| t.integer :shop_id; t.integer :id }
  create_table(:items) { |t| t.integer :shop_id; t.integer :dock_id; t.integer :crate_id }
end
class Crate < ActiveRecord::Base; end
class Dock < ActiveRecord::Base; self.primary_key = [:shop_id, :id]; end
class Item < ActiveRecord::Base
  belongs_to :crate
  has_many :siblings, class_name: "Item", foreign_key: :crate_id, primary_key: :crate_id
  # A composite target: the key is more than one column.
  belongs_to :dock, foreign_key: [:shop_id, :dock_id]
end
class Crate
  has_many :items
end
# `belongs_to ..., query_constraints:` (Rails 8.2): the join keys are single columns, but the
# predicate builder reads the composite query keys.
$constrained = begin
  Item.belongs_to :constrained_dock, class_name: "Dock", foreign_key: :dock_id, query_constraints: [:shop_id]
  true
rescue ArgumentError, ActiveRecord::ConfigurationError
  false
end

load ARGV[0]
Futamura.define(:by_id, "Crate", one: true, via: :where) { |p| Crate.all.where(id: p.bind).limit(1) }
Futamura.define(:by_code, "Crate", one: true, via: :where) { |p| Crate.all.where(code: p.bind).limit(1) }
Futamura.define(:by_open, "Crate", one: true, via: :where) { |p| Crate.all.where(open: p.bind).limit(1) }
Futamura.define(:by_day, "Crate", one: true, via: :where) { |p| Crate.all.where(day: p.bind).limit(1) }
Futamura.define(:by_at, "Crate", one: true, via: :where) { |p| Crate.all.where(at: p.bind).limit(1) }
Futamura.define(:by_weight, "Crate", one: true, via: :where) { |p| Crate.all.where(weight: p.bind).limit(1) }
Futamura.define(:by_crate, "Item", one: true, via: :where) { |p| Item.all.where(crate: p.bind).limit(1) }
Futamura.define(:by_dock, "Item", one: true, via: :where) { |p| Item.all.where(dock: p.bind).limit(1) }
# A build that raises over the placeholder (here a method called on it), as Rails does for a
# composite key it reads from a value.
Futamura.define(:unbuildable, "Crate", one: true, via: :where) { |p| Crate.all.where(id: p.bind.no_such_method).limit(1) }
Futamura.define(:crate_items, "Item", one: true, relation: true, via: :where, owner_keys: ["crate_id"], owner_columns: ["crate_id"]) { |p| Item.all.where(crate_id: p.bind).limit(1) }

$fallbacks = 0
def run(key, binds, **opts, &chain)
  Futamura.records(key, binds, **opts) { $fallbacks += 1; chain.call }&.id
end
def counted
  before = $fallbacks
  result = yield
  [result, $fallbacks - before]
end

# A test double stands for an object: it answers only what it expects, and raises for the rest.
class Double < BasicObject
  def initialize(*expected) = @expected = expected
  def method_missing(name, *) = @expected.include?(name) ? :expected : ::Kernel.raise("unexpected invocation: #{name}")
  def respond_to_missing?(*) = true
end

now = Time.utc(2026, 10, 12, 8, 30, 0)
crates = [
  Crate.create!(code: "a", zone: 1, open: true, day: Date.new(2026, 10, 12), at: now, weight: BigDecimal("1.5")),
  Crate.create!(code: "b", zone: 2, open: false),
  Crate.create!(code: "c", zone: 3),
]
a, b, c = crates
out = {}

# 1. Scalars are one placeholder each (no fallback); every collection or record Rails expands is
# Rails' (one fallback), and the chain answers the way Rails' does for it.
out["scalar"] = {
  integer: counted { run(:by_id, [a.id]) { Crate.where(id: a.id).first } },
  string: counted { run(:by_code, ["b"]) { Crate.where(code: "b").first } },
  symbol: counted { run(:by_code, [:c]) { Crate.where(code: :c).first } },
  true: counted { run(:by_open, [true]) { Crate.where(open: true).first } },
  false: counted { run(:by_open, [false]) { Crate.where(open: false).first } },
  date: counted { run(:by_day, [Date.new(2026, 10, 12)]) { Crate.where(day: Date.new(2026, 10, 12)).first } },
  time: counted { run(:by_at, [now]) { Crate.where(at: now).first } },
  zoned: counted { run(:by_at, [now.in_time_zone("UTC")]) { Crate.where(at: now.in_time_zone("UTC")).first } },
  decimal: counted { run(:by_weight, [BigDecimal("1.5")]) { Crate.where(weight: BigDecimal("1.5")).first } },
}.transform_values { |(id, fallbacks)| "#{id}/#{fallbacks}" }
ids = [a.id, c.id]
out["collection"] = {
  array: counted { run(:by_id, [ids]) { Crate.where(id: ids).first } },
  set: counted { run(:by_id, [Set.new(ids)]) { Crate.where(id: Set.new(ids)).first } },
  range: counted { run(:by_id, [(b.id..c.id)]) { Crate.where(id: (b.id..c.id)).first } },
  endless: counted { run(:by_id, [(b.id..)]) { Crate.where(id: (b.id..)).first } },
  relation: counted { run(:by_id, [Crate.where(zone: 3)]) { Crate.where(id: Crate.where(zone: 3)).first } },
  empty_set: counted { run(:by_id, [Set.new]) { Crate.where(id: Set.new).first } },
  record: counted { run(:by_id, [b]) { Crate.where(id: b).first } },
  nil: counted { run(:by_code, [nil]) { Crate.where(code: nil).first } },
}.transform_values { |(id, fallbacks)| "#{id.inspect}/#{fallbacks}" }

# 2. An association-named condition on a composite key is Rails'; on a single column it is the
# key. (`dock` is a composite target; `constrained_dock` adds query constraints, Rails 8.2.)
dock = Dock.create!(id: [1, 1])
Item.create!(shop_id: 1, dock_id: 1, crate_id: a.id)
out["association"] = {
  single: counted { run(:by_crate, [Futamura.association_key(Item, :crate, a)]) { Item.where(crate: a).first } },
  composite_key: Futamura.association_key(Item, :dock, dock).inspect,
  composite: counted { run(:by_dock, [Futamura.association_key(Item, :dock, dock)]) { Item.where(dock: dock).first } },
}.transform_values { |v| v.is_a?(Array) ? "#{v[0].inspect}/#{v[1]}" : v }
if $constrained
  constrained = Futamura.association_key(Item, :constrained_dock, dock)
  Futamura.define(:by_constrained, "Item", one: true, via: :where) { |p| Item.all.where(constrained_dock: p.bind).limit(1) }
  out["association"][:constrained_key] = constrained.inspect
  out["association"][:constrained] = counted { run(:by_constrained, [constrained]) { Item.where(constrained_dock: dock).first } }.then { |(id, f)| "#{id.inspect}/#{f}" }
else
  out["association"][:constrained_key] = "n/a"
  out["association"][:constrained] = "n/a"
end

# 3. A chain that cannot be built over placeholders is Rails'; the refusal is remembered.
out["unbuildable"] = [2, 3].map { |code| counted { run(:unbuildable, [code]) { Crate.where(id: code).first } } }

# 4. An owner that is a test double is asked nothing: not by a guard, not for its key. The binds
# of a site rooted at an association reader are a lambda, called for a record owner only.
asked = []
binds = -> { asked << :binds; [a.id] }
double = Double.new(:items)
out["owner"] = {
  double: counted { run(:crate_items, binds, owner: double, association: :items) { Item.where(crate_id: a.id).first } }.then { |(id, f)| "#{id}/#{f}" },
  asked: asked.inspect,
}
out["owner"][:record] = counted { run(:crate_items, binds, owner: a, association: :items) { Item.where(crate_id: a.id).first } }.then { |(id, f)| "#{id}/#{f}" }
out["owner"][:asked_after] = asked.inspect

# 5. A value of a class outside the scalar list is bound when the predicate builder would build
# `column = <bind>` for it whatever it is (no record key to read, no handler but the default),
# and is Rails' when it would not.
class TypedId # wraps an integer key the way a value object does: serialized by the column's type (to_i)
  def initialize(value) = @value = value
  def to_i = @value
end
IdBearer = Struct.new(:id) # PredicateBuilder#build reads value.id
Pair = Struct.new(:first, :second)
# Rails 8.2 makes a predicate builder shareable (frozen) when it is built: no handler can be added.
$handler = begin
  Crate.predicate_builder.register_handler(Pair, ->(attribute, pair) { attribute.eq(pair.first) })
  true
rescue FrozenError
  false
end
Futamura.define(:by_zone, "Crate", one: true, via: :where) { |p| Crate.all.where(zone: p.bind).limit(1) }
out["typed"] = {
  value_object: counted { run(:by_zone, [TypedId.new(3)]) { Crate.where(zone: TypedId.new(3)).first } },
  record_key: counted { run(:by_zone, [IdBearer.new(2)]) { Crate.where(zone: IdBearer.new(2)).first } },
  handler: $handler ? counted { run(:by_zone, [Pair.new(1, 2)]) { Crate.where(zone: Pair.new(1, 2)).first } } : "n/a",
  hash: counted { run(:by_zone, [{ zone: 3 }]) { Crate.where(zone: { zone: 3 }).first } },
}.transform_values { |v| v.is_a?(Array) ? "#{v[0].inspect}/#{v[1]}" : v }

# 6. nil is `IS NULL`: one statement for each pattern of nil positions among the binds (here
# `code` then `zone`), up to MAX_NILS; more is Rails'.
Crate.create!(code: "d", zone: nil)
Crate.create!(code: nil, zone: 4)
Crate.create!(code: nil, zone: nil)
Futamura.define(:by_both, "Crate", one: true, via: :where) { |p| Crate.all.where(code: p.bind).where(zone: p.bind).limit(1) }
both = ->(code, zone) { counted { run(:by_both, [code, zone]) { Crate.where(code: code).where(zone: zone).first } }.then { |(id, f)| "#{id.inspect}/#{f}" } }
out["nil"] = {
  none: both.call("b", 2),
  first: both.call(nil, 4),
  second: both.call("d", nil),
  both: both.call(nil, nil),
  none_again: both.call("c", 3),
  first_again: both.call(nil, 4),
  missing: both.call(nil, 99),
  many: counted { run(:by_both, [nil, nil, nil, nil]) { Crate.where(code: nil).where(zone: nil).first } }.then { |(id, f)| "#{id.inspect}/#{f}" },
}

out.each do |name, group|
  puts "#{name}=" + (group.is_a?(Hash) ? group.map { |k, v| "#{k}:#{v}" }.join(" ") : group.inspect)
end
