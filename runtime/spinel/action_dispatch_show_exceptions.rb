# frozen_string_literal: true

# ActionDispatch::ShowExceptions (Shopify/rails 5813559f4aed, 8.2.0.alpha
# actionpack/lib/action_dispatch/middleware/show_exceptions.rb), verbatim.
# Core's SessionUnavailableMiddleware subclasses it at load and calls
# `super(app, exceptions_app)` and `render_exception(request, error)`;
# `require "action_dispatch"` defines it before app code.
#
# ActionDispatch::Constants (action_dispatch/constants.rb, which
# `require "action_dispatch"` loads), verbatim, for `X_CASCADE`.
#
# Not carried, so not yet Rails' behavior: `call`'s rescue reaches
# ActionDispatch::ExceptionWrapper and Rails' Request header API
# (`get_header` / `set_header`), whose NameError leaves `call`; and
# `pass_response` reads ActionDispatch::Response, whose NameError
# `render_exception`'s failsafe turns into its 500.
#
# The PostgreSQL CRuby tree only (`project::apply_postgres_bootstrap`).
require "rack"
require "rack/version"

module ActionDispatch
  module Constants
    # Response Header keys for Rack 2.x and 3.x
    if Gem::Version.new(Rack::RELEASE) < Gem::Version.new("3")
      VARY = "Vary"
      CONTENT_ENCODING = "Content-Encoding"
      CONTENT_SECURITY_POLICY = "Content-Security-Policy"
      CONTENT_SECURITY_POLICY_REPORT_ONLY = "Content-Security-Policy-Report-Only"
      LOCATION = "Location"
      FEATURE_POLICY = "Feature-Policy"
      X_REQUEST_ID = "X-Request-Id"
      X_CASCADE = "X-Cascade"
      SERVER_TIMING = "Server-Timing"
      STRICT_TRANSPORT_SECURITY = "Strict-Transport-Security"
    else
      VARY = "vary"
      CONTENT_ENCODING = "content-encoding"
      CONTENT_SECURITY_POLICY = "content-security-policy"
      CONTENT_SECURITY_POLICY_REPORT_ONLY = "content-security-policy-report-only"
      LOCATION = "location"
      FEATURE_POLICY = "feature-policy"
      X_REQUEST_ID = "x-request-id"
      X_CASCADE = "x-cascade"
      SERVER_TIMING = "server-timing"
      STRICT_TRANSPORT_SECURITY = "strict-transport-security"
    end

    if Gem::Version.new(Rack::RELEASE) < Gem::Version.new("3.1")
      UNPROCESSABLE_CONTENT = :unprocessable_entity
    else
      UNPROCESSABLE_CONTENT = :unprocessable_content
    end
  end
end

module ActionDispatch
  # # Action Dispatch ShowExceptions
  #
  # This middleware rescues any exception returned by the application and calls an
  # exceptions app that will wrap it in a format for the end user.
  #
  # The exceptions app should be passed as a parameter on initialization of
  # `ShowExceptions`. Every time there is an exception, `ShowExceptions` will
  # store the exception in `env["action_dispatch.exception"]`, rewrite the
  # `PATH_INFO` to the exception status code, and call the Rack app.
  #
  # In Rails applications, the exceptions app can be configured with
  # `config.exceptions_app`, which defaults to ActionDispatch::PublicExceptions.
  #
  # If the application returns a response with the `X-Cascade` header set to
  # `"pass"`, this middleware will send an empty response as a result with the
  # correct status code. If any exception happens inside the exceptions app, this
  # middleware catches the exceptions and returns a failsafe response.
  class ShowExceptions
    def initialize(app, exceptions_app)
      @app = app
      @exceptions_app = exceptions_app
    end

    def call(env)
      @app.call(env)
    rescue Exception => exception
      request = ActionDispatch::Request.new env
      backtrace_cleaner = request.get_header("action_dispatch.backtrace_cleaner")
      wrapper = ExceptionWrapper.new(backtrace_cleaner, exception)
      request.set_header "action_dispatch.exception", wrapper.unwrapped_exception
      request.set_header "action_dispatch.report_exception", !wrapper.rescue_response?

      if wrapper.show?(request)
        render_exception(request.dup, wrapper)
      else
        raise exception
      end
    end

    private
      def render_exception(request, wrapper)
        status = wrapper.status_code
        request.set_header "action_dispatch.original_path", request.path_info
        request.set_header "action_dispatch.original_request_method", request.raw_request_method
        fallback_to_html_format_if_invalid_mime_type(request)
        request.path_info = "/#{status}"
        request.request_method = "GET"
        response = @exceptions_app.call(request.env)
        response[1][Constants::X_CASCADE] == "pass" ? pass_response(status) : response
      rescue Exception => failsafe_error
        $stderr.puts "Error during failsafe response: #{failsafe_error}\n  #{failsafe_error.backtrace * "\n  "}"

        [500, { Rack::CONTENT_TYPE => "text/plain; charset=utf-8" },
          ["500 Internal Server Error\n" \
          "If you are the administrator of this website, then please read this web " \
          "application's log file and/or the web server's log file to find out what " \
          "went wrong."]]
      end

      def fallback_to_html_format_if_invalid_mime_type(request)
        # If the MIME type for the request is invalid then the @exceptions_app may not
        # be able to handle it. To make it easier to handle, we switch to HTML.
        begin
          request.content_mime_type
        rescue ActionDispatch::Http::MimeNegotiation::InvalidType
          request.set_header "CONTENT_TYPE", "text/html"
        end

        begin
          request.formats
        rescue ActionDispatch::Http::MimeNegotiation::InvalidType
          request.set_header "HTTP_ACCEPT", "text/html"
        end
      end

      def pass_response(status)
        [status, { Rack::CONTENT_TYPE => "text/html; charset=#{Response.default_charset}",
                  Rack::CONTENT_LENGTH => "0" }, []]
      end
  end
end
