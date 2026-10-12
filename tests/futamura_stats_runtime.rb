# Runtime probe for tests/futamura.rs (stats_runtime): the FUTAMURA_STATS block of futamura.rb on real
# ActiveRecord (SQLite). usage: ruby futamura_stats_runtime.rb futamura.rb
require "active_record"
require "concurrent"
require "json"
require "set"
require "tmpdir"
dir = Dir.mktmpdir
ENV["FUTAMURA_STATS"] = dir
ActiveRecord::Base.establish_connection(adapter: "sqlite3", database: ":memory:")
ActiveRecord::Schema.verbose = false
ActiveRecord::Schema.define do
  create_table(:things) { |t| t.string :code }
  create_table(:ranked_things) { |t| t.string :code }
  create_table(:scoped_things) { |t| t.string :code }
end
class ApplicationRecord < ActiveRecord::Base; self.abstract_class = true; end
class Thing < ApplicationRecord; end
class RankedThing < ApplicationRecord; default_scope { order(:id) }; end
class ScopedThing < ApplicationRecord; default_scope { where(code: "x") }; end
load ARGV[0]
# The model is named by a String, as the emitted initializer does: only a call that gets as far as the
# model resolves it.
Futamura.define(:thing, "Thing", one: true) { |p| Thing.all.where(code: p.bind).send(:ordered_relation).limit(1) }
Futamura.define(:ranked, "RankedThing", one: true) { |p| RankedThing.all.where(code: p.bind).send(:ordered_relation).limit(1) }
Futamura.define(:scoped, "ScopedThing", one: true) { |p| ScopedThing.all.where(code: p.bind).send(:ordered_relation).limit(1) }
[Thing, RankedThing, ScopedThing].each { |m| m.create!(code: "x") }

Futamura.records(:thing, [nil]) { Thing.where(code: nil).first }
Futamura.records(:thing, [["x"]]) { Thing.where(code: ["x"]).first }
Futamura.records(:thing, ["x"]) { Thing.where(code: "x").first }
Futamura.records(:ranked, ["x"]) { RankedThing.where(code: "x").first }
Futamura.records(:scoped, ["x"]) { ScopedThing.where(code: "x").first }
Futamura::Stats.dump
sites = JSON.parse(File.read(Dir[File.join(dir, "futamura-stats-*.json")].first))["sites"]
%w[thing ranked scoped].each { |key| puts "#{key} specialized=#{sites[key]["specialized"]} fallback=#{sites[key]["fallback"].sort.to_h}" }
