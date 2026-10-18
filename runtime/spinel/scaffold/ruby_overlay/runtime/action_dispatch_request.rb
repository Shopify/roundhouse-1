# frozen_string_literal: true
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
require "uri"
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

    # ActionDispatch::Http::FilterParameters#parameter_filter: the
    # filter over the env's "action_dispatch.parameter_filter" (railties'
    # `env_config` puts it in every request's env, which this dispatcher
    # does not build, so the app's config answers when the env has none).
    def parameter_filter
      filters = get_header("action_dispatch.parameter_filter") || Rails.application.env_config["action_dispatch.parameter_filter"]
      ActiveSupport::ParameterFilter.new(filters || [])
    end

    def add_header(key, value)
      if value.nil?
        get_header(key)
      elsif has_header?(key)
        set_header(key, "#{get_header(key)},#{value}")
      else
        set_header(key, value)
      end
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

    # Rails' `controller_class` (actionpack request.rb): the class the
    # route's `:controller` capture names, `PASS_NOT_FOUND` when the
    # route captured none (a bare request outside a route), and
    # MissingController when the name resolves to nothing. Core's
    # CurrentRequest#entrypoint reads it on every background job. The
    # lookup is a constant lookup by name, so it lives with the CRuby
    # overlay.
    PASS_NOT_FOUND = Class.new do
      def self.action(_name)
        proc { [404, { "X-Cascade" => "pass" }, []] }
      end

      def self.action_encoding_template(_action)
        false
      end
    end

    def controller_class
      params = path_parameters
      params[:action] ||= "index"
      controller_class_for(params[:controller])
    end

    def controller_class_for(name)
      return PASS_NOT_FOUND unless name

      # Core's tree carries the full Inflector (its acronyms included:
      # "graph_api" is `GraphApi`, "graph_ql" `GraphQL`); a tree with
      # only the minimal one capitalizes each segment.
      const_name =
        if ActiveSupport::Inflector.respond_to?(:camelize) && ActiveSupport::Inflector.respond_to?(:underscore)
          ActiveSupport::Inflector.camelize(ActiveSupport::Inflector.underscore(name.to_s)) + "Controller"
        else
          name.to_s.split("/").map { |part| part.split("_").map(&:capitalize).join }.join("::") + "Controller"
        end
      begin
        Object.const_get(const_name)
      rescue NameError => error
        if const_name.split("::").include?(error.name.to_s)
          raise ActionDispatch::MissingController.new(error.message, error.name)
        else
          raise
        end
      end
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

    # Rack::Request#cookies: the request's cookies as a name => value
    # Hash, parsed once from the Cookie header and kept under
    # `rack.request.cookie_hash` (a TestRequest starts with an empty one,
    # which a test fills). Rack's rules: pairs split on `;`, the
    # first occurrence of a name wins, values are URI-decoded.
    # Core's Checkouts::One::ClientDetails reads the shared-discount-code
    # cookie off the request this way. (`cookies` on a controller is the
    # CookieJar, runtime/action_controller_cookies.rb.)
    def cookies
      fetch_header("rack.request.cookie_hash") do |key|
        parsed = {}
        get_header("HTTP_COOKIE").to_s.split(/; */).each do |pair|
          name, value = pair.split("=", 2)
          next if name.nil? || name.empty? || parsed.key?(name)
          parsed[name] = value && URI.decode_www_form_component(value)
        end
        set_header(key, parsed)
      end
    end

    # `request.filtered_parameters` - see the twin in
    # runtime/ruby/action_dispatch/request.rb.
    def filtered_parameters
      all = {}
      @params.each { |k, v| all[k] = v }
      # The route's captures live in the env here (Symbol keys), not in an ivar.
      path_parameters.each { |k, v| all[k.to_s] = v }
      ActiveSupport::ParameterFilter.new(Rails.application.config_filter_parameters).filter(all)
    end

    # Rails' `request.cookie_jar`: the jar over this request's cookies,
    # built once and kept in the env (`ActionDispatch::Cookies::CookieJar.
    # build`, runtime/ruby/action_controller/cookies.rb). Core's
    # Checkouts::One::ClientDetails reads `cookie_jar.signed[...]`.
    def cookie_jar
      fetch_header("action_dispatch.cookies") do |key|
        set_header(key, ActionDispatch::Cookies::CookieJar.build(self, cookies))
      end
    end

    # Rails' `request.authorization`: the header, or one of the names a
    # proxy or rewrite moves it to (the shared Headers table).
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

    # Rack::Request::Helpers' `path_info`: the PATH_INFO header, "" when
    # absent (a controller-test request has none until `path=`). Core's
    # MultiSessionStore routes a request by it.
    def path_info
      @env["PATH_INFO"].to_s
    end

    def path_info=(value)
      set_header("PATH_INFO", value.to_s)
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

    def port
      authority = @env.fetch("HTTP_X_FORWARDED_HOST", "").to_s.split(",").last.to_s.strip
      if authority.empty?
        authority = host
        if !@env.key?("HTTP_HOST") && (@env.key?("SERVER_NAME") || !authority.include?(":"))
          authority = "#{authority}:#{@env.fetch("SERVER_PORT", "").to_s}"
        end
      end
      sep = authority.rindex(":")
      unless sep.nil?
        value = authority[(sep + 1)..].to_s
        return value.to_i if value.match?(/\A[0-9]+\z/)
      end
      ssl? ? 443 : 80
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

    # Twin of the shared Request: scheme stripped, standard port already
    # dropped by `base_url`.
    def host_with_port
      b = base_url
      p = protocol
      return "" if b.length < p.length
      b[p.length..].to_s
    end

    # Same memo as the shared class. This overlay replaces that class on
    # the CRuby lane, so the method has to live here too.
    def content_security_policy
      return @content_security_policy if defined?(@csp_ready) && @csp_ready
      @csp_ready = true
      src = Rails.application.content_security_policy
      @content_security_policy = src.nil? ? nil : src.dup
      @content_security_policy
    end

    # ActionDispatch::Request#remote_ip: the address the RemoteIp
    # middleware stores under `action_dispatch.remote_ip`, else the
    # Rack ip. Without that middleware (tests) a writer stands in for it.
    def remote_ip
      (@env["action_dispatch.remote_ip"] || @env["REMOTE_ADDR"] || "127.0.0.1").to_s
    end

    def remote_ip=(address)
      @env["action_dispatch.remote_ip"] = address
    end

    # Rack::Request#ip: the first REMOTE_ADDR address that is not a
    # trusted proxy (loopback / private ranges), else the last such
    # X-Forwarded-For hop, else REMOTE_ADDR as it stands.
    TRUSTED_PROXY = /\A(127\.|::1\z|fe80:|fc|fd|10\.|172\.(1[6-9]|2\d|3[01])\.|192\.168\.|unix)/i

    def ip
      remote = @env["REMOTE_ADDR"].to_s.split(/[,\s]+/).reject(&:empty?)
      external = remote.reject { |addr| addr.match?(TRUSTED_PROXY) }
      return external.first unless external.empty?

      forwarded = @env["HTTP_X_FORWARDED_FOR"].to_s.split(/[,\s]+/).reject(&:empty?)
      hop = forwarded.reverse.find { |addr| !addr.match?(TRUSTED_PROXY) }
      hop || remote.first || forwarded.last
    end

    # ActionDispatch::Request#request_id / #uuid: the id the
    # ActionDispatch::RequestId middleware stores under
    # `action_dispatch.request_id`. Without that middleware in the
    # stack (controller tests), do what it does: take the sanitized
    # X-Request-Id header, else a fresh SecureRandom.uuid, and store it
    # so every later read in the request agrees.
    def request_id
      existing = @env["action_dispatch.request_id"]
      return existing if existing

      external = @env["HTTP_X_REQUEST_ID"].to_s.gsub(/[^\w\-@]/, "")[0, 255]
      @env["action_dispatch.request_id"] = external.nil? || external.empty? ? SecureRandom.uuid : external
    end
    alias uuid request_id

    def request_id=(id)
      @env["action_dispatch.request_id"] = id
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

    # The raw Accept-* request headers, as Rails' `HTTP_HEADERS` readers
    # answer them (`Checkouts::One::ClientDetails.from_request` reads
    # `request.accept_language`): the header's string, nil when unset.
    def accept_charset
      @env["HTTP_ACCEPT_CHARSET"]
    end

    def accept_encoding
      @env["HTTP_ACCEPT_ENCODING"]
    end

    def accept_language
      @env["HTTP_ACCEPT_LANGUAGE"]
    end

    # Rails' content negotiation (actionpack http/mime_negotiation.rb).
    # `content_mime_type` is the CONTENT_TYPE header's type; `accepts`
    # the Accept header's types in q order (the content type when the
    # header is absent); `formats` what `format` answers from: the
    # `format` parameter, else the Accept types when the header is not a
    # browser's, else the path's extension, else JS for an XHR, else
    # HTML. `format` has to be public here: `Kernel#format` is private
    # and would otherwise answer `request.format` with a NoMethodError.
    BROWSER_LIKE_ACCEPTS = /,\s*\*\/\*|\*\/\*\s*,/

    def content_mime_type
      fetch_header("action_dispatch.request.content_type") do |k|
        ctype = @env["CONTENT_TYPE"].to_s
        value = ctype.empty? ? nil : (Mime::Type.lookup(ctype) rescue nil)
        set_header(k, value)
      end
    end

    # Rack::Request::Helpers: the CONTENT_TYPE header, nil when absent or
    # empty, and its media type, the part before any parameters, downcased.
    def content_type
      value = get_header("CONTENT_TYPE")
      value.nil? || value.empty? ? nil : value
    end

    def media_type
      type = content_type
      type && type.split(/\s*[;,]\s*/, 2).first.downcase
    end

    def accepts
      fetch_header("action_dispatch.request.accepts") do |k|
        header = @env["HTTP_ACCEPT"].to_s.strip
        types = header.empty? ? [content_mime_type] : parse_accept_header(header)
        set_header(k, types)
      end
    end

    def formats
      fetch_header("action_dispatch.request.formats") do |k|
        param_format = path_parameters[:format] || @params["format"] || @params[:format]
        accept = @env["HTTP_ACCEPT"].to_s
        types =
          if param_format && !param_format.to_s.empty?
            Array(Mime[param_format])
          elsif (xhr? && (!accept.empty? || content_mime_type)) || (!accept.empty? && accept !~ BROWSER_LIKE_ACCEPTS)
            accepts.compact
          elsif (extension = format_from_path_extension)
            [extension]
          elsif xhr?
            [Mime[:js]]
          else
            [Mime[:html]]
          end
        set_header(k, types.select { |type| type.symbol || type.to_s == "*/*" })
      end
    end

    def formats=(extensions)
      set_header("action_dispatch.request.formats", extensions)
    end

    def format
      formats.first || Mime::Type.new("", nil)
    end

    def format=(extension)
      @params["format"] = extension.to_s
      path_parameters[:format] = extension.to_s
      set_header("action_dispatch.request.formats", [Mime::Type.lookup_by_extension(extension.to_s)].compact)
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
      out.with_indifferent_access
    end

    # Rack's `request.GET` / `request.POST`, which Rails' `query_parameters`
    # and `request_parameters` alias: the query string's params and the
    # body's params, each alone. Core's AdminAreaController reads
    # `request.GET["debug"]` on every admin request.
    def GET
      query_parameters
    end

    def POST
      request_parameters.with_indifferent_access
    end

    def format_from_path_extension
      path = get_header("action_dispatch.original_path") || get_header("PATH_INFO")
      match = path && path.match(/\.(\w+)\z/)
      match && Mime[match[1]]
    end
    private :format_from_path_extension

    # `Mime::Type.parse` for an Accept header: the media types in q
    # order, ties by position; `*/*` is the wildcard, an unregistered
    # but well-formed type keeps a nil symbol, a malformed one is dropped.
    def parse_accept_header(header)
      items = []
      header.split(",").each_with_index do |part, index|
        name, *params = part.split(/\s*;\s*/)
        name = name.to_s.strip.downcase
        next if name.empty?
        q = params.map { |p| p[/\Aq=([\d.]+)\z/, 1] }.compact.first
        items << [-(q ? q.to_f : 1.0), index, name]
      end
      items.sort.map { |(_, _, name)|
        name == "*/*" ? Mime::ALL : (Mime::Type.lookup(name) rescue nil)
      }.compact.uniq
    end
    private :parse_accept_header
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

    # Rack::Response::Helpers' header accessors, over the response
    # headers: this controller IS its own response, so
    # `response.set_header(name, value)` (Core's API deprecation concern
    # writes its reason header so) lands in the store the dispatchers
    # copy to the outgoing response.
    def get_header(name)
      @headers[name]
    end

    def set_header(name, value)
      @headers[name] = value
    end

    def has_header?(name)
      !@headers[name].nil?
    end

    def delete_header(name)
      @headers.delete(name)
    end

    # Rails' controller-test `recycle!` (actionpack
    # action_controller/test_case.rb): the response this controller is
    # (it IS its own response here) back to a fresh one, so a second
    # request in one test starts from status 200 and an empty body. The
    # request's inputs are the caller's to assign.
    def recycle!
      @status = 200
      @body = +""
      @location = nil
      @content_type = "text/html; charset=utf-8"
      @content_type_explicit = false
      @headers = ActionController::HeaderStore.new
      @performed = false
      @head_response = false
      @cache_control = ActionController::CacheControlStore.new
      @cookies = nil
      nil
    end
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

# Rails' forgery-protection strategies (actionpack
# action_controller/metal/request_forgery_protection.rb,
# `ProtectionMethods`): what `protect_from_forgery with:` runs when a
# request fails `verified_request?`. Core's AdminAreaController#suppress_session
# calls `ProtectionMethods::NullSession.new(self).handle_unverified_request`
# directly, and the essentials session middleware tests drive it with a
# stub controller.
module ActionController
  # actionpack's `InvalidCrossOriginRequest < ActionControllerError`.
  class InvalidCrossOriginRequest < ActionControllerError
  end

  module RequestForgeryProtection
    module ProtectionMethods
      # The session of a request that failed verification: empty, never
      # loaded from the store, and (with `session_options[:skip]`, which
      # the dispatcher honors) never written back.
      class NullSessionHash < ActionDispatch::Session
        # Rack's SessionHash#destroy is a no-op here, and the session
        # reports it exists (Rails 8.2).
        def destroy; end

        def exists?
          true
        end

        def enabled?
          false
        end
      end

      # The cookie jar of such a request: no inbound cookies are read and
      # nothing written through it reaches the response.
      class NullCookieJar < ActionController::CookieJar
        def self.build(request, cookies)
          new
        end

        def initialize(inbound = {})
          super({})
        end

        def raw_set(key, value)
          value.to_s
        end
      end

      # `protect_from_forgery with: :null_session` (Rails' default for
      # API-style endpoints): the request goes on, as a visitor with an
      # empty session and no cookies.
      class NullSession
        def initialize(controller)
          @controller = controller
        end

        def handle_unverified_request
          request = @controller.request
          session = NullSessionHash.new
          request.session = session
          request.session_options = { skip: true }
          @controller.session = session
          @controller.cookies = NullCookieJar.new
          nil
        end
      end

      # `with: :reset_session`: the session is discarded, the request goes on.
      class ResetSession
        def initialize(controller)
          @controller = controller
        end

        def handle_unverified_request
          @controller.reset_session
        end
      end

      # `with: :exception`.
      class Exception
        attr_accessor :warning_message

        def initialize(controller)
          @controller = controller
        end

        def handle_unverified_request
          raise ActionController::InvalidCrossOriginRequest, warning_message
        end
      end
    end
  end
end
