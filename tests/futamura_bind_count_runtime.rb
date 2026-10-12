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
end
class ApplicationRecord < ActiveRecord::Base; self.abstract_class = true; end
# An IN list over an integer column in a default scope, and the same over a string column.
class Rank < ApplicationRecord; default_scope { where(rank: [1, 2]) }; end
class Name < ApplicationRecord; default_scope { where(label: %w[a b]) }; end

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
