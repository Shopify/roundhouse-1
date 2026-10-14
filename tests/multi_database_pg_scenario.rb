# One multiple-database scenario, run by tests/multi_database_pg_runtime.rb
# on each side against the same local PostgreSQL databases:
#
#   ruby tests/multi_database_pg_scenario.rb native  APP_ROOT
#   ruby tests/multi_database_pg_scenario.rb emitted OUT_ROOT
#
# native: Core's pinned Rails 8.2 (tests/support/rails_oracle.rb) with the
# app's config/database.yml as Base.configurations (each entry given the
# run's host/port/user from PROBE_PG_*) and the app's own model files.
# emitted: the emitted program's boot.rb and DatabaseConfiguration.configure!,
# the endpoints from ROUNDHOUSE_DATABASE_URL / ROUNDHOUSE_ANIMALS_DATABASE_URL.
#
# Everything after the prelude is the same code on both sides. It prints one
# JSON line per answer: [label, value].
require "json"

side, root = ARGV.fetch(0), ARGV.fetch(1)
ENV["RAILS_ENV"] = "test"
case side
when "native"
  require_relative "support/rails_oracle"
  RailsOracle.activate("activesupport", "activemodel", "activerecord")
  require "active_record"
  require "erb"
  require "yaml"
  raw = YAML.unsafe_load(ERB.new(File.read(File.join(root, "config/database.yml"))).result)
  endpoint = { "host" => ENV["PROBE_PG_HOST"], "port" => ENV["PROBE_PG_PORT"], "username" => ENV["PROBE_PG_USER"] }.compact
  raw.each_value { |configs| configs.each_value { |c| c.merge!(endpoint) if c.is_a?(Hash) } }
  ActiveRecord::Base.configurations = raw
  ActiveRecord::Base.establish_connection
  %w[application_record animals_record post comment dog].each { |m| require File.join(root, "app/models", m) }
when "emitted"
  Dir.chdir(root)
  require File.join(root, "boot")
  DatabaseConfiguration.configure!
else
  abort "side: native or emitted"
end

def out(label, value)
  $stdout.puts JSON.generate([label, value])
  $stdout.flush
end

# The answer, or the error as class and message. A write refused in
# readonly mode quotes the SQL the ORM generated, which differs between the
# two (the emitted SQL is compiled); its verb is kept.
def outcome
  value = yield
  value.is_a?(Array) || value.is_a?(Hash) || value.nil? || value == true || value == false ||
    value.is_a?(Numeric) || value.is_a?(String) || value.is_a?(Symbol) ? value.to_s : value.class.name
rescue Exception => e
  message = e.message.sub(/\A(Write query attempted while in readonly mode: )(\w+).*\z/m, '\1\2 ...')
  "#{e.class}: #{message}"
end

def db_of(klass)
  klass.connection.execute("SELECT current_database() AS db").to_a.first["db"]
end

touched = []
ActiveSupport::Notifications.subscribe("sql.active_record") do |*, payload|
  connection = payload[:connection]
  touched << connection.pool.db_config.name if connection
end

out "specification names", [ActiveRecord::Base, ApplicationRecord, Post, AnimalsRecord, Dog].map(&:connection_specification_name)
out "db configs", [Post, Comment, Dog].map { |k| k.connection_db_config.name }
out "databases", [db_of(ActiveRecord::Base), db_of(Post), db_of(Dog)]

Post.create!(title: "hello")
Dog.create!(name: "rex")
out "counts", [Post.count, Dog.count]
out "raw counts", [Post.connection.execute("SELECT count(*) AS n FROM dogs").to_a.first["n"].to_i,
  Dog.connection.execute("SELECT count(*) AS n FROM posts").to_a.first["n"].to_i]

# An error leaving the block rolls the transaction back and propagates
# (ActiveRecord::Rollback, the silent form, is not in the emitted runtime).
class ProbeAbort < StandardError; end
begin
  Dog.transaction do
    Dog.create!(name: "rolled back")
    raise ProbeAbort
  end
rescue ProbeAbort
end
# The transaction is the primary's; the dog is written on animals' own
# connection, outside it, so the rollback keeps it.
begin
  Post.transaction do
    Post.create!(title: "rolled back")
    Dog.create!(name: "kept")
    raise ProbeAbort
  end
rescue ProbeAbort
end
out "after transactions", [Post.count, Dog.count, Dog.connection.execute("SELECT name FROM dogs ORDER BY id").to_a.map { |r| r["name"] }]

out "reading", ActiveRecord::Base.connected_to(role: :reading) {
  [ActiveRecord::Base.current_role.to_s, Post.connection_db_config.name, Dog.connection_db_config.name,
   Post.count, Dog.count, db_of(Dog), db_of(Post), ActiveRecord::Base.connected_to?(role: :reading)]
}
out "reading writes dog", outcome { ActiveRecord::Base.connected_to(role: :reading) { Dog.create!(name: "no") } }
out "reading writes post", outcome { ActiveRecord::Base.connected_to(role: :reading) { Post.create!(title: "no") } }
out "reading raw insert", outcome { ActiveRecord::Base.connected_to(role: :reading) { Dog.connection.execute("INSERT INTO dogs (name) VALUES ('raw')") } }
out "reading raw select", outcome { ActiveRecord::Base.connected_to(role: :reading) { Dog.connection.execute("SELECT 1 AS one").to_a } }
out "animals reading only", AnimalsRecord.connected_to(role: :reading) {
  [Dog.current_role.to_s, Post.current_role.to_s, Dog.connection_db_config.name, Post.connection_db_config.name]
}
out "preventing writes", outcome { ActiveRecord::Base.while_preventing_writes { Post.create!(title: "no") } }
out "preventing writes flag", ActiveRecord::Base.while_preventing_writes { [Post.current_preventing_writes, Post.count] }
out "preventing writes raw", outcome { ActiveRecord::Base.while_preventing_writes { Dog.connection.execute("DELETE FROM dogs") } }
out "after refusals", [Post.count, Dog.count]

# The insert family is the model's own pool's write, as `create!` is: a
# Dog row lands on animals (not the ambient primary), and the reading
# role refuses it.
Dog.insert!({ name: "ins" })
Dog.insert_all([{ name: "ia1" }, { name: "ia2" }])
Dog.insert({ name: "ins2" })
out "insert family", [Dog.count, db_of(Dog), Dog.connection.execute("SELECT name FROM dogs ORDER BY id").to_a.map { |r| r["name"] }]
out "reading insert!", outcome { ActiveRecord::Base.connected_to(role: :reading) { Dog.insert!({ name: "no" }) } }
out "reading insert_all", outcome { ActiveRecord::Base.connected_to(role: :reading) { Dog.insert_all([{ name: "no" }]) } }
out "after insert family", [Post.count, Dog.count]

out "concrete connected_to", outcome { Dog.connected_to(role: :reading) { 1 } }
out "unknown role", outcome { ActiveRecord::Base.connected_to(role: :nope) { Post.count } }
out "no role or shard", outcome { ActiveRecord::Base.connected_to { 1 } }
out "connected_to many", ActiveRecord::Base.connected_to_many([AnimalsRecord], role: :reading) {
  [Dog.current_role.to_s, Post.current_role.to_s, db_of(Dog)]
}
AnimalsRecord.connecting_to(role: :reading)
out "connecting_to", [Dog.current_role.to_s, Post.current_role.to_s, Dog.connection_db_config.name]
ActiveRecord::Base.connected_to_stack.pop
out "after connecting_to", [Dog.current_role.to_s, Dog.connection_db_config.name]

class ShardRecord < ActiveRecord::Base
  self.abstract_class = true

  connects_to shards: {
    default: { writing: :primary, reading: :primary_replica },
    shard_one: { writing: :animals, reading: :animals_replica },
  }
end

out "shards", [db_of(ShardRecord), ShardRecord.connected_to(shard: :shard_one) { db_of(ShardRecord) },
  ShardRecord.connected_to(shard: :shard_one, role: :reading) { [ShardRecord.current_shard.to_s, ShardRecord.connection_db_config.name] }]
out "shard keys", [ShardRecord.shard_keys.map(&:to_s), ShardRecord.sharded?, Dog.sharded?]
out "all shards", ShardRecord.connected_to_all_shards(role: :reading) { db_of(ShardRecord) }
out "unknown shard", outcome { ShardRecord.connected_to(shard: :nope) { db_of(ShardRecord) } }
out "shard write on reading", outcome {
  ShardRecord.connected_to(shard: :shard_one, role: :reading) { ShardRecord.connection.execute("DELETE FROM dogs") }
}
out "prohibited swap", outcome { ActiveRecord::Base.prohibit_shard_swapping { ShardRecord.connected_to(shard: :shard_one) { 1 } } }
out "prohibited flag", ActiveRecord::Base.prohibit_shard_swapping { ActiveRecord::Base.shard_swapping_prohibited? }

handler = ActiveRecord::Base.connection_handler
out "pool names", handler.connection_pool_names.sort
out "pools", handler.connection_pool_list(:all).map { |p| [p.db_config.name, p.role.to_s, p.shard.to_s] }.sort
out "touched", touched.uniq.sort
