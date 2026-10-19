# Native Rails oracle (the Rails native Core runs; tests/support/
# rails_oracle.rb) for ActionDispatch::Session::AbstractStore (and its
# Compatibility, StaleSessionCheck and SessionObject modules,
# AbstractSecureStore and SessionRestoreError), ActionDispatch::Constants
# and ActionDispatch::ShowExceptions against
# runtime/spinel/action_dispatch_session_store.rb and
# runtime/spinel/action_dispatch_show_exceptions.rb.
#
#   ruby tests/action_dispatch_session_store_runtime.rb compare
#
# runs this script twice: `native` requires the pinned actionpack;
# `emitted` loads the shared runtime's ActionDispatch::Session and the
# ports as the emitted PostgreSQL tree's boot.rb does, on Core's gem home
# alone (bin/run-target's GEM_PATH), so both run over rack 2.2.24. Each
# prints one line per observation over Core's uses (MultiSessionStore's
# and ShopifyRedisSessionStore's subclassing and store options,
# SessionUnavailableMiddleware's `super(app, exceptions_app)` and
# `render_exception(request, error)`) and ShopifyMysqlSessionStore's
# ActiveRecordStore superclass (activerecord-session_store); `compare`
# requires them equal.
require_relative "support/rails_oracle"

if ARGV.first == "compare"
  require "open3"
  outputs = ["native", "emitted"].map do |mode|
    env = {}
    if mode == "emitted"
      env = RailsOracle.core_gems_env.merge("GEM_HOME" => RailsOracle::CORE_GEM_HOME, "GEM_PATH" => RailsOracle::CORE_GEM_HOME)
    end
    stdout, stderr, status = Open3.capture3(env, RbConfig.ruby, File.expand_path(__FILE__), mode)
    abort "#{mode}: #{stderr}" unless status.success?
    stdout
  end
  native, emitted = outputs.map(&:lines)
  equal = native.zip(emitted).count { |x, y| x == y }
  unless outputs.uniq.length == 1
    diff = native.zip(emitted).each_with_index.reject { |(x, y), _| x == y }.first(8)
    abort "session stores differ (#{equal}/#{native.length} observations equal, emitted has #{emitted.length}):\n" +
      diff.map { |(x, y), i| "line #{i + 1}\n  native:  #{x}  emitted: #{y}" }.join("\n")
  end
  puts outputs.first
  puts "observations: #{equal}/#{native.length} equal"
  puts "Native Rails and emitted ActionDispatch session stores agree (#{RailsOracle.label})"
  exit
end

MODE = ARGV.first
abort "usage: #{$PROGRAM_NAME} compare | native | emitted" unless %w[native emitted].include?(MODE)

if MODE == "native"
  RailsOracle.activate("actionpack")
  require "action_dispatch"
  require "action_dispatch/middleware/session/abstract_store"
  require "action_dispatch/middleware/show_exceptions"
  # activerecord-session_store (Core's gem home, the version Core locks):
  # its store file alone, as the gem's session_store.rb loads it.
  require Dir[File.join(RailsOracle::CORE_GEM_HOME, "bundler/gems/activerecord-session_store-*/lib/action_dispatch/session/active_record_store.rb")].min
else
  require_relative "../runtime/ruby/action_dispatch/session"
  require_relative "../runtime/spinel/action_dispatch_session_store"
  require_relative "../runtime/spinel/action_dispatch_show_exceptions"
end
require "stringio"

def show(label, value)
  puts "#{label}: #{value.inspect}"
end

def outcome
  yield.inspect
rescue Exception => e
  "#{e.class}: #{e.message.lines.first.to_s.chomp.inspect}"
end

def stderr_first_line
  saved = $stderr
  $stderr = StringIO.new
  value = yield
  [$stderr.string.lines.first.to_s.chomp, value]
ensure
  $stderr = saved
end

show("rack", Rack.release)
# identity_client's SessionClass: `defined?(ActionDispatch::Http::Session)`.
show("Http::Session", [defined?(ActionDispatch::Http::Session), ActionDispatch::Http::Session.is_a?(Class), ActionDispatch::Http::Session.method_defined?(:[])])
store_class = ActionDispatch::Session::AbstractStore
show("AbstractStore ancestors", store_class.ancestors.take_while { |a| a != Object }.map(&:name))
show("AbstractSecureStore ancestors", ActionDispatch::Session::AbstractSecureStore.ancestors.take_while { |a| a != Object }.map(&:name))
show("SessionRestoreError ancestors", ActionDispatch::Session::SessionRestoreError.ancestors.take(3).map(&:name))
[
  ActionDispatch::Session::Compatibility,
  ActionDispatch::Session::StaleSessionCheck,
  ActionDispatch::Session::SessionObject,
  ActionDispatch::Session::AbstractStore,
  ActionDispatch::Session::AbstractSecureStore,
].each do |mod|
  show("#{mod.name} public", mod.public_instance_methods(false).sort)
  show("#{mod.name} private", mod.private_instance_methods(false).sort)
end

# MultiSessionStore / ShopifyRedisSessionStore: subclasses with their own
# `call` and session lookups.
class ProbeStore < ActionDispatch::Session::AbstractStore
  def find_session(req, sid)
    [sid || "fresh", { "sid" => sid }]
  end

  def instance(env)
    Rack::Request.new(env)
  end
end

app = ->(env) { [200, { "Content-Type" => "text/plain" }, ["ok"]] }
options = {}
store = ProbeStore.new(app, options)
show("abstract store DEFAULT_OPTIONS", ActionDispatch::Session::AbstractStore::DEFAULT_OPTIONS)
show("abstract store DEFAULT_OPTIONS is rack's", ActionDispatch::Session::AbstractStore::DEFAULT_OPTIONS == Rack::Session::Abstract::Persisted::DEFAULT_OPTIONS)
show("store key", store.key)
show("store default_options", store.default_options)
show("store same_site", store.instance_variable_get(:@same_site))
show("store cookie_only", store.instance_variable_get(:@cookie_only))
show("caller's options after new", options)
redis = ProbeStore.new(nil, key_prefix: "core:", key: "_secure_internal_session_id", secure: true, fallback_to_readslave: true, expire_after: 3600)
show("redis key", redis.key)
show("redis default_options", redis.default_options)
show("redis same_site", redis.instance_variable_get(:@same_site))
sid = store.generate_sid
show("generate_sid", [sid.class.name, sid.length, sid.encoding.name, sid.match?(/\A\h{32}\z/)])
secure = Class.new(ActionDispatch::Session::AbstractSecureStore).new(app, {})
secure_sid = secure.generate_sid
show("secure generate_sid", [secure_sid.class.name, secure_sid.public_id.length, secure_sid.private_id.length])
show("secure key", secure.key)

env = Rack::MockRequest.env_for("/services/internal?x=1", "HTTP_COOKIE" => "_session_id=abc123")
request = Rack::Request.new(env)
show("load_session", outcome { store.send(:load_session, request) })
show("extract_session_id", outcome { store.send(:extract_session_id, request) })
show("extract_session_id without cookie", outcome { store.send(:extract_session_id, Rack::Request.new(Rack::MockRequest.env_for("/"))) })
show("extract_session_id from params when not cookie only", outcome { ProbeStore.new(app, cookie_only: false).send(:extract_session_id, Rack::Request.new(Rack::MockRequest.env_for("/?_session_id=p1"))) })
show("session_exists?", outcome { store.send(:session_exists?, request) })
# ActionDispatch::Http::Session over a store: `create` merges the request's
# previous session, loads lazily through the store and writes through to
# the request's env.
class MemoryStore < ActionDispatch::Session::AbstractStore
  def find_session(_req, sid)
    sid ||= generate_sid
    [sid, { "stored" => "yes" }]
  end

  def session_exists?(_req) = true
end
memory = MemoryStore.new(app, key: "_probe", secure: true)
session_request = Rack::Request.new(Rack::MockRequest.env_for("/"))
previous = ActionDispatch::Http::Session.create(memory, session_request, { id: "previous-id", secure: true })
previous[:before] = 1
previous[:sym] = 2
created = ActionDispatch::Http::Session.create(memory, session_request, { id: "fixed-id", secure: true })
show("Http::Session.create class", created.class.name)
show("Http::Session.create loaded by the merge", [created.loaded?, created.exists?, created.id_was])
show("Http::Session.find", ActionDispatch::Http::Session.find(session_request).equal?(created))
show("Http::Session.create options", session_request.get_header(Rack::RACK_SESSION_OPTIONS).to_hash)
show("Http::Session []", [created["stored"], created[:before], created["sym"]])
show("Http::Session after write", [created.loaded?, created.to_h, created.keys.sort])
created[:added] = [1]
show("Http::Session write", [created["added"], created.key?(:added), created.fetch(:nope, :dflt), created.dig(:added, 0)])
show("Http::Session delete", [created.delete(:added), created.to_hash.key?("added")])
show("Http::Session id", created.id)
show("Http::Session enabled", created.enabled?)
disabled = ActionDispatch::Http::Session.disabled(Rack::Request.new(Rack::MockRequest.env_for("/")))
show("Http::Session.disabled write", outcome { disabled[:x] = 1 })
show("Http::Session.disabled read", [disabled.enabled?, disabled.exists?, disabled[:x]])
show("Http::Session.delete", ActionDispatch::Http::Session.delete(session_request).equal?(created))
# SessionObject's `commit_session` reaches request forgery protection's
# `commit_csrf_token`, which this tree does not carry; not observed.
show("initialize_sid", outcome { store.send(:initialize_sid) })

show("stale check passes", outcome { store.stale_session_check! { :value } })
show("stale check other ArgumentError", outcome { store.stale_session_check! { raise ArgumentError, "bad" } })
show("stale check missing class", outcome { store.stale_session_check! { raise ArgumentError, "undefined class/module Nope::Missing" } })
restore = begin
  store.stale_session_check! { raise ArgumentError, "undefined class/module Nope::Missing" }
rescue ActionDispatch::Session::SessionRestoreError => e
  e
end
show("restore error message", restore.message)
show("restore error cause", [restore.cause.class.name, restore.cause.message])
show("restore error backtrace copied", restore.backtrace == restore.cause.backtrace)
attempts = 0
show("stale check present class retries", outcome {
  store.stale_session_check! do
    attempts += 1
    raise ArgumentError, "undefined class/module String" if attempts == 1
    [:retried, attempts]
  end
})

# ShowExceptions: SessionUnavailableMiddleware.
show("Constants X_CASCADE", ActionDispatch::Constants::X_CASCADE)
show("Constants VARY", ActionDispatch::Constants::VARY)
show("Constants UNPROCESSABLE_CONTENT", ActionDispatch::Constants::UNPROCESSABLE_CONTENT)
show("ShowExceptions ancestors", ActionDispatch::ShowExceptions.ancestors.take(1).map(&:name))
show("ShowExceptions public", ActionDispatch::ShowExceptions.public_instance_methods(false).sort)
show("ShowExceptions private", ActionDispatch::ShowExceptions.private_instance_methods(false).sort)
show("ShowExceptions initialize arity", ActionDispatch::ShowExceptions.instance_method(:initialize).arity)

class SessionUnavailable < StandardError; end
EXCEPTIONS_APP = ->(env) { [404, { "Content-Type" => "text/html" }, ["not found"]] }
class ProbeMiddleware < ActionDispatch::ShowExceptions
  def initialize(app)
    super(app, EXCEPTIONS_APP)
  end

  def call(env)
    @app.call(env)
  rescue SessionUnavailable => e
    render_exception(Struct.new(:env).new(env), e)
  end
end
middleware = ProbeMiddleware.new(app)
show("middleware ivars", middleware.instance_variables.sort)
show("middleware exceptions_app", middleware.instance_variable_get(:@exceptions_app) == EXCEPTIONS_APP)
show("middleware passes through", middleware.call({}))
show("plain passes through", ActionDispatch::ShowExceptions.new(app, EXCEPTIONS_APP).call({}))
show("wrong arity", outcome { ActionDispatch::ShowExceptions.new(app) })
failing = ProbeMiddleware.new(->(env) { raise SessionUnavailable, "redis down" })
# Core passes the exception where Rails' render_exception takes an
# ExceptionWrapper: the failsafe answers.
show("render_exception with the exception", stderr_first_line { failing.call({}) })

# activerecord-session_store's ActionDispatch::Session::ActiveRecordStore:
# Core's ShopifyMysqlSessionStore subclasses it and registers
# `MultiSessionStore.stores[:services_internal]`. `logger.silence` reads
# `ActiveRecord::Base.logger`, so the probe gives Base a nil logger (the
# store falls back to its NilLogger); the session class is a plain
# duck-typed class of the gem's documented contract (find_by_session_id,
# new(session_id:, data:), data, save, destroy).
module ActiveRecord
  class Base
    def self.logger; nil; end
  end
end

class DuckSession
  ROWS = {}
  attr_accessor :session_id, :data
  def self.find_by_session_id(id); ROWS[id]; end
  def initialize(attrs, options = {}); @session_id = attrs[:session_id]; @data = attrs[:data]; end
  def save; ROWS[session_id] = self; true; end
  def destroy; ROWS.delete(session_id); end
end

ars = ActionDispatch::Session::ActiveRecordStore
show("ActiveRecordStore ancestors", ars.ancestors.take_while { |a| a != Object }.map(&:name))
show("ActiveRecordStore public", ars.public_instance_methods(false).sort)
show("ActiveRecordStore private", ars.private_instance_methods(false).sort)
show("ActiveRecordStore constants", ars.constants(false).sort)
show("ActiveRecordStore SESSION_RECORD_KEY", ars::SESSION_RECORD_KEY)
show("ActiveRecordStore ENV_SESSION_OPTIONS_KEY", ars::ENV_SESSION_OPTIONS_KEY)
show("ActiveRecordStore private_session_id?", [ars.private_session_id?("12::abc"), ars.private_session_id?("abc")])
ars.session_class = DuckSession

class ProbeDbStore < ars
  attr_reader :options

  def initialize(app = nil, options = {})
    @options = options
    @fallback = options[:fallback_to_readslave]
    super
  end
end

store_options = { key_prefix: "core:", key: "_secure_internal_session_id", secure: true, fallback_to_readslave: true }
db = ProbeDbStore.new(nil, store_options)
show("db key", db.key)
show("db default_options", db.default_options)
show("db secure_session_only", db.instance_variable_get(:@secure_session_only))
show("db options after new", db.options)
show("db session_class", [db.session_class, ProbeDbStore.session_class, ars.session_class])
show("db secure_session_only: true", ProbeDbStore.new(nil, secure_session_only: true).instance_variable_get(:@secure_session_only))

env = Rack::MockRequest.env_for("/services/internal")
request = Rack::Request.new(env)
env[Rack::RACK_SESSION_OPTIONS] = {}
sid, data = db.send(:get_session, request, nil)
show("get_session new", [sid.class.name, sid.private_id.length, data, env[ars::SESSION_RECORD_KEY].class.name, DuckSession::ROWS.size])
db.send(:write_session, request, sid, { "a" => 1 }, {})
show("write_session stored", DuckSession::ROWS[sid.private_id]&.data)
sid2, data2 = db.send(:get_session, Rack::Request.new(Rack::MockRequest.env_for("/")), sid)
show("get_session existing", [sid2 == sid, data2])
show("get_session unknown sid", db.send(:get_session, Rack::Request.new(Rack::MockRequest.env_for("/")), Rack::Session::SessionId.new("0" * 32)).last)
show("find_session", db.send(:find_session, Rack::Request.new(Rack::MockRequest.env_for("/").merge(Rack::RACK_SESSION_OPTIONS => {})), sid).last)
legacy = Rack::Session::SessionId.new("f" * 32)
DuckSession::ROWS[legacy.public_id] = DuckSession.new({ session_id: legacy.public_id, data: { "legacy" => true } })
show("fallback to the public id", db.send(:get_session_with_fallback, legacy).data)
show("secure_session_only skips it", ProbeDbStore.new(nil, secure_session_only: true).send(:get_session_with_fallback, legacy))
show("a private id is not looked up publicly", db.send(:get_session_with_fallback, Rack::Session::SessionId.new("1::abc")))
del_env = Rack::MockRequest.env_for("/")
del_env[Rack::RACK_SESSION] = Struct.new(:id).new(sid)
del_request = Rack::Request.new(del_env)
new_sid = db.send(:delete_session, del_request, sid, { renew: true })
show("delete_session renew", [new_sid == sid, DuckSession::ROWS.key?(sid.private_id), DuckSession::ROWS.key?(new_sid.private_id), DuckSession::ROWS[new_sid.private_id]&.data])
show("delete_session drop", db.send(:delete_session, del_request, sid, { drop: true }))
show("logger", db.send(:logger).respond_to?(:silence))
