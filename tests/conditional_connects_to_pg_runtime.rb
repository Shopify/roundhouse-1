# Driver for tests/conditional_connects_to_pg_runtime.rs: runs
# tests/conditional_connects_to_pg_scenario.rb on the emitted program three
# ways against freshly created primary and animals databases on the
# ROUNDHOUSE_PG_TEST_URL server:
#   enabled   the condition holds: Dog runs on the animals database, on the
#             pool the computed HashConfig named
#   disabled  DISABLE_ANIMALS=1: the `connects_to` never runs, Dog is on primary
#   no endpoint  the animals database has no endpoint for the run: Rails'
#             ConnectionNotEstablished, by name, not another database
#
#   ruby tests/conditional_connects_to_pg_runtime.rb OUT_ROOT
require "pg"
require "json"
require "open3"

out_root = ARGV.fetch(0)
admin_options = PG::Connection.conninfo_parse(ENV.fetch("ROUNDHOUSE_PG_TEST_URL"))
  .each_with_object({}) { |o, h| h[o[:keyword].to_sym] = o[:val] if o[:val] }
SUFFIX = "_#{Process.pid}"
DATABASES = { "primary" => "rh_e2e_cond#{SUFFIX}_primary", "animals" => "rh_e2e_cond#{SUFFIX}_animals" }.freeze
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

def conninfo(options, dbname)
  options.merge(dbname: dbname).slice(:host, :port, :user, :dbname).map { |k, v| "#{k}=#{v}" }.join(" ")
end

def run(root, env)
  stdout, stderr, status = Open3.capture3(env, RbConfig.ruby, File.join(__dir__, "conditional_connects_to_pg_scenario.rb"), root,
    chdir: File.expand_path("..", __dir__))
  [stdout.lines.to_h { |l| JSON.parse(l) }, stderr, status]
end

clean = %w[RAILS_ENV RACK_ENV DATABASE_URL ROUNDHOUSE_DATABASE_URL ROUNDHOUSE_ANIMALS_DATABASE_URL DISABLE_ANIMALS EXPECT_CREATE].to_h { |k| [k, nil] }
primary_url = conninfo(admin_options, DATABASES["primary"])
animals_url = conninfo(admin_options, DATABASES["animals"])
failures = []
cache = {
  "cache off" => [false, false, false], "cache on" => [true, true, true], "uncached" => [false, false],
  "cache back on" => true, "cache after" => false, "enabled by pool" => true, "disabled by pool" => false,
}
check = lambda do |side, answers, expected|
  expected.each { |label, want| failures << "#{side} #{label}: #{answers[label].inspect}, want #{want.inspect}" unless answers[label] == want }
end
begin
  recreate(admin_options)
  answers, stderr, status = run(out_root, clean.merge("ROUNDHOUSE_DATABASE_URL" => primary_url, "ROUNDHOUSE_ANIMALS_DATABASE_URL" => animals_url, "EXPECT_CREATE" => "1"))
  failures << "enabled exited #{status.exitstatus}:\n#{stderr}" unless status.success?
  check.("enabled", answers, cache.merge(
    "enabled" => true, "config" => ["animals", "animals-probe"], "database" => DATABASES["animals"],
    "primary" => ["primary", DATABASES["primary"]], "dogs" => 1))
  admin(admin_options.merge(dbname: DATABASES["animals"])) do |conn|
    failures << "dogs row not in animals" unless conn.exec("SELECT name FROM dogs").values == [["rex"]]
  end
  admin(admin_options.merge(dbname: DATABASES["primary"])) do |conn|
    failures << "dogs row in primary" unless conn.exec("SELECT name FROM dogs").values == []
  end

  answers, stderr, status = run(out_root, clean.merge("ROUNDHOUSE_DATABASE_URL" => primary_url, "ROUNDHOUSE_ANIMALS_DATABASE_URL" => animals_url, "DISABLE_ANIMALS" => "1"))
  failures << "disabled exited #{status.exitstatus}:\n#{stderr}" unless status.success?
  check.("disabled", answers, cache.merge(
    "enabled" => false, "config" => ["primary", nil], "database" => DATABASES["primary"], "primary" => ["primary", DATABASES["primary"]]))

  answers, stderr, status = run(out_root, clean.merge("ROUNDHOUSE_DATABASE_URL" => primary_url))
  failures << "no endpoint exited #{status.exitstatus}:\n#{stderr}" unless status.success?
  failures << "no endpoint database: #{answers["database"].inspect}" unless answers["database"].to_s.start_with?("ActiveRecord::ConnectionNotEstablished: database_endpoint")
ensure
  drop(admin_options)
end

if failures.empty?
  puts "conditional connects_to: enabled, disabled and no-endpoint runs agree"
else
  warn failures.join("\n")
  exit 1
end
