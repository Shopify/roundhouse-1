# Driver for the PostgreSQL probe tests (tests/*_pg_runtime.rs that
# compare with native Rails): runs the probe app's `PROBE.lines` on native
# Rails and on the emitted program, each against a freshly created
# DATABASE on the ROUNDHOUSE_PG_TEST_URL server, and prints both answers
# as JSON.
#
#   ruby tests/support/pg_probe_driver.rb APP_ROOT OUT_ROOT DATABASE PROBE LOAD...
#
# native: Core's pinned Rails 8.2 (tests/support/rails_oracle.rb), the
# app files LOAD (relative to APP_ROOT, in order) over `establish_connection` to the
# database. emitted: the emitted tree's main.rb, its database
# configuration pointed at the same database through
# ROUNDHOUSE_DATABASE_URL.
require "pg"
require "json"
require "open3"
require "rbconfig"
require_relative "rails_oracle"

app_root, out_root = File.expand_path(ARGV.fetch(0)), File.expand_path(ARGV.fetch(1))
admin_options = PG::Connection.conninfo_parse(ENV.fetch("ROUNDHOUSE_PG_TEST_URL"))
  .each_with_object({}) { |o, h| h[o[:keyword].to_sym] = o[:val] if o[:val] }
DATABASE = ARGV.fetch(2)
PROBE = ARGV.fetch(3)
DDL = File.read(File.join(app_root, "db/structure.sql")).freeze
LOADS = ARGV.drop(4).freeze

def admin(options)
  conn = PG.connect(**options)
  yield conn
ensure
  conn&.close
end

def recreate(admin_options)
  admin(admin_options) do |conn|
    conn.exec("DROP DATABASE IF EXISTS #{conn.quote_ident(DATABASE)} WITH (FORCE)")
    conn.exec("CREATE DATABASE #{conn.quote_ident(DATABASE)}")
  end
  admin(admin_options.merge(dbname: DATABASE)) { |conn| conn.exec(DDL) }
end

def drop(admin_options)
  admin(admin_options) { |conn| conn.exec("DROP DATABASE IF EXISTS #{conn.quote_ident(DATABASE)} WITH (FORCE)") }
end

clean = %w[RAILS_ENV RACK_ENV DATABASE_URL ROUNDHOUSE_DATABASE_URL].to_h { |k| [k, nil] }
endpoint = { host: admin_options[:host], port: admin_options[:port], username: admin_options[:user], database: DATABASE }
native_script = <<~RUBY
  require #{File.join(__dir__, "rails_oracle").inspect}
  RailsOracle.activate("activesupport", "activemodel", "activerecord")
  require "active_record"
  ActiveRecord::Base.establish_connection(#{{ adapter: "postgresql" }.merge(endpoint.compact).inspect})
  #{LOADS.map { |m| "require #{File.join(app_root, m).inspect}" }.join("\n")}
  puts #{PROBE}.lines
RUBY
emitted_script = <<~RUBY
  require File.expand_path("main", Dir.pwd)
  Main.configure_default_adapter!
  puts #{PROBE}.lines
RUBY
conninfo = endpoint.compact.map { |k, v| "#{k == :username ? "user" : k == :database ? "dbname" : k}=#{v}" }.join(" ")

answers = {}
begin
  recreate(admin_options)
  out, err, status = Open3.capture3(clean.merge(RailsOracle.env), RbConfig.ruby, "-e", native_script, chdir: app_root)
  answers["native"] = { "lines" => out.lines.map(&:chomp), "ok" => status.success?, "stderr" => err }
  recreate(admin_options)
  out, err, status = Open3.capture3(clean.merge("ROUNDHOUSE_DATABASE_URL" => conninfo, "RAILS_ENV" => "test"),
    RbConfig.ruby, "-e", emitted_script, chdir: out_root)
  answers["emitted"] = { "lines" => out.lines.map(&:chomp), "ok" => status.success?, "stderr" => err }
ensure
  drop(admin_options)
end
puts JSON.generate(answers)
