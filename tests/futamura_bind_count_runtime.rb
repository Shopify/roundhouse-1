# Runtime probe for tests/futamura.rs (bind_count_runtime): real ActiveRecord on SQLite.
# usage: ruby bind_count_probe.rb futamura.rb   (FUTAMURA_VERIFY=1 optional)
#
# A statement is compiled over placeholders and run with the values in order, so it must keep
# exactly one slot for each placeholder it was built with. An integer IN list does not: Rails
# serializes a placeholder to nil and compacts it away (HomogeneousIn#casted_values). And a chain
# whose build raises an Exception that is not a StandardError (ActiveSupport::ErrorReporter's
# UnexpectedError) is not a crash of the program but a chain Rails runs.
require "active_record"
require "concurrent"
require "set"
ActiveRecord::Base.establish_connection(adapter: "sqlite3", database: ":memory:")
ActiveRecord::Schema.verbose = false
ActiveRecord::Schema.define do
  %i[ranks names].each { |name| create_table(name) { |t| t.integer :rank; t.string :label; t.string :code } }
  create_table(:children) { |t| t.string :state }
  create_table(:parents) { |t| t.string :code; t.integer :child_id }
end
class ApplicationRecord < ActiveRecord::Base; self.abstract_class = true; end
# An IN list over an integer column in a default scope, and the same over a string column.
class Rank < ApplicationRecord; default_scope { where(rank: [1, 2]) }; end
class Name < ApplicationRecord; default_scope { where(label: %w[a b]) }; end
# A joined model's default scope (Reflection#join_scope) is evaluated while the statement is built.
$state = "a"
class Child < ApplicationRecord; default_scope { where(state: $state) }; end
class Parent < ApplicationRecord; belongs_to :child; end

load ARGV[0]
class Unexpected < Exception; end

[Rank, Name].each do |klass|
  Futamura.define(klass.name.downcase.to_sym, klass, via: :where) { |p| klass.all.where(code: p.bind).where.not(id: p.bind).limit(5) }
end
Futamura.define(:raising, Rank, unscoped: true, via: :where) do |p|
  value = p.bind
  raise Unexpected, "built over a placeholder" if value.is_a?(ActiveRecord::StatementCache::Substitute)
  Rank.unscoped.where(code: value)
end
Futamura.define(:joined, Parent, via: :where) { |p| Parent.all.joins(:child).where(code: p.bind) }

$fallbacks = 0
Rank.unscoped { [[1, "x"], [2, "x"], [3, "x"], [2, "y"]].each { |rank, code| Rank.create!(rank: rank, code: code) } }
Name.unscoped { [["a", "x"], ["b", "x"], ["c", "x"], ["b", "y"]].each { |label, code| Name.create!(label: label, code: code) } }

def look(key, *binds)
  before = $fallbacks
  records = Futamura.records(key, binds) { $fallbacks += 1; key == :name ? Name.where(code: binds[0]).where.not(id: binds[1]).limit(5).to_a : Rank.where(code: binds[0]).where.not(id: binds[1]).limit(5).to_a }
  [records.map(&:id), $fallbacks - before]
end

%i[rank name].each do |key|
  looks = [look(key, "x", 1), look(key, "x", 99)]
  puts "#{key}=#{looks.inspect}"
  puts "#{key}.ids=#{looks.map(&:first).inspect}"
end
before = $fallbacks
result = Futamura.records(:raising, ["x"]) { $fallbacks += 1; Rank.unscoped.where(code: "x").to_a }
puts "raising=#{[result.map(&:id), $fallbacks - before].inspect}"

# The joined model's default scope reads `$state` at every call; a statement built once would keep the first.
Child.unscoped { %w[a b].each { |state| Parent.create!(code: "x", child: Child.create!(state: state)) } }
joined = %w[a b a].map do |state|
  $state = state
  before = $fallbacks
  records = Futamura.records(:joined, ["x"]) { $fallbacks += 1; Parent.joins(:child).where(code: "x").to_a }
  [records.map(&:id), $fallbacks - before]
end
puts "joined=#{joined.inspect}"
puts "joined.ids=#{joined.map(&:first).inspect}"

# Joined models whose default scopes have values (an IN list over an integer column, an equality) that
# change between calls, joined twice (the second under an alias), under a default scope of the
# chain's own model. The joins' placeholders come first in the SQL, before the ones the chain
# creates first (`Futamura.joined_scope`, `ordered_binds`).
ActiveRecord::Schema.define do
  create_table(:vets) { |t| t.integer :region; t.boolean :active; t.string :name }
  create_table(:pets) { |t| t.integer :vet_id; t.integer :backup_id; t.integer :tier; t.string :code }
end
$region = 1
$tier = 1
class Vet < ApplicationRecord; default_scope { where(region: [$region, 7], active: true) }; end
class Pet < ApplicationRecord
  default_scope { where(tier: $tier) }
  belongs_to :vet
  belongs_to :backup, class_name: "Vet"
end
Vet.unscoped do
  [[1, true, "a"], [2, true, "b"], [7, true, "c"], [1, false, "d"]].each { |region, active, name| Vet.create!(region: region, active: active, name: name) }
end
Pet.unscoped do
  [[1, 1], [2, 1], [3, 1], [1, 3], [3, 3], [1, 2]].each_with_index do |(vet, backup), i|
    Pet.create!(vet_id: vet, backup_id: backup, tier: i < 5 ? 1 : 2, code: "x")
  end
end
Futamura.define(:pets, Pet, via: :where) { |p| Pet.all.joins(:vet, :backup).where(code: p.bind) }
Futamura.define(:pets_left, Pet, via: :where) { |p| Pet.all.where(code: p.bind).left_joins(:vet).where.not(vet_id: p.bind) }

def pets_look(key, plain)
  before = $fallbacks
  specialized = Futamura.records(key, key == :pets ? ["x"] : ["x", 99]) { $fallbacks += 1; plain.call.to_a }
  [specialized.map(&:id), plain.call.map(&:id), $fallbacks - before]
end

pets = lambda do
  [[1, 1], [2, 1], [7, 1], [1, 2], [2, 2], [1, 1]].map do |region, tier|
    $region = region
    $tier = tier
    pets_look(:pets, -> { Pet.all.joins(:vet, :backup).where(code: "x") })
  end
end
lefts = lambda do
  [[1, 1], [2, 1], [1, 2]].map do |region, tier|
    $region = region
    $tier = tier
    pets_look(:pets_left, -> { Pet.all.where(code: "x").left_joins(:vet).where.not(vet_id: 99) })
  end
end
# Each look is [records of the statement, records of Rails' chain, calls that ran Rails' chain].
{ pets: pets.call, pets_left: lefts.call }.each do |name, looks|
  # `distinct` is how many different answers the calls had: the values are the call's own.
  puts "#{name}=agree #{looks.all? { |specialized, plain, _| specialized == plain }} fallbacks #{looks.sum(&:last)} distinct #{looks.map(&:first).uniq.size}"
end
# A call whose joined default scope has other conditions than the statement's runs Rails' chain.
$region = 1
$tier = 1
before = $fallbacks
inside = Vet.unscoped { Futamura.records(:pets, ["x"]) { $fallbacks += 1; Pet.all.joins(:vet, :backup).where(code: "x").to_a } }
puts "pets_unscoped=#{[inside.map(&:id), $fallbacks - before].inspect}"
puts "pets.ids=#{pets.call.map(&:first).inspect}"
