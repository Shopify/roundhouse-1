# Runtime probe for tests/futamura.rs (class_bind_runtime): real ActiveRecord on SQLite.
# usage: ruby class_bind_probe.rb futamura.rb   (FUTAMURA_VERIFY=1 optional; PREPARED=1 for prepared statements)
#
# `where(column: Klass)`: PredicateBuilder#build reads `value.id` only of a value that answers it, and
# a Class does not (Class has no `id`); the handler is the default BasicObject one, so the condition
# is `column = <bind>` with the Class as the value. A connection without prepared statements inlines
# it with `quote(Class)` (`'#{value}'`, Class#to_s: no sti_name, no polymorphic_name); with prepared
# statements `type_cast(Class)` raises TypeError. Either way the statement serializes the same
# QueryAttribute (`BindMap#bind`), so only a Class that answers `id` (`def self.id`) must run Rails'.
# A Relation value is a subquery built from the relation's own Arel (RelationHandler): Rails' to run.
require "active_record"
require "concurrent"
require "set"
require "bigdecimal"
ActiveRecord::Base.establish_connection(adapter: "sqlite3", database: ":memory:", prepared_statements: ENV["PREPARED"] == "1")
ActiveRecord::Schema.verbose = false
ActiveRecord::Schema.define do
  create_table(:vehicles) { |t| t.string :type; t.string :name }
  create_table(:marks) { |t| t.integer :owner_id; t.string :owner_type; t.string :kind }
  create_table(:wheels) { |t| t.integer :vehicle_id; t.string :kind }
end
class Vehicle < ActiveRecord::Base; has_many :wheels; end
class Car < Vehicle; end
class Wheel < ActiveRecord::Base; belongs_to :vehicle; end
class Mark < ActiveRecord::Base; belongs_to :owner, polymorphic: true; end
module Fleet; class Truck < Vehicle; end; end
# A class that answers `id`: the predicate builder reads it as the key.
class Keyed; def self.id = 2; end

load ARGV[0]
Futamura.define(:by_type, "Vehicle", one: true, via: :find_by) { |p| Vehicle.all.where(type: p.bind).limit(1) }
Futamura.define(:by_owner_type, "Mark", one: true, via: :where) { |p| Mark.all.where(owner_type: p.bind).limit(1) }
Futamura.define(:by_id, "Vehicle", one: true, via: :where) { |p| Vehicle.all.where(id: p.bind).limit(1) }
Futamura.define(:wheel_vehicles, "Vehicle", via: :where) { |p| Vehicle.all.where(id: p.bind) }

$fallbacks = 0
def run(key, binds, &chain)
  before = $fallbacks
  result = begin
    value = Futamura.records(key, binds) { $fallbacks += 1; chain.call }
    value.is_a?(Array) ? value.map(&:id) : value&.id
  rescue => e
    e.class
  end
  "#{result.inspect}/#{$fallbacks - before}"
end

Car.create!(name: "car") # ids: car 1, plain 2, truck 3, second 4
Vehicle.create!(name: "plain")
Fleet::Truck.create!(name: "truck")
Vehicle.create!(name: "second")
Mark.create!(owner_id: 1, owner_type: "Vehicle", kind: "m")
Mark.create!(owner_id: 1, owner_type: "Fleet::Truck", kind: "m")
Wheel.create!(vehicle_id: 1, kind: "w")
Wheel.create!(vehicle_id: 3, kind: "w")

out = {}
out["sti"] = {
  subclass: run(:by_type, [Car]) { Vehicle.find_by(type: Car) },
  namespaced: run(:by_type, [Fleet::Truck]) { Vehicle.find_by(type: Fleet::Truck) },
  base: run(:by_type, [Vehicle]) { Vehicle.find_by(type: Vehicle) },
  string: run(:by_type, ["Car"]) { Vehicle.find_by(type: "Car") },
}
out["polymorphic"] = {
  class: run(:by_owner_type, [Fleet::Truck]) { Mark.where(owner_type: Fleet::Truck).first },
  base: run(:by_owner_type, [Vehicle]) { Mark.where(owner_type: Vehicle).first },
}
out["key"] = {
  keyed_class: run(:by_id, [Keyed]) { Vehicle.where(id: Keyed).first },
}
sub = Wheel.where(kind: "w").select(:vehicle_id)
out["relation"] = {
  subquery: run(:wheel_vehicles, [sub]) { Vehicle.where(id: sub).to_a },
}
out.each { |name, group| puts "#{name}=" + group.map { |k, v| "#{k}:#{v}" }.join(" ") }
