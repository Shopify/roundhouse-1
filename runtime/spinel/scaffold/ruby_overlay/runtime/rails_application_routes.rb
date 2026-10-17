# frozen_string_literal: true
# CRuby-only Rails::Application#routes surface.
#
# Lobsters' config/application.rb composes absolute URLs via
# `Rails.application.routes.url_helpers.root_url(host:, protocol:)`
# (Story#short_id_url and friends). The shared runtime's Application is
# deliberately empty (NameError over silent stubs); this overlay adds
# the real thing for CRuby: url_helpers resolves against the emitted
# RouteHelpers, and root_url composes protocol://host + root_path.
# Overlay, not shared runtime — the kwarg signature is exactly the
# forwarding shape strict targets refuse (see the kwarg-forwarding gap).
#
# `#routes` returns `RouteSet`, which is Rails' name for this object
# (`ActionDispatch::Routing::RouteSet`). The name must not be
# `RouteTable`: a nested `RouteTable` hides the emitted top-level
# `RouteTable` that `recognize_path` reads.
require "uri"
require_relative "action_controller/parameter_missing"

module ActionController
  # Raised by `recognize_path` for a verb that Rails does not accept.
  # It lives in this overlay and not in runtime/ruby/, so `check` still
  # reports app code that names it. Its superclass is Rails'
  # `ActionControllerError` from parameter_missing.rb, the one
  # action_controller_exceptions.rb (a native gem's `require
  # "action_controller/metal/exceptions"`) reopens this class over; a
  # different superclass here made that require a TypeError.
  class UnknownHttpMethod < ActionControllerError
  end
end

module Rails
  class Application
    def routes
      RouteSet
    end

    module RouteSet
      # `ActionDispatch::Request::HTTP_METHODS`, in the Rails order.
      HTTP_METHODS = %w[
        OPTIONS GET HEAD POST PUT DELETE TRACE CONNECT
        PROPFIND PROPPATCH MKCOL COPY MOVE LOCK UNLOCK
        VERSION-CONTROL REPORT CHECKOUT CHECKIN UNCHECKOUT MKWORKSPACE UPDATE LABEL MERGE BASELINE-CONTROL MKACTIVITY
        ORDERPATCH ACL SEARCH MKCALENDAR PATCH
      ].freeze

      # The Rails paths of the Active Storage controllers that
      # `ActiveStorage::Routes.table` names. The emitted
      # `RouteTable::CONTROLLER_PATHS` holds the app's namespaced ones.
      CONTROLLER_PATHS = {
        active_storage_blobs_redirect: "active_storage/blobs/redirect",
        active_storage_blobs_proxy: "active_storage/blobs/proxy",
        active_storage_representations_redirect: "active_storage/representations/redirect",
        active_storage_disk: "active_storage/disk",
        active_storage_direct_uploads: "active_storage/direct_uploads"
      }.freeze

      def self.url_helpers
        UrlHelpers
      end

      # ActionDispatch::Routing::RouteSet#append / #prepend (actionpack
      # 8.2.0.alpha route_set.rb:464-470): the blocks an initializer adds
      # (`app.routes.append { … }`), which Rails draws when it loads the
      # routes. They are kept; this tree's routes are compiled from
      # config/routes.rb and drawing a block is not ported, so the railtie
      # runtime names each at the routes-reloader step
      # (Railtie.refuse_undrawn_application_routes).
      def self.append(&block)
        appended_blocks << block
      end

      def self.prepend(&block)
        prepended_blocks << block
      end

      def self.appended_blocks # :nodoc:
        @appended_blocks ||= []
      end

      def self.prepended_blocks # :nodoc:
        @prepended_blocks ||= []
      end

      # Rails' `recognize_path` for one path. The result has Symbol
      # keys and String values: `:controller`, `:action`, each dynamic
      # segment, and `:format` last. `:controller` is the Rails path of
      # a namespaced or Active Storage controller (`"admin/posts"`), and
      # the router symbol of any other controller.
      # `environment[:method]` is the verb, a Symbol or a String in any
      # case, and GET is the default. Other keys have no effect.
      def self.recognize_path(path, environment = {})
        # Rails normalizes a path, but not a full URL. The error message
        # shows the result.
        path = normalize_path(path) unless path.to_s.include?("://")
        # Rack's `MockRequest.env_for` reads the request path with this
        # parser. The query and the fragment have no effect. The router
        # ignores host and subdomain constraints, so the host has no
        # effect either.
        begin
          uri = URI::Parser.new.parse(path)
        rescue URI::InvalidURIError => e
          raise ActionController::RoutingError, e.message
        end
        request_path = uri.path.to_s
        request_path = "/#{request_path}" unless request_path.start_with?("/")
        verb = (environment[:method] || "GET").to_s.upcase
        # Rails checks the verb after it parses the path and before it
        # matches. The check also keeps out "ANY", which the router uses
        # for a `via: :all` route.
        unless HTTP_METHODS.include?(verb)
          accepted = "#{HTTP_METHODS[0...-1].join(", ")}, and #{HTTP_METHODS.last}"
          raise ActionController::UnknownHttpMethod, "#{verb}, accepted HTTP methods are #{accepted}"
        end
        matched = ActionDispatch::Router.match(verb, request_path, table)
        # The router ignores a trailing slash. Rails does not normalize
        # the path of a full URL, so there a trailing slash matches no
        # route without a glob. On a glob route, Rails keeps the slash in
        # the glob value. The match result does not show a glob, so this
        # method gives no match for that case too.
        matched = nil if request_path != "/" && request_path.end_with?("/")
        raise ActionController::RoutingError, "No route matches #{path.inspect}" if matched.nil?

        params = matched.path_params
        controller = CONTROLLER_PATHS[matched.controller] ||
                     (defined?(::RouteTable) ? ::RouteTable::CONTROLLER_PATHS[matched.controller] : nil)
        recognized = { controller: controller || matched.controller.to_s, action: matched.action.to_s }
        params.each { |name, value| recognized[name.to_sym] = value unless name == "format" }
        recognized[:format] = params["format"] if params.key?("format")
        recognized
      end

      # `ActionDispatch::Journey::Router::Utils.normalize_path`: one
      # leading slash, no doubled slashes, no trailing slash, and an
      # uppercase percent-encoded octet.
      def self.normalize_path(path)
        normalized = "/#{path}".squeeze("/")
        return normalized if normalized == "/"

        normalized.delete_suffix("/").gsub(/(%[a-f0-9]{2})/) { $1.upcase }
      end
      private_class_method :normalize_path

      # The table that the dispatcher composes, built once.
      # `RouteTable.table` makes new Route objects on each call. The
      # routes emit defines `RouteTable.root` only for an app with a
      # root route, and no `RouteTable` for an app that draws none (Core
      # keeps its routes in component files). Rails `recognize_path` skips
      # a redirect route and tries the later routes, so the table leaves
      # the redirect routes out.
      def self.table
        @table ||= (drawn_table + ActiveStorage::Routes.table)
                   .reject { |route| route.controller == :roundhouse_redirects }
      end

      def self.drawn_table
        return [] unless defined?(::RouteTable)

        (::RouteTable.respond_to?(:root) ? [::RouteTable.root] : []) + ::RouteTable.table
      end
      private_class_method :drawn_table
    end

    # `Rails.application.routes.url_helpers`: the module an app extends or
    # includes to get the named route helpers. Every `<name>_path` the
    # emitted RouteHelpers answers is answered here too, and `<name>_url`
    # is that path under `protocol://host[:port]`, so a call on the module
    # (`UrlHelpers.posts_path`) and one through a class or singleton that
    # includes it agree. A name no drawn route explains is a NoMethodError
    # as in Rails, or, when initializers appended route blocks this tree
    # does not draw (`routes.append { … }`), a refusal that names them: the
    # route may be in one of those, and the plain NoMethodError would read
    # as a route that does not exist.
    module UrlHelpers
      module Dispatch
        URL_KEYS = %i[host protocol port only_path script_name].freeze

        def method_missing(name, *args, **kwargs, &block)
          text = name.to_s
          return super unless text.end_with?("_path", "_url")
          path_name = text.end_with?("_url") ? "#{text.delete_suffix("_url")}_path" : text
          unless defined?(::RouteHelpers) && ::RouteHelpers.respond_to?(path_name)
            undrawn = Rails::Application::RouteSet.appended_blocks.length + Rails::Application::RouteSet.prepended_blocks.length
            return super if undrawn.zero?
            raise NotImplementedError, "error[route_set_not_drawn]: #{text}: no route of this tree's drawn route set is named " \
              "#{path_name.delete_suffix("_path")}, and #{undrawn} route block(s) added by Rails.application.routes.append/prepend " \
              "are not drawn (drawing a route set at run time, ActionDispatch::Routing::Mapper, is not ported)"
          end
          path = ::RouteHelpers.public_send(path_name, *args, **kwargs.reject { |key, _| URL_KEYS.include?(key) })
          return path if text == path_name || kwargs[:only_path]
          host = kwargs[:host]
          raise ArgumentError, "Missing host to link to! Please provide the :host parameter, set default_url_options[:host], or set :only_path to true" if host.nil?
          port = kwargs[:port] ? ":#{kwargs[:port]}" : ""
          "#{kwargs[:protocol] || "http"}://#{host}#{port}#{kwargs[:script_name]}#{path}"
        end

        def respond_to_missing?(name, include_private = false)
          text = name.to_s
          return super unless text.end_with?("_path", "_url")
          path_name = text.end_with?("_url") ? "#{text.delete_suffix("_url")}_path" : text
          (defined?(::RouteHelpers) && ::RouteHelpers.respond_to?(path_name)) || super
        end
      end
      include Dispatch
      extend Dispatch

      def self.root_url(host: "localhost", protocol: "http")
        "#{protocol}://#{host}#{RouteHelpers.root_path}"
      end
    end
  end
end
