# frozen_string_literal: true
require_relative "action_dispatch/router"
# Whether a render answers `Vary: Accept` (ruby-family dispatchers).
require_relative "action_dispatch/mime_negotiation"
require_relative "action_dispatch/flash"
require_relative "action_dispatch/session"
require_relative "action_dispatch/exceptions"
# X-Request-Id and X-Runtime (ActionDispatch::RequestId, Rack::Runtime),
# stamped by both ruby-family dispatchers around every app response.
require_relative "action_dispatch/request_id"
require_relative "action_dispatch/content_security_policy"

# actionpack lib/action_dispatch/routing/route_set.rb:849, from
# Shopify/rails 5813559f4aed (8.2.0.alpha; docs/RAILS_VERSION.md): the
# options `url_for` handles itself and never passes to a route's path
# generator. lower::route_url_options applies the same list to route
# helper calls when it compiles them. App code also reads the constant.
# Core's url_for patches subtract from it in a module body (essentials'
# lib/patches/action_dispatch_url_for_param_filter.rb:
# `RESERVED_OPTIONS - URL_FOR_RESERVED_OPTIONS_ALLOWED`) and in a test
# helper (`RESERVED_OPTIONS - [:params]`). The rest of the route set is
# runtime/spinel/rails_engine.rb's, on the trees that carry engines.
# This file adds only the constant, so it reopens the class with no
# superclass, as that file declares it.
module ActionDispatch
  module Routing
    class RouteSet
      RESERVED_OPTIONS = [:host, :protocol, :port, :subdomain, :domain, :tld_length,
                          :trailing_slash, :anchor, :params, :only_path, :script_name,
                          :original_script_name].freeze
    end
  end
end
