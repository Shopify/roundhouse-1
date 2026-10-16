# The scenario run by tests/conditional_connects_to_pg_runtime.rb on the
# emitted program: `ruby tests/conditional_connects_to_pg_scenario.rb OUT_ROOT`.
# AnimalsRecord connects under `if ENABLED` from a computed HashConfig, so
# what Dog reaches is decided when the class body runs. Prints one JSON line
# per answer: [label, value].
require "json"

root = ARGV.fetch(0)
ENV["RAILS_ENV"] = "test"
Dir.chdir(root)
require File.join(root, "boot")
DatabaseConfiguration.configure!

def out(label, value)
  $stdout.puts JSON.generate([label, value])
  $stdout.flush
end

def outcome
  yield
rescue Exception => e
  "#{e.class}: #{e.message}"
end

def db_of(klass)
  klass.connection.execute("SELECT current_database() AS db").to_a.first["db"]
end

out "enabled", AnimalsRecord::ENABLED
out "config", [Dog.connection_db_config.name, Dog.connection_db_config.configuration_hash[:application_name]]
out "database", outcome { db_of(Dog) }
out "primary", [ActiveRecord::Base.connection_pool.db_config.name.sub(/_readonly\z/, ""), db_of(Post)]

if ENV["EXPECT_CREATE"]
  Dog.create!(name: "rex")
  out "dogs", Dog.count
end

# The query cache predicates (connection_pool, connection, Base, Db).
pool = ActiveRecord::Base.connection_pool
out "cache off", [pool.query_cache_enabled, ActiveRecord::Base.connection.query_cache_enabled, Db.query_cache_enabled?]
ActiveRecord::Base.cache do
  out "cache on", [pool.query_cache_enabled, ActiveRecord::Base.connection.query_cache_enabled, Db.query_cache_enabled?]
  ActiveRecord::Base.uncached { out "uncached", [pool.query_cache_enabled, Db.query_cache_enabled?] }
  out "cache back on", Db.query_cache_enabled?
end
out "cache after", Db.query_cache_enabled?
pool.enable_query_cache!
out "enabled by pool", Db.query_cache_enabled?
pool.send(:query_cache).clear
pool.disable_query_cache!
out "disabled by pool", Db.query_cache_enabled?
