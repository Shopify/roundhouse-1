# Driver for tests/multi_database_pg_runtime.rs: runs
# tests/multi_database_pg_scenario.rb on native Rails and on the emitted
# program, each against freshly created rh_e2e_mdb_primary and
# rh_e2e_mdb_animals on the ROUNDHOUSE_PG_TEST_URL server, and compares
# their answers and the rows each database holds afterwards.
#
#   ruby tests/multi_database_pg_runtime.rb APP_ROOT OUT_ROOT
require "pg"
require "json"
require "open3"
require_relative "support/rails_oracle"

app_root, out_root = ARGV.fetch(0), ARGV.fetch(1)
admin_options = PG::Connection.conninfo_parse(ENV.fetch("ROUNDHOUSE_PG_TEST_URL"))
  .each_with_object({}) { |o, h| h[o[:keyword].to_sym] = o[:val] if o[:val] }
# The server is shared with other lanes' runs: this driver's pid keeps
# their `DROP DATABASE ... WITH (FORCE)` off ours. The app's
# database.yml (written for this run) names the databases, so it is
# pointed at the same names.
SUFFIX = "_#{Process.pid}"
DATABASES = { "primary" => "rh_e2e_mdb_primary#{SUFFIX}", "animals" => "rh_e2e_mdb_animals#{SUFFIX}" }.freeze
database_yml = File.join(app_root, "config/database.yml")
File.write(database_yml, File.read(database_yml).gsub(/\b(rh_e2e_mdb_(?:primary|animals))\b/) { "#{$1}#{SUFFIX}" })
DDL = [
  'CREATE TABLE "posts" ("id" bigserial PRIMARY KEY, "title" character varying NOT NULL)',
  'CREATE TABLE "comments" ("id" bigserial PRIMARY KEY, "body" text NOT NULL, "post_id" bigint NOT NULL)',
  'CREATE TABLE "dogs" ("id" bigserial PRIMARY KEY, "name" character varying NOT NULL)',
].freeze

def admin(options)
  conn = PG.connect(**options)
  yield conn
ensure
  conn&.close
end

# Both databases, empty, with the app's tables (each database has every
# table, so a write routed to the wrong one lands and is counted).
def recreate(admin_options)
  admin(admin_options) do |conn|
    DATABASES.each_value do |name|
      conn.exec("DROP DATABASE IF EXISTS #{conn.quote_ident(name)} WITH (FORCE)")
      conn.exec("CREATE DATABASE #{conn.quote_ident(name)}")
    end
  end
  DATABASES.each_value do |name|
    admin(admin_options.merge(dbname: name)) { |conn| DDL.each { |sql| conn.exec(sql) } }
  end
end

def drop(admin_options)
  admin(admin_options) do |conn|
    DATABASES.each_value { |name| conn.exec("DROP DATABASE IF EXISTS #{conn.quote_ident(name)} WITH (FORCE)") }
  end
end

def storage(admin_options)
  DATABASES.to_h do |config, name|
    rows = admin(admin_options.merge(dbname: name)) do |conn|
      { "posts" => conn.exec("SELECT title FROM posts ORDER BY id").values.flatten,
        "dogs" => conn.exec("SELECT name FROM dogs ORDER BY id").values.flatten }
    end
    [config, rows]
  end
end

def conninfo(options, dbname)
  options.merge(dbname: dbname).slice(:host, :port, :user, :dbname).map { |k, v| "#{k}=#{v}" }.join(" ")
end

def run_side(side, root, env)
  stdout, stderr, status = Open3.capture3(env, RbConfig.ruby, File.join(__dir__, "multi_database_pg_scenario.rb"), side, root,
    chdir: File.expand_path("..", __dir__))
  answers = stdout.lines.map { |l| JSON.parse(l) }
  [answers, stderr, status]
end

clean = %w[RAILS_ENV RACK_ENV DATABASE_URL ROUNDHOUSE_DATABASE_URL ROUNDHOUSE_ANIMALS_DATABASE_URL].to_h { |k| [k, nil] }
results = {}
begin
  recreate(admin_options)
  native_env = clean.merge(RailsOracle.env).merge(
    "PROBE_PG_HOST" => admin_options[:host], "PROBE_PG_PORT" => admin_options[:port], "PROBE_PG_USER" => admin_options[:user])
  results["native"] = [*run_side("native", app_root, native_env), storage(admin_options)]

  recreate(admin_options)
  emitted_env = clean.merge(
    "ROUNDHOUSE_DATABASE_URL" => conninfo(admin_options, DATABASES["primary"]),
    "ROUNDHOUSE_ANIMALS_DATABASE_URL" => conninfo(admin_options, DATABASES["animals"]))
  results["emitted"] = [*run_side("emitted", out_root, emitted_env), storage(admin_options)]
ensure
  drop(admin_options)
end

failures = []
results.each do |side, (_answers, stderr, status, _storage)|
  failures << "#{side} exited #{status.exitstatus}:\n#{stderr}" unless status.success?
end
native, emitted = results.fetch("native"), results.fetch("emitted")
labels = (native[0].map(&:first) + emitted[0].map(&:first)).uniq
labels.each do |label|
  n = native[0].find { |l, _| l == label }&.last
  e = emitted[0].find { |l, _| l == label }&.last
  next if n == e
  failures << "#{label}:\n  native:  #{n.to_json}\n  emitted: #{e.to_json}"
end
failures << "storage:\n  native:  #{native[3].to_json}\n  emitted: #{emitted[3].to_json}" unless native[3] == emitted[3]

# What both must show, so two identical wrong answers still fail.
expected_storage = {
  "primary" => { "posts" => ["hello"], "dogs" => [] },
  "animals" => { "posts" => [], "dogs" => ["rex", "kept"] },
}
failures << "native storage #{native[3].to_json}" unless native[3] == expected_storage
answers = native[0].to_h
failures << "native databases #{answers["databases"]}" unless answers["databases"] == %w[rh_e2e_mdb_primary rh_e2e_mdb_primary rh_e2e_mdb_animals].map { |n| "#{n}#{SUFFIX}" }
failures << "native reading role #{answers["reading"]}" unless answers.dig("reading", 1) == "primary_replica"

puts "native: #{native[0].size} answers, emitted: #{emitted[0].size} answers"
native[0].each { |label, value| puts "#{label}: #{value.to_json}" }
unless failures.empty?
  warn failures.join("\n")
  exit 1
end
puts "native and emitted agree"
