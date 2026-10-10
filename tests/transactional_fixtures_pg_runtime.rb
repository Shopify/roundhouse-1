# Driver for tests/transactional_fixtures_pg_runtime.rs: runs the probe
# app's two authored test files on native Rails and on the emitted
# program, each against freshly created rh_e2e_txf_primary,
# rh_e2e_txf_animals and rh_e2e_txf_extra on the ROUNDHOUSE_PG_TEST_URL
# server, and compares what the tests report, the transaction statements
# each configuration received (sql.active_record events named
# TRANSACTION), and the rows each database holds afterwards.
#
#   ruby tests/transactional_fixtures_pg_runtime.rb APP_ROOT OUT_ROOT
#
# native: Core's pinned Rails 8.2 (tests/support/rails_oracle.rb). The
# test files' `require "test_helper"` loads this driver's native helper
# (tests/support/transactional_fixtures_native_helper.rb, put first on the
# load path): the app's database.yml as Base.configurations, the app's
# models, and ActiveRecord::TestFixtures in ActiveSupport::TestCase, as
# rails/test_help includes it. emitted: the emitted test file, after the
# emitted test/test_helper.rb.
require "pg"
require "open3"
require "rbconfig"
require "tmpdir"
require_relative "support/rails_oracle"

app_root, out_root = File.expand_path(ARGV.fetch(0)), File.expand_path(ARGV.fetch(1))
admin_options = PG::Connection.conninfo_parse(ENV.fetch("ROUNDHOUSE_PG_TEST_URL"))
  .each_with_object({}) { |o, h| h[o[:keyword].to_sym] = o[:val] if o[:val] }
# The server is shared with other lanes' runs: this driver's pid keeps
# their `DROP DATABASE ... WITH (FORCE)` off ours. The app's
# database.yml (written for this run) names the databases, so it is
# pointed at the same names.
SUFFIX = "_#{Process.pid}"
DATABASES = { "primary" => "rh_e2e_txf_primary#{SUFFIX}", "animals" => "rh_e2e_txf_animals#{SUFFIX}", "extra" => "rh_e2e_txf_extra#{SUFFIX}" }.freeze
database_yml = File.join(app_root, "config/database.yml")
File.write(database_yml, File.read(database_yml).gsub(/\b(rh_e2e_txf_(?:primary|animals|extra))\b/) { "#{$1}#{SUFFIX}" })
DDL = [
  'CREATE TABLE "posts" ("id" bigserial PRIMARY KEY, "title" character varying NOT NULL)',
  'CREATE TABLE "comments" ("id" bigserial PRIMARY KEY, "body" text NOT NULL, "post_id" bigint NOT NULL)',
  'CREATE TABLE "dogs" ("id" bigserial PRIMARY KEY, "name" character varying NOT NULL)',
].freeze
TEST_FILES = %w[test/models/transactional_probe_test.rb test/models/non_transactional_probe_test.rb].freeze

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

# The transaction statements, as each side's subscriber prints them.
SUBSCRIBER = <<~'RUBY'
  ActiveSupport::Notifications.subscribe("sql.active_record") do |*, payload|
    next unless payload[:name] == "TRANSACTION"
    $stdout.puts "sql: #{[payload[:connection].pool.db_config.name, payload[:sql]].inspect}"
  end
RUBY

# One test file in its own process: its report lines, and the result line
# each runner prints for a test that did not pass.
def run_file(side, root, file, env)
  script = case side
  when "native"
    "$LOAD_PATH.unshift(#{File.join(__dir__, "support/transactional_fixtures_native").inspect})\n" \
      "require 'test_helper'\n#{SUBSCRIBER}\nActiveSupport::TestCase.i_suck_and_my_tests_are_order_dependent!\n" \
      "load #{File.join(root, file).inspect}\n"
  else
    "Dir.chdir(#{root.inspect})\nrequire #{File.join(root, "test/test_helper").inspect}\n#{SUBSCRIBER}\n" \
      "load #{File.join(root, file).inspect}\n"
  end
  stdout, stderr, status = Open3.capture3(env, RbConfig.ruby, "-e", script, chdir: root)
  [stdout.lines.map(&:chomp), stderr, status]
end

clean = %w[RAILS_ENV RACK_ENV DATABASE_URL ROUNDHOUSE_DATABASE_URL ROUNDHOUSE_ANIMALS_DATABASE_URL
  ROUNDHOUSE_EXTRA_DATABASE_URL ROUNDHOUSE_TEST_RESULTS].to_h { |k| [k, nil] }
results = {}
begin
  recreate(admin_options)
  native_env = clean.merge(RailsOracle.env).merge(
    "PROBE_APP_ROOT" => app_root, "PROBE_PG_HOST" => admin_options[:host], "PROBE_PG_PORT" => admin_options[:port],
    "PROBE_PG_USER" => admin_options[:user],
    # Minitest plugins found on the gem path (minitest-prof needs
    # minitest-reporters, which the pinned gem home lacks) are not part of
    # what is compared.
    "MT_NO_PLUGINS" => "1")
  results["native"] = [TEST_FILES.map { |f| run_file("native", app_root, f, native_env) }, storage(admin_options)]

  recreate(admin_options)
  emitted_env = clean.merge(
    "ROUNDHOUSE_DATABASE_URL" => conninfo(admin_options, DATABASES["primary"]),
    "ROUNDHOUSE_ANIMALS_DATABASE_URL" => conninfo(admin_options, DATABASES["animals"]),
    "ROUNDHOUSE_EXTRA_DATABASE_URL" => conninfo(admin_options, DATABASES["extra"]))
  results["emitted"] = [TEST_FILES.map { |f| run_file("emitted", out_root, f, emitted_env) }, storage(admin_options)]
ensure
  drop(admin_options)
end

failures = []
# The report and statement lines, without what each runner prints around
# them (minitest's progress characters run onto the line that follows).
reports = ->(lines) { lines.filter_map { |l| l[/\A[.EFS]*((?:sql|start|depths|isolation|raised|posts|after(?: super)?|before_setup|extra|non-transactional[\w -]*): .*)\z/, 1] } }
results.each do |side, (files, _storage)|
  files.each_with_index do |(lines, stderr, status), i|
    failures << "#{side} #{TEST_FILES[i]} exited #{status.exitstatus}:\n#{lines.join("\n")}\n#{stderr}" unless status.success?
  end
end
native, emitted = results.fetch("native"), results.fetch("emitted")
TEST_FILES.each_with_index do |file, i|
  n, e = reports.(native[0][i][0]), reports.(emitted[0][i][0])
  next if n == e
  failures << "#{file}:\n  native:\n    #{n.join("\n    ")}\n  emitted:\n    #{e.join("\n    ")}"
end
failures << "storage:\n  native:  #{native[1].inspect}\n  emitted: #{emitted[1].inspect}" unless native[1] == emitted[1]

# What both must show, so two identical wrong answers still fail.
expected_storage = {
  "primary" => { "posts" => ["kept", "kept inner"], "dogs" => [] },
  "animals" => { "posts" => [], "dogs" => ["kept"] },
  "extra" => { "posts" => [], "dogs" => [] },
}
failures << "native storage #{native[1].inspect}" unless native[1] == expected_storage
native_reports = reports.(native[0][0][0])
%w[start:\ [1,\ 1,\ true] depths:\ [2,\ 3,\ 2] isolation:\ [2,\ "invalid\ transaction\ isolation\ level:\ :bogus"] extra:\ [1,\ true] after:\ [1,\ true]].each do |line|
  failures << "native lacks #{line.inspect}" unless native_reports.include?(line)
end
failures << "native posts #{native_reports.grep(/\Aposts/)}" unless native_reports.include?('posts: ["outer", "nested commit"]')

native[0].each_with_index do |(lines, _, _), i|
  puts "#{TEST_FILES[i]} (native):"
  reports.(lines).each { |line| puts "  #{line}" }
end
puts "storage: #{native[1].inspect}"
unless failures.empty?
  warn failures.join("\n")
  exit 1
end
puts "native and emitted agree"
