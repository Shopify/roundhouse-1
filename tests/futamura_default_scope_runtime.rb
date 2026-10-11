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
  create_table(:plain) { |t| t.string :code }
  create_table(:legacy) { |t| t.integer :shop_id; t.string :code }
end
class Current; class << self; attr_accessor :shop_id, :evaluated; end; end
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
load ARGV[0]
Futamura.define(:item, Item, one: true) { |p| Item.all.where(code: p.bind).send(:ordered_relation).limit(1) }
Futamura.define(:item_unscoped, Item, one: true, unscoped: true) { |p| Item.unscoped.where(code: p.bind).send(:ordered_relation).limit(1) }
Futamura.define(:ranked, Ranked, one: true) { |p| Ranked.all.where(code: p.bind).send(:ordered_relation).limit(1) }
Futamura.define(:plain, Plain, one: true) { |p| Plain.all.where(code: p.bind).send(:ordered_relation).limit(1) }
Futamura.define(:legacy, Legacy, one: true) { |p| Legacy.all.where(code: p.bind).send(:ordered_relation).limit(1) }

$fallbacks = 0; $log = []
def look(key, model, code, tag = nil)
  Futamura.records(key, [code]) { $fallbacks += 1; $log << [key, :fallback]; model.where(code: code).first }&.id
end
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
puts out.map { |k, v| "#{k}=#{v.inspect}" }
puts "fallbacks=#{$fallbacks}"
puts $log.map(&:inspect).join(" ")
