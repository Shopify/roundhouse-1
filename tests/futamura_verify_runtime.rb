# Runtime probe for tests/futamura.rs (verify_runtime): real ActiveRecord on SQLite.
# usage: ruby futamura_verify_runtime.rb futamura.rb   (FUTAMURA_VERIFY=0|1)
#
# FUTAMURA_VERIFY=1 runs Rails' chain beside the specialized statement and compares both. That
# comparison must not be seen by the application: a `sql.active_record` subscriber counts the
# statements of the specialized run alone, and a preload through a polymorphic association (whose
# targets are of classes that do not all have the next association) compares the way Rails'
# Preloader::Branch#grouped_records loads it.
require "active_record"
require "concurrent"
require "set"
ActiveRecord::Base.establish_connection(adapter: "sqlite3", database: ":memory:")
ActiveRecord::Schema.verbose = false
ActiveRecord::Schema.define do
  create_table(:countries) { |t| t.string :name }
  create_table(:provinces) { |t| t.string :name; t.integer :country_id }
  create_table(:regions) { |t| t.string :code; t.string :place_type; t.integer :place_id }
  create_table(:notes) { |t| t.string :code }
end
class ApplicationRecord < ActiveRecord::Base; self.abstract_class = true; end
class Country < ApplicationRecord; end
# Only a Province has a `country`: a Region's place can be either.
class Province < ApplicationRecord; belongs_to :country; end
class Region < ApplicationRecord; belongs_to :place, polymorphic: true; end
class Note < ApplicationRecord; end

load ARGV[0]

Futamura.define(:regions, Region, via: :where) { |p| Region.all.preload(place: :country).where(code: p.bind) }
Futamura.define(:notes, Note, via: :where) { |p| Note.all.where(code: p.bind) }

country = Country.create!(name: "c")
province = Province.create!(name: "p", country: country)
Region.create!(code: "x", place: country)
Region.create!(code: "x", place: province)
Note.create!(code: "x")

$fallbacks = 0
$events = []
ActiveSupport::Notifications.subscribe("sql.active_record") do |*, payload|
  $events << payload[:name] unless payload[:name] == "SCHEMA"
end

def counted
  $events.clear
  value = yield
  [value, $events.dup]
end

# Statements the application's subscriber sees for one specialized call, with the query cache on and off.
def look(key, model, code)
  records, events = counted do
    Futamura.records(key, [code]) { $fallbacks += 1; model.where(code: code).to_a }
  end
  [records.map(&:id), events.group_by(&:itself).transform_values(&:size).sort.to_h]
end

# The same call twice inside a query cache, as a controller action sees it.
def look_cached(key, model, code)
  ActiveRecord::Base.cache do
    first = look(key, model, code)
    second = look(key, model, code)
    [first, second]
  end
end

puts "notes=#{look(:notes, Note, 'x').inspect}"
puts "notes_cached=#{look_cached(:notes, Note, 'x').inspect}"

# The shadow runs in the same transaction: a row created inside it is read by both.
ActiveRecord::Base.transaction do
  Note.create!(code: "y")
  puts "notes_in_transaction=#{look(:notes, Note, 'y').inspect}"
  raise ActiveRecord::Rollback
end
puts "notes_after_rollback=#{look(:notes, Note, 'y').inspect}"

# The slot the shadow swaps is restored, also when Rails' chain raises.
slot = ActiveSupport::IsolatedExecutionState[:active_record_instrumenter]
begin
  Futamura.records(:notes, ["x"]) { raise "boom" }
rescue RuntimeError
end
puts "slot_restored=#{ActiveSupport::IsolatedExecutionState[:active_record_instrumenter].equal?(slot)}"

# A polymorphic preload: the Country place has no `country`; Rails' Preloader skips it.
Futamura.records(:regions, ["x"]) { Region.where(code: "x").preload(place: :country).to_a }.then do |records|
  places = records.map { |r| [r.place_type, r.place.id] }
  puts "regions=#{places.inspect}"
end
puts "fallbacks=#{$fallbacks}"
