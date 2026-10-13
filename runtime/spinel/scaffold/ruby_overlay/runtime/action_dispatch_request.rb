# CRuby-only ActionDispatch::Request over the CGI env.
#
# The request-object surface controllers and filters reach
# (`request.remote_ip`, `request.referer`, `request.xhr?`,
# `request.env[...]=`, `request[:format]`). Built straight from the
# CGI/1.1 env hash the dispatcher already receives — no Rack. Lives on
# the CRuby overlay next to CookieJar: 8 targets don't exercise a
# request object yet, and per the CRuby-first strategy each target
# grows its own when its lobsters turn comes.
#
# `[]` delegates to the request params (Rails: `request[:format]` ==
# `params[:format]`), so the dispatcher hands the merged params in
# alongside the env. `env` is a plain mutable Hash copy — callers
# write scratch keys into it (`exception_notifier.exception_data`),
# which the real ENV object would reject for non-String values.
require "stringio"
require "securerandom"
require "digest"
require_relative "action_dispatch/headers"

module ActionDispatch
  class Request
    attr_reader :env
    attr_accessor :params
    # The body's params alone - see the twin in
    # runtime/ruby/action_dispatch/request.rb.
    attr_accessor :request_parameters

    def initialize(env, params = {})
      @env = env
      @params = params
      @request_parameters = {}
      @session_options = {}
    end

    # Rack's per-request session options — see the twin in
    # runtime/ruby/action_dispatch/request.rb. `:skip` is the one key
    # read: the dispatcher writes no session cookie when it is set.
    attr_reader :session_options

    def session_skip?
      @session_options[:skip] == true
    end

    # Rails' `session_options=` stores them in the env as well, where
    # Rack's session middleware reads them.
    def session_options=(options)
      @session_options = options
      set_header("rack.session.options", options)
    end

    # Rack's session: the env's `rack.session`, an empty Hash until one
    # is set (`Rack::Request::Helpers#session`).
    def session
      fetch_header("rack.session") { |k| set_header(k, {}) }
    end

    def session=(session)
      set_header("rack.session", session)
    end

    # Rack::Request::Env, which Rails' Request includes: the env read
    # and written by key.
    def get_header(name) = @env[name]

    def set_header(name, value)
      @env[name] = value
    end

    def has_header?(name) = @env.key?(name)

    def fetch_header(name, &block) = @env.fetch(name, &block)

    def delete_header(name) = @env.delete(name)

    def add_header(key, value)
      if value.nil?
        get_header(key)
      elsif has_header?(key)
        set_header(key, "#{get_header(key)},#{value}")
      else
        set_header(key, value)
      end
    end

    # Rails' `request.authorization`: the header, or one of the names a
    # proxy or rewrite moves it to.
    def authorization
      get_header("HTTP_AUTHORIZATION") ||
        get_header("X-HTTP_AUTHORIZATION") ||
        get_header("X_HTTP_AUTHORIZATION") ||
        get_header("REDIRECT_X_HTTP_AUTHORIZATION")
    end

    # Rails' path parameters (the route's captures), kept in the env
    # under its key; writing them drops the merged-parameters cache. Keys
    # are symbolized, as the router's captures are, so a string-keyed
    # write still reads back by `path_parameters[:name]`.
    PARAMETERS_KEY = "action_dispatch.request.path_parameters"

    def path_parameters
      get_header(PARAMETERS_KEY) || set_header(PARAMETERS_KEY, {})
    end

    def path_parameters=(parameters)
      delete_header("action_dispatch.request.parameters")
      set_header(PARAMETERS_KEY, parameters.transform_keys(&:to_sym))
    end

    # Rails' verb table (ActionDispatch::Request::HTTP_METHODS), which
    # `request_method=` checks a name against.
    HTTP_METHODS = %w[
      OPTIONS GET HEAD POST PUT DELETE TRACE CONNECT
      PROPFIND PROPPATCH MKCOL COPY MOVE LOCK UNLOCK
      VERSION-CONTROL REPORT CHECKOUT CHECKIN UNCHECKOUT MKWORKSPACE UPDATE LABEL MERGE BASELINE-CONTROL MKACTIVITY
      ORDERPATCH ACL SEARCH MKCALENDAR PATCH QUERY
    ].freeze

    def request_method=(request_method)
      if request_method && !HTTP_METHODS.include?(request_method)
        raise ActionController::UnknownHttpMethod, "#{request_method}, accepted HTTP methods are #{HTTP_METHODS.to_sentence}"
      end
      set_header("REQUEST_METHOD", request_method) if request_method
    end

    # `request.headers["Accept-Encoding"]` — the shared
    # `ActionDispatch::Http::Headers` over this request's env.
    def headers
      ActionDispatch::Http::Headers.new(env)
    end

    def [](key)
      @params[key.to_s]
    end

    def authorization
      ActionDispatch::Http::Headers.authorization(@env)
    end

    def request_method
      (@env["REQUEST_METHOD"] || "GET").upcase
    end

    def get? = request_method == "GET"
    def post? = request_method == "POST"
    # Do not route `put?` through `request_method` — that helper upcases,
    # while Rack/Rails compare the raw env value to literal `"PUT"` (so
    # `"put"` must stay false). Shared `runtime/ruby` `put?` is likewise
    # case-sensitive on `@request_method` (no upcase on assign).
    def put? = @env["REQUEST_METHOD"] == "PUT"
    # Rails' "is this a safe request" idiom is `request.get? ||
    # request.head?`, and campfire writes exactly that in
    # `BlockBannedRequests#safe_request?`. This overlay is the Request
    # the ruby-family SERVER actually loads — the `runtime/ruby` copy
    # beside it answers the same questions for the analyzer and the
    # strict targets, so a predicate has to land in both or the one
    # that runs is the one still missing it.
    def head? = request_method == "HEAD"

    def path
      @env["PATH_INFO"] || "/"
    end

    def query_string
      @env["QUERY_STRING"] || ""
    end

    # Rack's SCRIPT_NAME — the prefix the app is mounted under, "" at
    # the root. Campfire's cable helper joins it with Action Cable's
    # mount path to build the socket URL, so every page that renders
    # the layout reads it.
    def script_name
      @env["SCRIPT_NAME"] || ""
    end

    def fullpath
      query_string.empty? ? path : "#{path}?#{query_string}"
    end

    # No middleware rewrites paths here, so original_* == current.
    def original_fullpath = fullpath

    def original_url = "#{base_url}#{fullpath}"
    # Rails' own name for the same string; app code reaches for `url`
    # (campfire stores it as the post-login return path).
    def url = original_url

    # Host (with port when the client sent one), as Rails reports it.
    # The shared `ActionDispatch::Request` has always carried this; the
    # overlay twin did not, and `Rails.application.domain` — the
    # framework default every `_url` helper grounds against — reads it.
    # So an app with no `domain` of its own raised NoMethodError on its
    # first absolute URL: campfire's message row, which links each
    # message by `room_at_message_url`.
    def host
      @env["HTTP_HOST"] || @env["SERVER_NAME"] || "localhost"
    end

    # See the shared twin: HTTPS=on, or a TLS-terminating proxy's
    # X-Forwarded-Proto, which Rack honors unconfigured.
    def ssl?
      return true if @env["HTTPS"] == "on"
      forwarded = @env["HTTP_X_FORWARDED_PROTO"].to_s.split(",").first.to_s.strip.downcase
      return forwarded == "https" unless forwarded.empty?
      @env["rack.url_scheme"].to_s.downcase == "https"
    end

    def protocol
      ssl? ? "https://" : "http://"
    end

    # Twin of the shared class's: the port from the Host header, nil at
    # the scheme's standard one (campfire's default_url_options).
    def optional_port
      h = host
      sep = h.rindex(":")
      return nil if sep.nil? || h.end_with?("]")
      port = h[(sep + 1)..].to_i
      return nil if port == 0 || port == (ssl? ? 443 : 80)
      port
    end

    # Without the scheme's standard port, as the shared twin's: Rails'
    # `host_with_port` writes no `:443`/`:80`, and the CSRF check
    # compares this with an Origin a browser serializes the same way.
    def base_url
      h = host
      default = ssl? ? ":443" : ":80"
      h = h[0, h.length - default.length] if h.end_with?(default)
      "#{protocol}#{h}"
    end

    def remote_ip
      @env["REMOTE_ADDR"] || "127.0.0.1"
    end

    def referer
      @env["HTTP_REFERER"]
    end
    alias referrer referer

    # The User-Agent header. campfire's auth spine records it on every
    # Session row and `deny_bots` filters on it — a hole we opened
    # ourselves (the walk's stub ledger carried it as "ours to
    # implement"). Twin of the shared class's, which stores it.
    def user_agent
      @env["HTTP_USER_AGENT"] || ""
    end

    # Shared constructor — see the twin in
    # `runtime/ruby/action_dispatch/request.rb`. The two classes hold
    # their state differently and so cannot share a `new`; this is the
    # seam one caller uses to build a request on either target.
    def self.for(env, params = {})
      new(env, params)
    end

    # `request.body` — the raw body, as an IO, because that is what
    # Rails hands back and what app code does with it: campfire's bot
    # endpoint reads `reading(request.body) { |b| … }`, which calls
    # `rewind` / `read` / `force_encoding`. A String would answer none
    # of those.
    #
    # The WRITER takes a String (the CGI dispatcher's body, or the test
    # harness's `post url, params: "raw text"`) and wraps it here, so
    # the caller stays target-neutral — the harness is transpiled for
    # spinel too, where `StringIO` is a CRuby thing that does not
    # exist. Wrapping at the overlay boundary keeps that knowledge on
    # the CRuby side, where the rest of this file already lives.
    def body=(value)
      @body = value.is_a?(String) ? StringIO.new(value) : value
    end

    def body
      @body ||= StringIO.new(@env["RAW_POST_DATA"].to_s)
    end

    def xhr?
      @env["HTTP_X_REQUESTED_WITH"] == "XMLHttpRequest"
    end

    def inertia?
      !@env["HTTP_X_INERTIA"].to_s.empty?
    end

    # Query-string params only (Rails' GET-vs-POST split); lobsters'
    # search/time-series pages rebuild URLs from these.
    def query_parameters
      out = {}
      CgiIo.parse_form_into(query_string, out) unless query_string.empty?
      out
    end
  end

  # `ActionDispatch::TestRequest.create(env)` — see the twin in
  # `runtime/ruby/action_dispatch/request.rb` for what it is for. This
  # lane's Request IS env-backed, so `create` is the constructor and
  # every key the env carries is readable, not just the ones a mapping
  # thought to name. A Request subclass, as Rails' is: Core's
  # `class FakeRequest < ActionDispatch::TestRequest` raised "superclass
  # must be an instance of Class (given Module)" at load. Rails' env
  # under the given keys: `Rack::MockRequest.env_for("/")` with its
  # test host, address and agent (TestRequest::DEFAULT_ENV), then the
  # application's `env_config`.
  #
  # The writers are Rails' (actionpack testing/test_request.rb), each a
  # header write: Core's synthetic storefront request sets `host`,
  # `request_method` and `path` on one.
  class TestRequest < Request
    def self.create(env = {})
      base = default_env
      app = defined?(Rails) && Rails.respond_to?(:application) && Rails.application
      base.merge!(app.env_config) if app.respond_to?(:env_config)
      base.merge!(env.each_with_object({}) { |(k, v), h| h[k.to_s] = v })
      base["rack.request.cookie_hash"] ||= {}.with_indifferent_access
      new(base)
    end

    def self.default_env
      {
        "rack.input" => StringIO.new("".b),
        "rack.errors" => StringIO.new,
        "rack.multithread" => true,
        "rack.multiprocess" => true,
        "rack.run_once" => false,
        "REQUEST_METHOD" => "GET",
        "SERVER_NAME" => "example.org",
        "SERVER_PORT" => "80",
        "SERVER_PROTOCOL" => "HTTP/1.1",
        "QUERY_STRING" => "",
        "PATH_INFO" => "/",
        "rack.url_scheme" => "http",
        "HTTPS" => "off",
        "SCRIPT_NAME" => "",
        "CONTENT_LENGTH" => "0",
        "HTTP_HOST" => "test.host",
        "REMOTE_ADDR" => "0.0.0.0",
        "HTTP_USER_AGENT" => "Rails Testing",
      }
    end
    private_class_method :default_env

    def request_method=(method)
      super(method.to_s.upcase)
    end

    def host=(host)
      set_header("HTTP_HOST", host)
    end

    def port=(number)
      set_header("SERVER_PORT", number)
    end

    def request_uri=(uri)
      set_header("REQUEST_URI", uri)
    end

    def path=(path)
      set_header("PATH_INFO", path)
    end

    def action=(action_name)
      path_parameters[:action] = action_name.to_s
    end

    def if_modified_since=(last_modified)
      set_header("HTTP_IF_MODIFIED_SINCE", last_modified)
    end

    def if_none_match=(etag)
      set_header("HTTP_IF_NONE_MATCH", etag)
    end

    def remote_addr=(addr)
      set_header("REMOTE_ADDR", addr)
    end

    def user_agent=(user_agent)
      set_header("HTTP_USER_AGENT", user_agent)
    end

    def accept=(mime_types)
      delete_header("action_dispatch.request.accepts")
      set_header("HTTP_ACCEPT", Array(mime_types).collect(&:to_s).join(","))
    end
  end
end

# Rails' controller-test request and session (actionpack
# action_controller/test_case.rb). Core's checkout PolicyContextBuilder
# builds `ActionController::TestRequest.create(GraphQLController)` and
# gives it a session store. `assign_parameters`, which only
# `ActionController::TestCase#process` calls, is not ported: the test
# harness drives controller actions itself.
module ActionController
  # Rails' TestSession is a Rack `SecureSessionHash` whose data is loaded
  # up front and never written to a store; this is that hash. Keys are
  # Strings, as Rack's session stores them.
  class TestSession
    # Rack::Session::Abstract::Persisted::DEFAULT_OPTIONS.
    DEFAULT_OPTIONS = {
      key: "rack.session",
      path: "/",
      domain: nil,
      expire_after: nil,
      secure: false,
      httponly: true,
      defer: false,
      renew: false,
      sidbits: 128,
      cookie_only: true,
      secure_random: ::SecureRandom,
    }.freeze

    # Rack::Session::SessionId: the cookie's public id, and the hashed
    # private id a store keys on.
    class SessionId
      ID_VERSION = 2

      attr_reader :public_id

      def initialize(public_id)
        @public_id = public_id
      end

      def private_id
        "#{ID_VERSION}::#{Digest::SHA256.hexdigest(public_id)}"
      end

      alias cookie_value public_id
      alias to_s public_id

      def empty? = false

      def inspect = public_id.inspect
    end

    def initialize(session = {}, id = SessionId.new(SecureRandom.hex(16)))
      @id = id
      @data = stringify_keys(session)
      @loaded = true
    end

    def id = @id

    def id_was = @id

    def exists? = true

    def enabled? = true

    def loaded? = @loaded

    def options = {}

    def keys = @data.keys

    def values = @data.values

    def [](key) = @data[key.to_s]

    def []=(key, value)
      @data[key.to_s] = value
    end
    alias store []=

    def key?(key) = @data.key?(key.to_s)
    alias has_key? key?
    alias include? key?

    def fetch(key, *args, &block)
      @data.fetch(key.to_s, *args, &block)
    end

    def dig(*keys)
      keys = keys.map.with_index { |key, i| i.zero? ? key.to_s : key }
      @data.dig(*keys)
    end

    def delete(key) = @data.delete(key.to_s)

    def update(hash)
      @data.update(stringify_keys(hash))
    end
    alias merge! update

    def to_hash = @data.dup
    alias to_h to_hash

    def each(&block) = @data.each(&block)

    def empty? = @data.empty?

    def clear
      @data.clear
    end

    def destroy
      clear
    end

    def load!
      @id
    end

    def inspect = @data.inspect

    private

    def stringify_keys(other)
      other.to_hash.each_with_object({}) { |(key, value), hash| hash[key.to_s] = value }
    end
  end

  class TestRequest < ActionDispatch::TestRequest
    def self.new_session
      TestSession.new
    end

    attr_reader :controller_class

    # Rails' controller-test env is the dispatch one without PATH_INFO
    # (its DEFAULT_ENV); the application's `env_config` still applies.
    def self.create(controller_class)
      env = {}
      app = defined?(Rails) && Rails.respond_to?(:application) && Rails.application
      env = app.env_config.merge(env) if app.respond_to?(:env_config)
      env["rack.request.cookie_hash"] = {}.with_indifferent_access
      new(default_env.merge(env), new_session, controller_class)
    end

    def self.default_env
      env = super
      env.delete("PATH_INFO")
      env
    end
    private_class_method :default_env

    def initialize(env, session, controller_class)
      super(env)

      self.session = session
      self.session_options = TestSession::DEFAULT_OPTIONS.dup
      @controller_class = controller_class
    end

    def query_string=(string)
      set_header("QUERY_STRING", string)
    end

    def content_type=(type)
      set_header("CONTENT_TYPE", type)
    end
  end
end

# `request` accessor on the controller — same overlay-reopen shape as
# `cookies` (runtime/action_controller_cookies.rb).
module ActionController
  class Base
    attr_accessor :request
  end

  # Per-request context reachable from module-function helpers. Rails
  # helpers run in the view context, which delegates `request` to the
  # controller; the emitted helpers are module functions with no such
  # context, so the dispatcher parks the request here (the
  # ActiveSupport::CurrentAttributes pattern) and the Ruby emit path
  # rewrites bare `request` reads in helper/view module bodies to
  # `ActionController::Current.request`. Single-threaded CGI dispatch —
  # plain module state, reset by assignment each request.
  module Current
    class << self
      attr_accessor :request
      # The dispatching controller, parked so module-function helpers
      # can reach per-request session state. Held as the controller
      # (not the session object) because `reset_session` swaps the
      # controller's @session for a fresh instance mid-action — a
      # parked session reference would go stale, and a CSRF token
      # generated during the post-logout render would land in the
      # discarded session instead of the one the dispatch persists.
      attr_accessor :controller
    end

    # The current request's session, or nil outside a dispatch (unit
    # tests construct view helpers without a controller; they get the
    # shared runtime's empty-token behavior).
    def self.session
      c = Current.controller
      c.nil? ? nil : c.session
    end
  end
end
