# frozen_string_literal: true

# ActionDispatch::Session::AbstractStore and its modules (Shopify/rails
# 5813559f4aed, 8.2.0.alpha actionpack/lib/action_dispatch/middleware/
# session/abstract_store.rb), verbatim over the rack gem's
# Rack::Session::Abstract::Persisted (rack 2.2.24, Core's lockfile).
# Core's MultiSessionStore and ShopifyRedisSessionStore subclass it at
# load; `require "action_dispatch"` defines it before app code.
#
# Two departures, each named where it stands: the namespace reopens the
# shared runtime's `ActionDispatch::Session` class, and
# StaleSessionCheck's `$1.constantize` is the body of String#constantize.
# The request-time paths (`commit_session`, `prepare_session`,
# `set_cookie`) reach Rails' Request::Session, request forgery
# protection's `commit_csrf_token` and the cookie jar, which this tree
# does not carry.
#
# The PostgreSQL CRuby tree only (`project::apply_postgres_bootstrap`).
require "securerandom"
require "rack/utils"
require "rack/request"
require "rack/session/abstract/id"

module ActionDispatch
  # runtime/ruby/action_dispatch/session.rb, which every target shares,
  # declares `ActionDispatch::Session` as the session object (Rails'
  # Request::Session), so the store namespace reopens that class.
  class Session
    class SessionRestoreError < StandardError # :nodoc:
      def initialize
        super("Session contains objects whose class definition isn't available.\n" \
          "Remember to require the classes for all objects kept in the session.\n" \
          "(Original exception: #{$!.message} [#{$!.class}])\n")
        set_backtrace $!.backtrace
      end
    end

    module Compatibility
      def initialize(app, options = {})
        options[:key] ||= "_session_id"
        super
      end

      def generate_sid
        sid = SecureRandom.hex(16)
        sid.encode!(Encoding::UTF_8)
        sid
      end

    private
      def initialize_sid # :doc:
        @default_options.delete(:sidbits)
        @default_options.delete(:secure_random)
      end

      def make_request(env)
        ActionDispatch::Request.new env
      end
    end

    module StaleSessionCheck
      def load_session(env)
        stale_session_check! { super }
      end

      def extract_session_id(env)
        stale_session_check! { super }
      end

      def stale_session_check!
        yield
      rescue ArgumentError => argument_error
        if argument_error.message =~ %r{undefined class/module ([\w:]*\w)}
          begin
            # Note that the regexp does not allow $1 to end with a ':'.
            # String#constantize (activesupport inflector/methods.rb:331).
            Object.const_get($1)
          rescue LoadError, NameError
            raise ActionDispatch::Session::SessionRestoreError
          end
          retry
        else
          raise
        end
      end
    end

    module SessionObject # :nodoc:
      def commit_session(req, res)
        req.commit_csrf_token
        super(req, res)
      end

      def prepare_session(req)
        Http::Session.create(self, req, @default_options)
      end

      def loaded_session?(session)
        !session.is_a?(Http::Session) || session.loaded?
      end
    end

    class AbstractStore < Rack::Session::Abstract::Persisted
      include Compatibility
      include StaleSessionCheck
      include SessionObject

      private
        def set_cookie(request, response, cookie)
          request.cookie_jar[key] = cookie
        end
    end

    class AbstractSecureStore < Rack::Session::Abstract::PersistedSecure
      include Compatibility
      include StaleSessionCheck
      include SessionObject

      def generate_sid
        Rack::Session::SessionId.new(super)
      end

      private
        def set_cookie(request, response, cookie)
          request.cookie_jar[key] = cookie
        end
    end
  end

  # Rails 8.2's session object, ActionDispatch::Http::Session (8.2.0.alpha
  # actionpack/lib/action_dispatch/http/session.rb; Request::Session
  # before 8.2), is the shared runtime's `ActionDispatch::Session` on
  # this tree, so the name answers that class. identity_client 59.3.0's
  # session_class.rb picks `ActionDispatch::Http::Session` by `defined?`
  # at load and names it in its sigs. Its class methods (`create`, `find`) are
  # request-time paths this tree does not carry (SessionObject above).
  module Http
    Session = ::ActionDispatch::Session unless const_defined?(:Session, false)
  end
end
