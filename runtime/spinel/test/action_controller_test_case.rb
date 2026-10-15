# ActionController::TestCase::Behavior's request half (actionpack 8.2
# action_controller/test_case.rb): `process` and the verb helpers that
# call it, `assigns`, `flash`, `cookies`, and the response helpers a
# controller test reads off `@response`. Spliced after the base classes
# `emit::ruby::MINITEST_TEST_CASES` declares, so the stub's `setup`
# callback (`setup_controller_request_and_response`) builds the
# controller and request this dispatches to.
#
# `process` drives the emitted controller's own `process_action` — the
# dispatcher lowering synthesizes with the before/around/after filter
# chain, `rescue_from` handlers and the case over the actions — the way
# `main.rb` and the integration harness do, and answers an
# `ActionResponse`, so `assert_response` and `assert_redirected_to` read
# it as they read an integration response.
#
# DIVERGENCES from Rails, stated:
# - Rails generates the request path from the app's routes and raises
#   `ActionController::UrlGenerationError` for an unroutable
#   controller/action. Here a route that names the controller and the
#   action (its `:segment`s all given as params) supplies the path and
#   the path parameters; with no such route (Core's routes are not
#   ingested) the path is `/<controller_path>/<action>` and every param
#   is a query/body param. No error is raised for an unroutable action.
# - A multipart upload (`fixture_file_upload` / `UploadedFile` values) is
#   not encoded to a body and parsed back: the upload objects are the
#   params as given.
# - `@controller` is reused across requests in one test (Rails does the
#   same), its response state recycled and the ivars the last request
#   set cleared, as `clear_instance_variables_between_requests` does.
module ActionController
  class TestCase < TestBase
    module Behavior
      # `Rack::Response::Helpers` / `ActionDispatch::TestResponse`'s
      # status predicates and `redirect_url`, on the response this
      # harness builds.
      module ResponseHelpers
        def response_code = status

        def code = status.to_s

        def info? = status >= 100 && status < 200

        def successful? = status >= 200 && status < 300

        def redirection? = status >= 300 && status < 400

        def client_error? = status >= 400 && status < 500

        def server_error? = status >= 500 && status < 600

        def error? = client_error? || server_error?

        def ok? = status == 200

        def created? = status == 201

        def accepted? = status == 202

        def no_content? = status == 204

        def bad_request? = status == 400

        def unauthorized? = status == 401

        def forbidden? = status == 403

        def not_found? = status == 404

        def missing? = status == 404

        def method_not_allowed? = status == 405

        def redirect_url = location
      end

      # The ivars a controller carries before any request writes to it,
      # which `assigns` does not report.
      ASSIGNS_PROTECTED = [:@_ivars, :@cookies].freeze

      def process(action, method: "GET", params: nil, session: nil, body: nil, flash: {}, format: nil, xhr: false, as: nil)
        if @controller.nil?
          raise "@controller is nil: make sure you set it in your test's setup method."
        end
        controller = @controller
        # `Object#method` by name: an app controller may define a `method` reader of its own.
        if Object.instance_method(:method).bind_call(controller, :process_action).owner == ActionController::Base
          raise NotImplementedError,
            "roundhouse: ActionController::TestCase#process (#{action}): #{controller.class} has no emitted process_action " \
            "(lowering synthesizes it for a controller with a routed action, or one a controller test names)"
        end
        clear_instance_variables_between_requests(controller)
        ActionResponse.include(ResponseHelpers) unless ActionResponse.include?(ResponseHelpers)

        action = action.to_s
        http_method = method.to_s.upcase
        request = @request
        controller_path = controller.class.name.to_s.delete_suffix("Controller").underscore

        parameters = {}
        raw_params = params.nil? ? {} : (params.respond_to?(:to_unsafe_h) ? params.to_unsafe_h : params.to_h)
        raw_params.each { |key, value| parameters[key.to_s] = value }
        content_type = nil
        if as
          content_type = as.to_sym == :json ? "application/json" : (defined?(Mime) && Mime[as] ? Mime[as].to_s : as.to_s)
          format ||= as
        end
        parameters["format"] = format.to_s unless format.nil?

        path_params = { "controller" => controller_path, "action" => action }
        path = routed_path_for(controller_path, action, parameters, path_params) || "/#{controller_path}/#{action}"
        non_path = parameters.reject { |key, _| path_params.key?(key) }

        query_string = ""
        query_params = {}
        request_params = {}
        raw_body = body.nil? ? nil : body.to_s
        if http_method == "GET" || http_method == "HEAD"
          if contains_upload?(non_path)
            query_params = non_path
          elsif !non_path.empty?
            query_string = ActionView::ViewHelpers.to_query_pairs(non_path, "")
            CgiIo.parse_form_into(query_string, query_params)
          end
        elsif content_type == "application/json" || (content_type && content_type.end_with?("json"))
          request_params = JSON.parse(JSON.generate(non_path.as_json)) unless non_path.empty?
          raw_body ||= JSON.generate(non_path) unless non_path.empty?
        elsif contains_upload?(non_path)
          request_params = non_path
        elsif !non_path.empty?
          encoded = ActionView::ViewHelpers.to_query_pairs(non_path, "")
          CgiIo.parse_form_into(encoded, request_params)
          raw_body ||= encoded
          content_type ||= "application/x-www-form-urlencoded"
        end

        merged = {}.with_indifferent_access
        query_params.each { |key, value| merged[key] = value }
        request_params.each { |key, value| merged[key] = value }
        path_params.each { |key, value| merged[key] = value }

        request.set_header("REQUEST_METHOD", http_method)
        request.set_header("PATH_INFO", path)
        request.set_header("QUERY_STRING", query_string)
        request.set_header("CONTENT_TYPE", content_type) unless content_type.nil?
        unless raw_body.nil?
          request.set_header("RAW_POST_DATA", raw_body)
          request.body = raw_body
        end
        request.path_parameters = path_params
        request.params = merged
        request.request_parameters = request_params
        request.session.update(session) if session

        if xhr
          request.set_header("HTTP_X_REQUESTED_WITH", "XMLHttpRequest")
          unless request.has_header?("HTTP_ACCEPT")
            request.set_header("HTTP_ACCEPT", ["text/javascript", "text/html", "application/xml", "text/xml", "*/*"].join(", "))
          end
        end

        controller.recycle!
        controller.params = merged
        controller.path_parameters = path_params
        controller.session = request.session
        inbound_flash = (@__flash_persisted || {}).merge((flash || {}).each_with_object({}) { |(k, v), h| h[k.to_s] = v })
        controller.flash = ActionDispatch::Flash.new(inbound_flash)
        controller.cookies = ActionController::CookieJar.new(cookies.to_h)
        controller.request = request
        controller.request_method = http_method
        controller.request_path = path
        controller.query_string = query_string
        format_name = format.to_s
        controller.request_format = :json if format_name == "json"
        controller.request_format = :turbo_stream if format_name == "turbo_stream"
        controller.request_format = :rss if format_name == "rss"
        controller.request_format = :xml if format_name == "xml"
        controller.request_format = :js if format_name == "js"
        ActionController::Current.request = request
        ActionController::Current.controller = controller

        begin
          # The lowered dispatcher's `case` is over Symbols (a router's
          # `matched.action`), where Rails' `process` takes the String.
          controller.process_action(action.to_sym)
        ensure
          finish_process(controller, request, xhr)
        end
        @response
      end

      %i[get post patch put delete head].each do |verb|
        define_method(verb) { |action, **options| process(action, method: verb.to_s.upcase, **options) }
      end

      # `ActionDispatch::TestProcess#flash`: the request's flash, the one
      # the last request left (and its carried-over entries).
      def flash
        @controller.flash
      end

      # The browser jar a controller test reads and seeds
      # (`cookies[:k] = v` before a request, `cookies[:k]` after).
      def cookies
        @__cookies = ActionController::CookieJar.new if @__cookies.nil?
        @__cookies
      end

      # rails-controller-testing's `assigns`: the controller's instance
      # variables, minus the framework's own, by name without the `@`.
      def assigns(key = nil)
        assigned = {}.with_indifferent_access
        protected_ivars = ActionController::TestCase::Behavior.framework_ivars
        @controller.instance_variables.each do |ivar|
          next if protected_ivars.include?(ivar) || ivar.to_s.start_with?("@_")
          assigned[ivar.to_s.delete_prefix("@")] = @controller.instance_variable_get(ivar)
        end
        key.nil? ? assigned : assigned[key]
      end

      # The response assertions are the integration harness's. Rails'
      # `assert_response` / `assert_redirected_to` are
      # `RoundhouseResponseAssertions` (test/minitest_runner.rb, which
      # loads after this file, so it is included on first use; the
      # include reaches the classes that already include Behavior). The
      # rest are `RequestDispatch`'s (runtime/spinel/test/test_helper.rb),
      # which read `@__response` / `@__request` as `process` sets them.
      %i[assert_response assert_redirected_to].each do |name|
        define_method(name) do |*args, **kwargs, &block|
          ActionController::TestCase::Behavior.include(::RoundhouseResponseAssertions)
          super(*args, **kwargs, &block)
        end
      end

      %i[redirection_url_for_assertion assert_select].each do |name|
        define_method(name) do |*args, **kwargs, &block|
          ::RequestDispatch.instance_method(name).bind_call(self, *args, **kwargs, &block)
        end
      end

      def self.framework_ivars
        @framework_ivars ||= (ActionController::Base.new.instance_variables + ASSIGNS_PROTECTED).freeze
      end

      private

      def contains_upload?(value)
        case value
        when Hash then value.values.any? { |v| contains_upload?(v) }
        when Array then value.any? { |v| contains_upload?(v) }
        else value.respond_to?(:original_filename)
        end
      end

      # The route that names this controller and action and whose
      # `:segment`s the params all supply: its path, with the segments
      # moved from the params into `path_params`. nil when no route
      # reaches it.
      def routed_path_for(controller_path, action, parameters, path_params)
        return nil unless defined?(RouteTable) && RouteTable.respond_to?(:table)
        route = RouteTable.table.find do |candidate|
          next false unless candidate.action.to_s == action
          next false unless candidate.controller.to_s == controller_path || candidate.controller.to_s == controller_path.split("/").last
          segments = candidate.pattern_parts.select { |part| part.start_with?(":") }.map { |part| part[1..] }
          segments.all? { |segment| segment == "format" || parameters.key?(segment) }
        end
        return nil if route.nil?
        parts = route.pattern_parts.map do |part|
          next part unless part.start_with?(":")
          name = part[1..]
          value = parameters[name].respond_to?(:to_param) ? parameters[name].to_param.to_s : parameters[name].to_s
          path_params[name] = value
          value
        end
        parts.join("/")
      end

      # Rails' AbstractController `clear_instance_variables_between_
      # requests`: the first request records the controller's ivars, each
      # later one drops what the request before it set.
      def clear_instance_variables_between_requests(controller)
        if controller.instance_variable_defined?(:@_ivars)
          kept = controller.instance_variable_get(:@_ivars)
          (controller.instance_variables - kept - [:@_ivars]).each { |ivar| controller.remove_instance_variable(ivar) }
        end
        controller.instance_variable_set(:@_ivars, controller.instance_variables)
      end

      # What Rails' `process` does in its `ensure`: the request and
      # response are the controller's, cookies fold back into the jar,
      # the flash is stored for the next request.
      def finish_process(controller, request, xhr)
        controller.commit_cache_control!
        copied_headers = {}
        index = 0
        while index < controller.headers.size
          copied_headers[controller.headers.key_at(index)] = controller.headers.val_at(index)
          index += 1
        end
        pending = controller.cookies.pending
        carried = cookies.to_h.merge(pending)
        pending.each { |key, value| carried.delete(key) if value == "" }
        @__cookies = ActionController::CookieJar.new(carried)
        @response = ActionResponse.new(
          status: controller.status,
          body: controller.body,
          location: controller.location,
          flash: controller.flash,
          cookies: ActionController::CookieJar.new(pending),
          content_type: controller.content_type,
          cache_control_max_age: controller.cache_control_max_age,
          cache_control_public: controller.cache_control_public,
          cache_control_store: controller.cache_control,
          headers: copied_headers
        )
        @__response = @response
        @request = request
        @__request = request
        # A controller that `reset_session`ed swapped its session object;
        # the request's (the test's `session`) follows it.
        unless controller.session.equal?(request.session)
          request.session.clear
          request.session.update(controller.session.to_hash)
        end
        @__flash_persisted = controller.flash.to_persisted
        if xhr
          request.delete_header("HTTP_X_REQUESTED_WITH")
          request.delete_header("HTTP_ACCEPT")
        end
        request.set_header("QUERY_STRING", "")
      end
    end
  end
end
