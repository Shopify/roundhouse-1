# Runtime probe for tests/futamura.rs (locals_assigned_on_several_paths):
# real ActiveRecord on SQLite, running the emitted initializer and model.
# usage: ruby futamura_flow_runtime.rb <emitted tree dir>   (FUTAMURA_VERIFY=1 optional)
require "active_record"
require "concurrent"
require "set"
ActiveRecord::Base.establish_connection(adapter: "sqlite3", database: ":memory:")
ActiveRecord::Schema.verbose = false
load File.join(ARGV[0], "db/schema.rb")
class ApplicationRecord < ActiveRecord::Base; self.abstract_class = true; end
# The initializer registers its statements when Rails prepares the app.
module Rails
  PREPARE = Object.new.tap { |o| o.define_singleton_method(:to_prepare) { |&block| block.call } }
  def self.application = Struct.new(:config).new(PREPARE)
end
load File.join(ARGV[0], "config/initializers/futamura.rb")
load File.join(ARGV[0], "app/models/note.rb")
load File.join(ARGV[0], "app/models/item.rb")

[[1, "open", "a"], [1, "closed", "a"], [1, "open", "b"], [2, "open", "a"], [2, "closed", "b"]].each do |shop, status, kind|
  Item.create!(shop_id: shop, status: status, kind: kind)
end
fallbacks = 0
Futamura.singleton_class.prepend(Module.new do
  define_method(:records) { |key, binds, **o, &fb| super(key, binds, **o) { fallbacks += 1; fb.call } }
end)
[[1, nil, nil], [1, "open", nil], [1, nil, "a"], [1, "open", "a"], [1, "", "b"], [2, "closed", "b"], [3, "open", "a"]].each do |shop, status, kind|
  got = Item.find_all(shop, status, kind).map(&:id)
  want = Item.where(shop_id: shop)
  want = want.where(status: status) if status.present?
  want = want.where(kind: kind) if kind.present?
  want = want.order(:id).map(&:id)
  puts "#{[shop, status, kind].inspect} #{got == want ? "ok" : "MISMATCH #{got.inspect} #{want.inspect}"} #{got.inspect}"
end
item = Item.first
[3, 1, 0].each { |n| item.notes.create!(body: "n#{n}") if n > 0 }
# A fresh owner: creating through the association fills its target, and the
# runtime then leaves the call to Rails (see Futamura.compile).
item = Item.find(item.id)
got = item.recent_notes.map(&:body)
want = item.notes.order(id: :desc).limit(2).map(&:body)
puts "recent_notes #{got == want ? "ok" : "MISMATCH"} #{got.inspect}"
puts "fallbacks=#{fallbacks}"
