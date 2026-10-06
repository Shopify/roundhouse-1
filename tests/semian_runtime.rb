# semian 0.28.3 oracle for the PostgreSQL tree's semian port
# (runtime/spinel/semian.rb, runtime/spinel/semian_activerecord_postgresql_adapter.rb).
#
#   ruby tests/semian_runtime.rb native core      the gem: require "semian"
#   ruby tests/semian_runtime.rb emitted core     the port
#   ruby tests/semian_runtime.rb native adapter   activerecord 8.1.4's PostgreSQLAdapter
#                                                 + semian/rails + semian/activerecord_postgresql_adapter
#   ruby tests/semian_runtime.rb emitted adapter  the tree's Db + PostgreSQLAdapter + the port
#   ruby tests/semian_runtime.rb compare core|adapter
#
# Each mode prints one observation per line; `compare` requires native and
# emitted equal, the core part once per SEMIAN_* environment. The adapter
# part needs ROUNDHOUSE_PG_TEST_URL (a local server); it creates and drops
# its own database, and its connection failures go to a closed port on
# 127.0.0.1. Every process uses its own Semian.namespace, so its SysV
# semaphores are its own, and destroys its resources before it exits.
require "socket"

MODE, PART = ARGV
CORE_ENVIRONMENTS = [
  {},
  { "SEMIAN_DISABLED" => "1" },
  { "SEMIAN_SEMAPHORES_DISABLED" => "1" },
  { "SEMIAN_BULKHEAD_DISABLED" => "1" },
  { "SEMIAN_CIRCUIT_BREAKER_DISABLED" => "1" },
].freeze

if MODE == "compare"
  require "open3"
  environments = PART == "core" ? CORE_ENVIRONMENTS : [{}]
  lines = 0
  environments.each do |env|
    outputs = ["native", "emitted"].map do |mode|
      stdout, stderr, status = Open3.capture3(env, RbConfig.ruby, __FILE__, mode, PART)
      abort "#{mode} #{PART} #{env.keys.join(",")}: #{stderr}#{stdout}" unless status.success?
      stdout
    end
    unless outputs.uniq.length == 1
      a, b = outputs.map(&:lines)
      diff = Array.new([a.size, b.size].max) { |i| [[a[i], b[i]], i] }.reject { |(x, y), _| x == y }.first(6)
      abort "semian #{PART} differs (#{env.keys.join(",")}):\n" +
        diff.map { |(x, y), i| "line #{i + 1}\n  native:  #{x}  emitted: #{y}" }.join("\n")
    end
    puts "## env #{env.keys.join(",")}" unless env.empty?
    puts outputs.first
    lines += outputs.first.lines.count
  end
  if PART == "core"
    # The port refuses the circuit breakers it does not model.
    stdout, stderr, status = Open3.capture3(RbConfig.ruby, __FILE__, "emitted", "unsupported")
    abort "emitted unsupported: #{stderr}" unless status.success?
    puts stdout
  end
  puts "Native semian 0.28.3 and the emitted semian port agree on #{PART} (#{lines} observations)"
  exit
end

NS = "rhsemian#{Process.pid}_#{rand(1 << 30)}_"
LOG = []
EVENTS = []

class CaptureLogger
  def info(message)
    LOG << "info #{message}"
  end

  def warn(message)
    LOG << "warn #{message}"
  end
end

def install_observers
  Semian.namespace = NS
  Semian.logger = CaptureLogger.new
  Semian.subscribe(:rh_observer) do |event, resource, scope, adapter, payload|
    payload = case payload
    when Numeric then "wait"
    when nil then nil
    else payload.inspect
    end
    EVENTS << "event #{event} #{resource.class.name}(#{resource.name.inspect}) scope=#{scope.inspect} adapter=#{adapter.inspect} payload=#{payload.inspect}"
  end
end

NORMALIZERS = []

def norm(text)
  text = text.to_s.gsub(NS, "NS").gsub(/error_last_at="[^"]*"/, 'error_last_at="T"')
  NORMALIZERS.each { |from, to| text = text.gsub(from, to) }
  text
end

def out(line)
  puts norm(line)
end

# Prints and clears what the subscriber and the logger saw. Establishing a
# session runs Rails' and the runtime's own configuration statements, whose
# number differs (Rails sets client_min_messages, intervalstyle, ...), so
# those flushes leave out the :query successes.
def flush(label, skip_query_success: false)
  EVENTS.reject! { |line| line.start_with?("event success ") && line.include?("scope=:query") } if skip_query_success
  EVENTS.each { |line| out("#{label}: #{line}") }
  LOG.each { |line| out("#{label}: log #{line}") }
  EVENTS.clear
  LOG.clear
end

def show(label)
  value = yield
  out("#{label} => #{value.inspect}")
  value
rescue Exception => error
  raise if error.is_a?(SystemExit) || error.is_a?(Interrupt)
  out("#{label} !! #{error.class}: #{error.message}")
  cause = error.cause
  out("#{label} !! cause #{cause.class}: #{cause.message}") if cause
  nil
end

class Boom < StandardError
end

class QuietBoom < Boom
  def marks_semian_circuits?
    false
  end
end

class TimeoutRecorder
  attr_reader :calls

  def initialize
    @calls = []
  end

  def with_resource_timeout(timeout)
    @calls << timeout
    yield
  end
end

def run_core
  install_observers
  out "errors " + [Semian::BaseError, Semian::SyscallError, Semian::TimeoutError, Semian::InternalError,
    Semian::OpenCircuitError, Semian::SemaphoreMissingError].map { |k| "#{k.name}<#{k.superclass.name}" }.join(" ")
  out "flags semaphores_enabled=#{!!Semian.semaphores_enabled?} disabled=#{Semian.disabled?} max_tickets=#{Semian::MAX_TICKETS}"
  out "defaults lru=#{Semian.maximum_lru_size}/#{Semian.minimum_lru_time} permissions=#{Semian.default_permissions.to_s(8)} thread_safe=#{Semian.thread_safe?}"

  options = { tickets: 2, timeout: 0, success_threshold: 2, error_threshold: 3, error_timeout: 0.25,
              half_open_resource_timeout: 1, exceptions: [Boom] }
  resource = show("register") { Semian.register(:rh_core, **options).class.name } && Semian[:rh_core]
  show("parts") { [resource.class.name, resource.bulkhead.class.name, resource.circuit_breaker.class.name] }
  show("tickets") { [resource.tickets, resource.count, resource.registered_workers] }
  show("same resource") { Semian.retrieve_or_register(:rh_core, **options).equal?(resource) }
  show("acquire") { resource.acquire(scope: :s, adapter: :a) { |held| [held.equal?(resource), resource.count] } }
  flush("acquire")
  show("third ticket") { resource.acquire { resource.acquire { resource.acquire { :third } } } }
  show("busy marks") { [resource.closed?, resource.open?, resource.half_open?] }
  show("reset") { resource.reset }
  flush("bulkhead")

  3.times { |i| show("boom #{i}") { resource.acquire { raise Boom, "boom #{i}" } } }
  show("opened") { [resource.closed?, resource.open?, resource.half_open?, resource.request_allowed?] }
  show("last error") { resource.circuit_breaker&.last_error&.message }
  show("open acquire") { resource.acquire { :ran } }
  flush("open")
  sleep 0.35
  recorder = TimeoutRecorder.new
  show("half-open 1") { resource.acquire(resource: recorder) { [resource.half_open?, :ok] } }
  show("half-open 2") { resource.acquire(resource: recorder) { [resource.half_open?, :ok] } }
  show("closed again") { [resource.closed?, recorder.calls] }
  show("closed acquire") { resource.acquire(resource: recorder) { :ok } }
  show("timeouts") { recorder.calls }
  flush("half-open")

  4.times { |i| show("plain #{i}") { resource.acquire { raise StandardError, "plain #{i}" } } }
  4.times { |i| show("quiet #{i}") { resource.acquire { raise QuietBoom, "quiet #{i}" } } }
  show("unmarked") { [resource.closed?, resource.open?] }
  flush("unmarked")

  3.times { |i| show("reopen #{i}") { resource.acquire { raise Boom, "again #{i}" } } }
  sleep 0.35
  show("half-open failure") { resource.acquire { raise Boom, "still down" } }
  show("reopened") { [resource.closed?, resource.open?, resource.half_open?] }
  show("reset") { resource.reset }
  flush("half-open failure")

  show("destroy") { Semian.destroy(:rh_core) && Semian[:rh_core].inspect }
  show("both disabled") do
    both = Semian.register(:rh_none, circuit_breaker: false, bulkhead: false)
    [both.class.name, both.bulkhead.inspect, both.circuit_breaker.inspect, both.acquire { :ok }]
  end
  show("missing thresholds") { Semian.register(:rh_lax, tickets: 1).class.name }
  show("duplicate") { Semian.register(:rh_lax, tickets: 1).class.name }
  show("forced error_timeout") do
    Semian.register(:rh_forced, tickets: 1, success_threshold: 1, error_threshold: 1, error_timeout: 0, force_config_validation: true).class.name
  end
  show("forced tickets") do
    Semian.register(:rh_forced2, tickets: Semian::MAX_TICKETS, success_threshold: 1, error_threshold: 1,
      error_timeout: 1, force_config_validation: true).class.name
  end
  show("forced both") do
    Semian.register(:rh_forced3, tickets: 1, quota: 0.5, success_threshold: 1, error_threshold: 1,
      error_timeout: 1, force_config_validation: true).class.name
  end
  show("thread without bulkheads") do
    Semian.disable_bulkheads_for_thread(Thread.current) do
      Semian.register(:rh_nobulk, tickets: 1, success_threshold: 1, error_threshold: 1, error_timeout: 1).bulkhead.inspect
    end
  end
  if ENV.key?("SEMIAN_CIRCUIT_BREAKER_DISABLED")
    show("adaptive when disabled") do
      Semian.register(:rh_adaptive, tickets: 1, adaptive_circuit_breaker: true).circuit_breaker.inspect
    end
    show("dual when disabled") do
      Semian.register(:rh_dual, tickets: 1, dual_circuit_breaker: true).circuit_breaker.inspect
    end
  end
  flush("validation")

  error_class = Class.new(StandardError) { include Semian::AdapterError }
  show("adapter error") do
    plain = error_class.new("boom")
    tagged = error_class.new("boom")
    tagged.semian_identifier = :rh_id
    already = error_class.new("[rh_id] boom")
    already.semian_identifier = :rh_id
    [plain.to_s, tagged.to_s, tagged.message, already.message]
  end
  show("resources") { Semian.resources.keys.map(&:to_s).sort }
  Semian.destroy_all_resources
  show("destroyed") { Semian.resources.size }
  flush("end")
end

def run_unsupported
  install_observers
  [:adaptive_circuit_breaker, :dual_circuit_breaker].each do |kind|
    show(kind) { Semian.register(:"rh_#{kind}", tickets: 1, kind => true) }
    show("#{kind} registered") { Semian[:"rh_#{kind}"].inspect }
  end
  Semian.destroy_all_resources
end

# --- the adapter part -------------------------------------------------------

def admin_options
  require "pg"
  url = ENV.fetch("ROUNDHOUSE_PG_TEST_URL")
  admin = PG.connect(url)
  [admin, admin.conninfo_hash.slice(:host, :port, :user, :password).compact]
end

def closed_port
  server = TCPServer.new("127.0.0.1", 0)
  port = server.addr[1]
  server.close
  port
end

# The two sides build adapters their own way and run statements through
# their own entry points; everything they print is the same.
module NativeSide
  ADAPTERS = {}

  def self.load
    gem "activerecord", "8.1.4"
    gem "semian", "0.28.3"
    require "active_record"
    require "active_record/connection_adapters/postgresql_adapter"
    require "semian"
    require "semian/rails"
    require "semian/activerecord_postgresql_adapter"
  end

  def self.config(endpoint, database, semian)
    { adapter: "postgresql", host: endpoint[:host], port: endpoint[:port], username: endpoint[:user],
      password: endpoint[:password], database: database, semian: semian }.compact
  end

  def self.establish(key, endpoint, database, semian, pool: 1, string_keys: false)
    ADAPTERS[key] = Array.new(pool) do
      ActiveRecord::ConnectionAdapters::PostgreSQLAdapter.new(config(endpoint, database, semian)).connect!
    end
    nil
  end

  # A whole resolved database.yml entry, pooled as Rails' HashConfig sizes it.
  def self.establish_entry(key, entry)
    size = ActiveRecord::DatabaseConfigurations::HashConfig.new("test", key, entry).max_connections
    ADAPTERS[key] = Array.new(size) { ActiveRecord::ConnectionAdapters::PostgreSQLAdapter.new(entry).connect! }
    nil
  end

  def self.pool_size(key)
    ADAPTERS.fetch(key).size
  end

  def self.adapter(key)
    ADAPTERS.fetch(key).first
  end

  def self.run(key, sql)
    adapter(key).execute(sql)
    :ok
  end

  def self.close
    ADAPTERS.each_value { |adapters| adapters.each(&:disconnect!) }
  end
end

module EmittedSide
  def self.load
    require "pg"
    root = File.expand_path("..", __dir__)
    require File.join(root, "runtime/ruby/active_record/errors")
    require File.join(root, "runtime/ruby/active_record/connection_not_established")
    require File.join(root, "runtime/ruby/active_record/connection")
    require File.join(root, "runtime/spinel/db_pg_cruby")
    require File.join(root, "runtime/spinel/scaffold/ruby_overlay/runtime/active_record_postgresql_adapter")
    require File.join(root, "runtime/spinel/semian_activerecord_postgresql_adapter")
  end

  # The resolved database.yml entry, as boot passes it; `string_keys`
  # gives it as YAML spells it before Rails symbolizes it.
  def self.establish(key, endpoint, database, semian, pool: 1, string_keys: false)
    config = NativeSide.config(endpoint, database, semian)
    config = config.transform_keys(&:to_s) if string_keys
    Db.configure_database(key, config, pool_size: pool)
  end

  def self.establish_entry(key, entry)
    Db.configure_database(key, entry)
  end

  def self.pool_size(key)
    Db.instance_variable_get(:@databases).fetch(key).size
  end

  def self.adapter(key)
    Db.with_database(key) { ActiveRecord::Base.connection }
  end

  def self.run(key, sql)
    Db.with_database(key) { Db.exec(sql) }
    :ok
  end

  def self.close
    Db.close
  end
end

def run_adapter(side)
  admin, endpoint = admin_options
  database = "roundhouse_semian_#{Process.pid}"
  missing = "roundhouse_semian_missing_#{Process.pid}"
  admin.exec("CREATE DATABASE #{admin.quote_ident(database)}").clear
  down = closed_port
  NORMALIZERS << [missing, "MISSING_DB"] << [database, "DB"] << [/port #{down}\b/, "port DOWN"]
  NORMALIZERS << [endpoint[:host], "HOST"] if endpoint[:host]
  side.load
  install_observers
  adapter_class = ActiveRecord::ConnectionAdapters::PostgreSQLAdapter
  begin
    out "prepended #{adapter_class.ancestors.first(2).map(&:name).join(" < ")}"
    out "adapter error on PG::ConnectionBad=#{PG::ConnectionBad.include?(Semian::AdapterError)} " \
      "StatementInvalid=#{ActiveRecord::StatementInvalid.include?(Semian::AdapterError)} " \
      "ConnectionNotEstablished=#{ActiveRecord::ConnectionNotEstablished.include?(Semian::AdapterError)}"
    out "semian errors " + [adapter_class::SemianError, adapter_class::ResourceBusyError, adapter_class::CircuitOpenError]
      .map { |k| "#{k.name}<#{k.superclass.name}" }.join(" ")
    out "resource exceptions are ConnectionNotEstablished: #{adapter_class::SemianError < ActiveRecord::ConnectionNotEstablished}"
    ["COMMIT", "ROLLBACK", "commit", "/* app:core */ COMMIT", "RELEASE SAVEPOINT active_record_1",
     "RELEASE SAVEPOINT active_record_3", "ROLLBACK TO SAVEPOINT active_record_2", "COMMIT;", "BEGIN",
     "SELECT 1", "SELECT 'COMMIT'", "  ROLLBACK "].each do |sql|
      out "allowlisted #{sql.inspect} => #{adapter_class.query_allowlisted?(sql)}"
    end

    healthy = { name: "rhgood", tickets: 1, timeout: 0, success_threshold: 2, error_threshold: 3,
                error_timeout: 0.5, half_open_resource_timeout: 1 }
    show("establish good") { side.establish("good", endpoint, database, healthy) }
    flush("establish good", skip_query_success: true)
    good = side.adapter("good")
    show("identity") { [good.class.name, good.semian_identifier, good.semian_resource.name, good.respond_to?(:semian_identifier)] }
    show("resource parts") { [good.semian_resource.class.name, good.semian_resource.bulkhead.class.name, good.semian_resource.circuit_breaker.class.name] }
    show("select") { side.run("good", "SELECT 1") }
    show("ping") { good.active? }
    flush("statements")
    ["BEGIN", "SAVEPOINT active_record_1", "RELEASE SAVEPOINT active_record_1", "COMMIT",
     "BEGIN", "/* app:core */ ROLLBACK"].each { |sql| show(sql) { side.run("good", sql) } }
    flush("transaction statements")
    show("bad sql") { side.run("good", "SELECT * FROM no_such_table") }
    show("after bad sql") { [side.run("good", "SELECT 2"), good.semian_resource.closed?] }
    flush("bad sql")
    # A session the client itself finished: Rails' retryable_query_error?
    # reads its transaction status and raises PG::ConnectionBad again, so
    # the session stays verified and is never reconnected implicitly.
    # The session is reached through the adapter's ivar: Rails' public
    # `raw_connection` marks the adapter dirty, which disables reconnects
    # (not modeled by the runtime, see BOOT_BOUNDARIES.md).
    session = -> { good.instance_variable_get(:@raw_connection) }
    show("finished session") { session.call.finish }
    show("query on finished session") { side.run("good", "SELECT 3") }
    show("circuit after finish") { [good.semian_resource.closed?, good.semian_resource.circuit_breaker.last_error&.class&.name] }
    show("finished session again") { side.run("good", "SELECT 3") }
    show("explicit reconnect") { good.reconnect!; good.active? }
    flush("finished session", skip_query_success: true)
    sleep 0.6 # the two errors leave the breaker's error_timeout window
    # A session the server terminated: the statement's failure marks the
    # session unverified, and the next statement reconnects.
    show("terminate backend") { admin.exec("SELECT pg_terminate_backend($1)", [session.call.backend_pid]).getvalue(0, 0) }
    sleep 0.2
    show("query on terminated session") { side.run("good", "SELECT 4") }
    show("circuit after termination") { [good.semian_resource.closed?, good.semian_resource.circuit_breaker.last_error&.class&.name] }
    show("reconnects inside the statement") { side.run("good", "SELECT 4") }
    flush("terminated session", skip_query_success: true)
    sleep 0.6
    show("held ticket") do
      good.semian_resource.acquire(scope: :test, adapter: :test) do
        [show("busy statement") { side.run("good", "SELECT 5") }, show("busy ping") { good.active? }]
      end
    end
    show("busy marked") { [good.semian_resource.closed?, good.semian_resource.circuit_breaker.last_error&.class&.name] }
    show("reentrant") do
      good.send(:acquire_semian_resource, scope: :query, adapter: :postgresql_adapter) do
        [side.run("good", "SELECT 6"), good.active?]
      end
    end
    flush("bulkhead")

    unnamed = { tickets: 2, timeout: 0, success_threshold: 1, error_threshold: 3, error_timeout: 1 }
    show("establish unnamed") { side.establish("unnamed", endpoint, database, unnamed.transform_keys(&:to_s), string_keys: true) }
    show("unnamed identity") { side.adapter("unnamed").semian_identifier }
    show("establish unprotected") { side.establish("unprotected", endpoint, database, false) }
    show("unprotected") { [side.adapter("unprotected").semian_identifier, side.adapter("unprotected").semian_resource.class.name] }
    show("establish unconfigured") { side.establish("unconfigured", endpoint, database, nil) }
    show("unconfigured") { [side.adapter("unconfigured").semian_identifier, side.adapter("unconfigured").semian_resource.class.name] }
    flush("identities", skip_query_success: true)

    show("missing database") { side.establish("missing", endpoint, missing, { name: "rhmissing", tickets: 1, success_threshold: 1, error_threshold: 1, error_timeout: 1 }) }
    show("missing again") { side.establish("missing", endpoint, missing, { name: "rhmissing", tickets: 1, success_threshold: 1, error_threshold: 1, error_timeout: 1 }) }
    show("missing circuit") { Semian[:postgresql_rhmissing].closed? }
    flush("missing database")

    unreachable = { host: "127.0.0.1", port: down, user: endpoint[:user] }
    show("hostname error") { side.establish("host", unreachable, "roundhouse_semian_absent", { name: "rhhost", tickets: 1, success_threshold: 1, error_threshold: 5, error_timeout: 1 }) }
    flush("hostname error")

    breaker = { name: "rhdown", tickets: 1, timeout: 0, success_threshold: 2, error_threshold: 3,
                error_timeout: 0.5, half_open_resource_timeout: 1 }
    show("down 1") { side.establish("down1", unreachable, "postgres", breaker) }
    show("down 1 circuit") { [Semian[:postgresql_rhdown].closed?, Semian[:postgresql_rhdown].open?] }
    flush("down 1")
    show("down 2") { side.establish("down2", unreachable, "postgres", breaker) }
    show("down 2 circuit") { [Semian[:postgresql_rhdown].closed?, Semian[:postgresql_rhdown].open?] }
    flush("down 2")
    show("down 3") { side.establish("down3", unreachable, "postgres", breaker) }
    flush("down 3")
    sleep 0.6
    show("half-open down") { side.establish("down4", unreachable, "postgres", breaker) }
    show("half-open down circuit") { [Semian[:postgresql_rhdown].half_open?, Semian[:postgresql_rhdown].open?] }
    flush("half-open down")
    sleep 0.6
    show("recover") { side.establish("up", endpoint, database, breaker) }
    show("recovered circuit") { [Semian[:postgresql_rhdown].closed?, Semian[:postgresql_rhdown].open?] }
    flush("recover", skip_query_success: true)
    show("recovered select") { side.run("up", "SELECT 7") }
    flush("recovered")

    # Rails' configure_connection session settings and HashConfig pool
    # size, from whole database.yml entries, read back from the session.
    settings = %w[client_encoding client_min_messages search_path standard_conforming_strings
                  IntervalStyle TimeZone statement_timeout lock_timeout application_name work_mem]
    base = NativeSide.config(endpoint, database, false)
    entries = {
      "configured" => base.merge(pool: 2, encoding: "LATIN1", min_messages: "notice",
        schema_search_path: "public, pg_catalog",
        variables: { statement_timeout: 5000, "lock_timeout" => "2s", application_name: "rh'semian",
                     work_mem: :default, idle_in_transaction_session_timeout: nil }),
      "zoned" => base.merge(max_connections: 3, variables: { timezone: "America/New_York", statement_timeout: ":default" }),
      "defaults" => base,
    }
    entries.each do |key, entry|
      show("entry #{key}") { side.establish_entry(key, entry) }
      show("entry #{key} pool") { side.pool_size(key) }
      session = side.adapter(key).instance_variable_get(:@raw_connection)
      settings.each { |name| show("entry #{key} #{name}") { session.exec("SHOW #{name}").getvalue(0, 0) } }
      show("entry #{key} interval") { session.exec("SELECT (interval '1 day 2 hours')::text").getvalue(0, 0) }
    end
    show("entry ambiguous") { side.establish_entry("ambiguous", base.merge(pool: 2, max_connections: 3)) }
    show("entry pool with min") { side.establish_entry("minimum", base.merge(pool: 2, min_connections: 1)) }
    flush("entries")
  ensure
    side.close
    Semian.destroy_all_resources
    admin.exec("DROP DATABASE IF EXISTS #{admin.quote_ident(database)}").clear
    admin.finish
  end
end

case [MODE, PART]
when ["native", "core"]
  gem "semian", "0.28.3"
  require "semian"
  run_core
when ["emitted", "core"]
  require_relative "../runtime/spinel/semian"
  run_core
when ["emitted", "unsupported"]
  require_relative "../runtime/spinel/semian"
  run_unsupported
when ["native", "adapter"]
  run_adapter(NativeSide)
when ["emitted", "adapter"]
  run_adapter(EmittedSide)
else
  abort "usage: ruby tests/semian_runtime.rb native|emitted|compare core|adapter"
end
