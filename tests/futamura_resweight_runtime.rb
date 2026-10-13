# Runtime probe for tests/futamura.rs (resweight_runtime): real ActiveRecord on SQLite.
# usage: ruby futamura_resweight_runtime.rb futamura.rb   (FUTAMURA_STATS=<dir> is set by the probe itself)
#
# FUTAMURA_STATS counts the queries Futamura did not serve by the innermost app frame that issued them.
# The app lives in a scratch directory (the working directory, as no Rails.root is set): a gem under vendor/ and
# Rails' own frames are skipped, a chain written over several lines is counted at the line its terminal ran on.
require "tmpdir"
require "fileutils"
require "json"
root = File.realpath(Dir.mktmpdir("resweight"))
stats = File.join(root, "stats")
FileUtils.mkdir_p([stats, File.join(root, "app"), File.join(root, "vendor/gem")])
ENV["FUTAMURA_STATS"] = stats
Dir.chdir(root)

at_exit { FileUtils.rm_rf(root) unless ENV["RESWEIGHT_KEEP"] }
require "active_record"
require "concurrent"
require "set"
ActiveRecord::Base.establish_connection(adapter: "sqlite3", database: ":memory:")
ActiveRecord::Schema.verbose = false
ActiveRecord::Schema.define { create_table(:things) { |t| t.string :name; t.integer :rank } }
class Thing < ActiveRecord::Base; end
load ARGV[0]

File.write("vendor/gem/helper.rb", <<~RUBY)
  module VendoredFinder
    def self.named(name) = Thing.where(name: name).to_a
  end
RUBY
File.write("app/finders.rb", <<~RUBY)
  Futamura.define(:things_named, Thing, via: :where) { |p| Thing.where(name: p.bind) }
  module Finders
    def self.served(name) = Futamura.records(:things_named, [name]) { Thing.where(name: name) }.to_a
    def self.fell_back(name) = Futamura.records(:things_named, [[]]) { Thing.where(name: name) }.to_a
    def self.residue_where(name) = Thing.where(name: name).to_a
    def self.residue_chain(name)
      Thing
        .where(name: name)
        .order(:rank)
        .to_a
    end
    def self.residue_count = Thing.count
    def self.residue_exists = Thing.exists?(name: "a")
    def self.residue_find_by = Thing.find_by(name: "a")
    def self.through_vendor(name) = VendoredFinder.named(name)
  end
RUBY
load "vendor/gem/helper.rb"
load "app/finders.rb"

Thing.create!(name: "a", rank: 1)
Thing.create!(name: "b", rank: 2)

2.times { Finders.served("a") }
Finders.fell_back("a")
3.times { Finders.residue_where("a") }
2.times { Finders.residue_chain("b") }
Finders.residue_count
Finders.residue_exists
Finders.residue_find_by
Finders.through_vendor("a")
# Inside the query cache a repeated statement is answered from it, and is still Rails' chain running.
Thing.cache { 2.times { Finders.residue_where("b") } }

queries = Futamura::Stats.app_queries
puts "served=#{Futamura::Stats.served_queries}"
queries.sort.each { |site, q| puts "#{site} calls=#{q.calls} cached=#{q.cached} names=#{q.names.sort.to_h}" }
Futamura::Stats.dump
dump = JSON.parse(File.read(Dir[File.join(stats, "futamura-stats-*.json")].first))
puts "dump_root=#{dump["app_root"] == root} dump_sites=#{dump["app_queries"].keys.sort == queries.keys.sort} dump_served=#{dump["served_queries"]}"
