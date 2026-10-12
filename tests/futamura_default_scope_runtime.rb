# Runtime probe for tests/futamura.rs (default_scope_runtime): real ActiveRecord on SQLite.
# usage: ruby dscope_probe.rb futamura.rb   (FUTAMURA_VERIFY=1 optional)
require "active_record"
require "concurrent"
require "set"
ActiveRecord::Base.establish_connection(adapter: "sqlite3", database: ":memory:")
ActiveRecord::Schema.verbose = false
ActiveRecord::Schema.define do
  create_table(:items) { |t| t.integer :shop_id; t.string :code; t.boolean :archived, default: false }
  create_table(:ranked) { |t| t.string :code }
  create_table(:plain) { |t| t.string :code; t.integer :owner_id }
  create_table(:legacy) { |t| t.integer :shop_id; t.string :code }
  %i[isles guarded anons loaders flips].each { |name| create_table(name) { |t| t.integer :shop_id; t.string :code } }
end
class Current; class << self; attr_accessor :shop_id, :evaluated, :flip; end; end
Current.evaluated = 0
class ApplicationRecord < ActiveRecord::Base; self.abstract_class = true; end
# A default scope that reads run-time state and branches: nil state adds nothing.
class Item < ApplicationRecord
  default_scope do
    Current.evaluated += 1
    Current.shop_id ? where(shop_id: Current.shop_id, archived: false) : nil
  end
end
class Ranked < ApplicationRecord; self.table_name = "ranked"; default_scope { order(:id) }; end
class Plain < ApplicationRecord; self.table_name = "plain"; end
class Legacy < ApplicationRecord
  self.table_name = "legacy"
  def self.default_scope = where(shop_id: Current.shop_id)
end
# `extending` only adds modules to the relation (query_methods.rb extending!). A named module that overrides
# a Relation method the chain does not call is accepted...
module ShopRelation
  def unscope(*args) = super
  def shop_scoped? = true
end
class Isle < ApplicationRecord
  default_scope { extending(ShopRelation).where(shop_id: Current.shop_id) }
end
# ...one that overrides a link the chain calls is run by Rails (the statement would run it once)...
module WhereOverride
  def where(*args) = super
end
class Guarded < ApplicationRecord
  self.table_name = "guarded"
  default_scope { extending(WhereOverride).where(shop_id: Current.shop_id) }
end
# ...and so is one made by a block (not a constant, a new module at every call), or one overriding a loader.
class Anon < ApplicationRecord
  self.table_name = "anons"
  default_scope { extending { def anon? = true }.where(shop_id: Current.shop_id) }
end
module LoadOverride
  def load(*) = super
end
class Loader < ApplicationRecord
  self.table_name = "loaders"
  default_scope { extending(LoadOverride).where(shop_id: Current.shop_id) }
end
# A default scope that is not a plain condition only while the flag is set.
class Flip < ApplicationRecord
  self.table_name = "flips"
  default_scope { Current.flip ? order(:id) : where(shop_id: Current.shop_id) }
end
load ARGV[0]
Futamura.define(:item, Item, one: true) { |p| Item.all.where(code: p.bind).send(:ordered_relation).limit(1) }
Futamura.define(:item_unscoped, Item, one: true, unscoped: true) { |p| Item.unscoped.where(code: p.bind).send(:ordered_relation).limit(1) }
Futamura.define(:ranked, Ranked, one: true) { |p| Ranked.all.where(code: p.bind).send(:ordered_relation).limit(1) }
Futamura.define(:plain, "Plain", one: true) { |p| Plain.all.where(code: p.bind).send(:ordered_relation).limit(1) }
Futamura.define(:plain_owner, "Plain", one: true) { |p| Plain.all.where(owner_id: p.bind).limit(1) }
Futamura.define(:legacy, Legacy, one: true) { |p| Legacy.all.where(code: p.bind).send(:ordered_relation).limit(1) }
Futamura.define(:plain_find, "Plain", one: true, via: :find_by) { |p| Plain.all.where(code: p.bind).limit(1) }
%i[isle guarded anon loader flip].each do |name|
  klass = name.to_s.capitalize.constantize
  Futamura.define(name, klass, one: true) { |p| klass.all.where(code: p.bind).send(:ordered_relation).limit(1) }
end

$fallbacks = 0; $log = []
def look(key, model, code, tag = nil)
  Futamura.records(key, [code]) { $fallbacks += 1; $log << [key, :fallback]; model.where(code: code).first }&.id
end
[Isle, Guarded, Anon, Loader, Flip].each { |m| m.unscoped { m.create!(shop_id: 1, code: "x"); m.create!(shop_id: 2, code: "x") } }
Item.unscoped { Item.create!(shop_id: 1, code: "x"); Item.create!(shop_id: 2, code: "x"); Item.create!(shop_id: nil, code: "x"); Item.create!(shop_id: 1, code: "y", archived: true) }
Ranked.create!(code: "x"); Plain.create!(code: "x")
Legacy.unscoped { Legacy.create!(shop_id: 1, code: "x"); Legacy.create!(shop_id: nil, code: "x") }
out = {}
[1, 2, 3, nil, 1].each { |s| Current.shop_id = s; out["item/#{s.inspect}"] ||= []; out["item/#{s.inspect}"] << look(:item, Item, "x") }
Current.shop_id = 1
out["archived"] = look(:item, Item, "y")
out["unscoped"] = look(:item_unscoped, Item, "x")
out["scoping"] = Item.where(shop_id: 2).scoping { look(:item, Item, "x") }
out["unscoped_block"] = Item.unscoped { look(:item, Item, "x") }
out["ordered"] = [look(:ranked, Ranked, "x"), look(:ranked, Ranked, "x")]
out["plain"] = look(:plain, Plain, "x")
Current.shop_id = 1; a = look(:legacy, Legacy, "x"); Current.shop_id = nil; b = look(:legacy, Legacy, "x")
out["legacy"] = [a, b]
Current.shop_id = 2
Current.evaluated = 0; before = $fallbacks
look(:item, Item, "x")
out["calls_item"] = [Current.evaluated, $fallbacks - before]
Current.shop_id = 2
compiled = Futamura.instance_variable_get(:@compiled)
%i[isle guarded anon loader].each do |name|
  klass = name.to_s.capitalize.constantize
  before = $fallbacks
  out["ext_#{name}"] = [look(name, klass, "x")]
  # How many calls ran Rails' chain (verify runs it beside the statement, so only the result is compared).
  puts "calls_ext_#{name}=#{$fallbacks - before} built=#{compiled.keys.select { |k| k.first == name }.any? { |k| compiled[k] }}"
end
# A default scope refused five times is not refused for good: it is evaluated again every OPAQUE_RETRY calls.
Current.flip = true
(Futamura::OPAQUE_LIMIT + 3).times { look(:flip, Flip, "x") }
opaque = Futamura.instance_variable_get(:@opaque)
refused_for_good = opaque[Flip].first >= Futamura::OPAQUE_LIMIT
Current.flip = false
calls = 0
calls += 1 while look(:flip, Flip, "x") && opaque[Flip] && calls < Futamura::OPAQUE_RETRY * 2
out["flip"] = [refused_for_good, calls < Futamura::OPAQUE_RETRY, opaque[Flip].nil?]
# `Model.find_by(owner_id: record)` reads the record's id (core.rb find_by); the call site passes find_by_value.
Ref = Struct.new(:id)
Plain.create!(code: "owned", owner_id: 7)
out["find_by_id"] = [Futamura.records(:plain_owner, [Futamura.find_by_value(Ref.new(7))]) { Plain.find_by(owner_id: Ref.new(7)) }&.code]
# A chain that starts with a class method the class itself defines (a test double, a model's override) is
# Rails': the statement would answer beside the double.
via = ->(double) { Futamura.records(:plain_find, ["x"]) { $fallbacks += 1; double || Plain.find_by(code: "x") } }
before = $fallbacks
plain_specialized = via.call(nil)&.id
Plain.define_singleton_method(:find_by) { |*| :double }
doubled = via.call(:double)
Plain.singleton_class.send(:remove_method, :find_by)
again = via.call(nil)&.id
# Mocha prepends an anonymous module to the class's singleton instead.
Plain.singleton_class.prepend(Module.new { def find_by(*) = :prepended })
prepended = via.call(:prepended)
out["via"] = [plain_specialized, doubled, again, prepended]
puts "calls_via=#{$fallbacks - before}"
puts out.map { |k, v| "#{k}=#{v.inspect}" }
puts "fallbacks=#{$fallbacks}"
puts $log.map(&:inspect).join(" ")
