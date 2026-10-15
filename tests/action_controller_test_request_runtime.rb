# Native Rails oracle (the Rails native Core runs; tests/support/
# rails_oracle.rb) for ActionDispatch::TestRequest's writers,
# ActionController::TestRequest.create(controller_class) and its
# TestSession, and ActionController::HttpAuthentication::Basic's module
# functions, against the CRuby overlay's runtime/action_dispatch_request.rb
# and runtime/spinel/http_authentication.rb.
#
#   ruby tests/action_controller_test_request_runtime.rb compare
#
# runs this script twice: `native` loads the pinned actionpack's
# action_controller/test_case; `emitted` lays the runtime files out as the
# emitted CRuby tree's runtime/ holds them and loads them in boot order.
# Each prints one line per observation; `compare` requires them equal.
require_relative "support/rails_oracle"

ROOT = File.expand_path("..", __dir__)

if ARGV.first == "compare"
  require "open3"
  outputs = ["native", "emitted"].map do |mode|
    env = mode == "emitted" ? RailsOracle.core_gems_env : {}
    stdout, stderr, status = Open3.capture3(env, RbConfig.ruby, File.expand_path(__FILE__), mode)
    abort "#{mode}: #{stderr}" unless status.success?
    stdout
  end
  native, emitted = outputs.map(&:lines)
  unless outputs.uniq.length == 1
    diff = native.zip(emitted).each_with_index.reject { |(x, y), _| x == y }.first(8)
    abort "ports differ:\n" + diff.map { |(x, y), i| "line #{i + 1}\n  native:  #{x}  emitted: #{y}" }.join("\n")
  end
  puts outputs.first
  puts "observations: #{native.length}"
  puts "Native Rails and emitted TestRequest/HttpAuthentication::Basic agree (#{RailsOracle.label})"
  exit
end

MODE = ARGV.first
abort "usage: #{$PROGRAM_NAME} compare | native | emitted" unless %w[native emitted].include?(MODE)

if MODE == "native"
  RailsOracle.activate("activesupport", "actionpack", "actionview")
  require "active_support"
  require "active_support/core_ext"
  require "action_controller"
  require "action_controller/test_case"
  require "action_dispatch/testing/test_request"
else
  abort "TestRequest defined before the port loads" if defined?(ActionDispatch::TestRequest)
  require "tmpdir"
  require "fileutils"
  require "base64"
  runtime = Dir.mktmpdir("rh_test_request_runtime")
  at_exit { FileUtils.rm_rf(runtime) }
  {
    "runtime/spinel/scaffold/ruby_overlay/runtime/active_support_core_ext.rb" => "active_support_core_ext.rb",
    "runtime/ruby/action_controller/parameter_missing.rb" => "action_controller/parameter_missing.rb",
    "runtime/spinel/action_controller_exceptions.rb" => "action_controller_exceptions.rb",
    "runtime/ruby/action_dispatch/headers.rb" => "action_dispatch/headers.rb",
    "runtime/spinel/http_authentication.rb" => "http_authentication.rb",
    "runtime/ruby/action_controller/cookies.rb" => "action_controller/cookies.rb",
    "runtime/spinel/scaffold/ruby_overlay/runtime/action_dispatch_request.rb" => "action_dispatch_request.rb",
  }.each do |from, to|
    FileUtils.mkdir_p(File.dirname(File.join(runtime, to)))
    FileUtils.cp(File.join(ROOT, from), File.join(runtime, to))
  end
  %w[active_support_core_ext action_controller_exceptions http_authentication action_controller/cookies action_dispatch_request].each do |file|
    require File.join(runtime, file)
  end
end

def show(label, value)
  puts "#{label}: #{value.inspect}"
end

def outcome
  yield.inspect
rescue Exception => e
  "#{e.class}: #{e.message}"
end

class WidgetsController < ActionController::Base; end

# ActionDispatch::TestRequest: Core's Markets::WebPresenceDebugger
# builds its synthetic storefront request this way.
r = ActionDispatch::TestRequest.create
show("ad ancestors", ActionDispatch::TestRequest.superclass)
show("ad default host", r.host)
show("ad default path", r.path)
show("ad default method", r.request_method)
show("ad default remote ip", r.remote_ip)
show("ad default agent", r.user_agent)
show("ad cookie hash", r.get_header("rack.request.cookie_hash").class.name)
show("ad cookies empty", r.cookies.to_h)
r.cookies["shared"] = "CODE10"
show("ad cookies write", r.cookies.to_h)
parsed = ActionDispatch::TestRequest.create("HTTP_COOKIE" => "a=1; b=x%20y,c=3; a=dup; e")
parsed.delete_header("rack.request.cookie_hash")
show("ad cookies parsed", parsed.cookies.to_h)
show("ad cookies memoized", parsed.cookies.equal?(parsed.cookies))
show("ad cookie_jar read", parsed.cookie_jar["a"])
show("ad cookie_jar decoded", parsed.cookie_jar["b"])
show("ad cookie_jar memoized", parsed.cookie_jar.equal?(parsed.cookie_jar))
r.host = "shop.example.com"
r.request_method = "post"
r.path = "/en/collections"
show("ad host=", r.host)
show("ad method=", r.request_method)
show("ad post?", r.post?)
show("ad path=", r.path)
show("ad fullpath", r.fullpath)
show("ad url", r.url)
r.remote_addr = "10.0.0.1"
r.user_agent = "Probe/1"
r.if_none_match = "\"abc\""
r.if_modified_since = "Tue, 01 Jan 2030 00:00:00 GMT"
r.request_uri = "/en/collections?x=1"
r.port = 8080
r.accept = ["text/html", "application/json"]
# Rails memoizes `remote_ip` on first read, so the header is observed.
show("ad remote_addr=", r.get_header("REMOTE_ADDR"))
show("ad remote_addr= fresh", ActionDispatch::TestRequest.create.tap { |q| q.remote_addr = "10.0.0.2" }.remote_ip)
show("ad user_agent=", r.user_agent)
show("ad headers", %w[HTTP_IF_NONE_MATCH HTTP_IF_MODIFIED_SINCE REQUEST_URI SERVER_PORT HTTP_ACCEPT].map { |k| r.get_header(k) })
show("ad headers[]", r.headers["Accept"])
r.headers["X-Ip-Metadata"] = "countryCode:CA"
r.headers["HTTP_X_RAW"] = "raw"
r.headers["Content-Type"] = "text/plain"
show("ad headers[]=", %w[HTTP_X_IP_METADATA HTTP_X_RAW CONTENT_TYPE].map { |k| r.get_header(k) })
show("ad bad method", outcome { r.request_method = "BREW" })
show("ad method after bad", r.request_method)
r.path_parameters = { shop_route_locale: "en" }
show("ad path_parameters", r.path_parameters)
r.action = "show"
show("ad action=", r.path_parameters)
show("ad env override", ActionDispatch::TestRequest.create("HTTP_HOST" => "once.example", "PATH_INFO" => "/x").then { |q| [q.host, q.path] })
show("ad has_header?", [r.has_header?("HTTP_HOST"), r.has_header?("HTTP_NOPE")])
show("ad fetch_header block", r.fetch_header("HTTP_NOPE") { |k| "missing #{k}" })
r.delete_header("HTTP_USER_AGENT")
show("ad delete_header", r.has_header?("HTTP_USER_AGENT"))
r.add_header("HTTP_X_LIST", "a")
r.add_header("HTTP_X_LIST", "b")
show("ad add_header", r.get_header("HTTP_X_LIST"))

# ActionController::TestRequest: Core's checkout PolicyContextBuilder.
c = ActionController::TestRequest.create(WidgetsController)
show("ac superclass", ActionController::TestRequest.superclass)
show("ac controller_class", c.controller_class)
show("ac host", c.host)
show("ac path_info", c.get_header("PATH_INFO"))
# Not `c.path`: with no PATH_INFO Rails answers "" (script_name +
# path_info) where the overlay's Request answers "/", the root its CGI
# dispatcher routes when the server omits PATH_INFO.
show("ac method", c.request_method)
show("ac session class", c.session.class.name)
show("ac session empty", c.session.empty?)
show("ac session_options", c.session_options.reject { |k, _| k == :secure_random })
show("ac session_options secure_random", c.session_options[:secure_random])
show("ac session_options env", c.get_header("rack.session.options").equal?(c.session_options))
show("ac rack.session", c.get_header("rack.session").equal?(c.session))
s = c.session
s[:buyer] = 1
s["cart"] = "c1"
show("session []", [s[:buyer], s["buyer"], s[:cart]])
show("session keys", s.keys)
show("session values", s.values)
show("session to_hash", s.to_hash)
show("session key?", [s.key?(:buyer), s.has_key?("cart"), s.include?(:nope)])
show("session fetch", [s.fetch(:buyer), s.fetch(:nope, 2), outcome { s.fetch(:nope) }])
s[:nested] = { "a" => { "b" => 3 } }
show("session dig", s.dig(:nested, "a", "b"))
show("session delete", s.delete(:cart))
s.update(z: 9)
show("session update", s.to_hash)
show("session exists/enabled/loaded", [s.exists?, s.enabled?, s.loaded?])
show("session id shape", s.id.public_id.match?(/\A\h{32}\z/))
show("session id_was", s.id_was.equal?(s.id))
show("session private_id", ActionController::TestSession.new({}, ActionController::TestSession.new.id.class.new("abc")).id.private_id)
show("session id to_s", s.id.to_s == s.id.public_id)
s.destroy
show("session destroy", s.empty?)
show("session seeded", ActionController::TestSession.new(a: 1, "b" => 2).to_hash)
c.query_string = "q=1"
c.content_type = "application/json"
show("ac query_string=", c.query_string)
show("ac content_type=", c.get_header("CONTENT_TYPE"))
show("ac new_session", ActionController::TestRequest.new_session.class.name)

# HttpAuthentication::Basic: Core's TaxesApi builds its Authorization
# header from encode_credentials at load.
basic = ActionController::HttpAuthentication::Basic
header = basic.encode_credentials("taxes", "s3cr:et")
show("encode", header)
show("encode long", basic.encode_credentials("u" * 40, "p" * 40))
show("encode non-string", basic.encode_credentials(:sym, 12))
auth = ActionDispatch::TestRequest.create("HTTP_AUTHORIZATION" => header)
show("authorization", auth.authorization)
show("has_basic_credentials?", basic.has_basic_credentials?(auth))
show("auth_scheme", basic.auth_scheme(auth))
show("auth_param", basic.auth_param(auth))
show("decode_credentials", basic.decode_credentials(auth))
show("user_name_and_password", basic.user_name_and_password(auth))
show("authenticate", basic.authenticate(auth) { |u, p| "#{u}/#{p}" })
none = ActionDispatch::TestRequest.create
show("none has?", basic.has_basic_credentials?(none))
show("none authenticate", basic.authenticate(none) { |u, p| "#{u}/#{p}" })
show("none scheme", basic.auth_scheme(none))
show("none param", basic.auth_param(none))
show("none decode", basic.decode_credentials(none))
show("none user/pass", basic.user_name_and_password(none))
token = ActionDispatch::TestRequest.create("HTTP_AUTHORIZATION" => "Token abc")
show("token has?", basic.has_basic_credentials?(token))
lower = ActionDispatch::TestRequest.create("HTTP_AUTHORIZATION" => "basic #{Base64.strict_encode64("a:b")}")
show("lowercase scheme", basic.user_name_and_password(lower))
alt = ActionDispatch::TestRequest.create("X_HTTP_AUTHORIZATION" => header)
show("x_http_authorization", [alt.authorization, basic.has_basic_credentials?(alt)])
redirect = ActionDispatch::TestRequest.create("REDIRECT_X_HTTP_AUTHORIZATION" => header)
show("redirect authorization", redirect.authorization)
nocolon = ActionDispatch::TestRequest.create("HTTP_AUTHORIZATION" => "Basic #{Base64.strict_encode64("solo")}")
show("no colon", basic.user_name_and_password(nocolon))
bare = ActionDispatch::TestRequest.create("HTTP_AUTHORIZATION" => "Basic")
show("bare scheme", [basic.has_basic_credentials?(bare), basic.decode_credentials(bare), basic.user_name_and_password(bare)])
empty = ActionDispatch::TestRequest.create("HTTP_AUTHORIZATION" => "")
show("empty header", basic.has_basic_credentials?(empty))
