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
require_relative "active_support_hash_keys"

# `ActiveRecord::SessionStore::Session` (the ActiveRecordStore default
# session class) loads from runtime/active_record_session_store.rb on its
# first reference.
module ActiveRecord
  module SessionStore
    autoload :Session, File.expand_path("active_record_session_store", __dir__)
  end
end

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

      # Rack::Session::Abstract::Persisted::DEFAULT_OPTIONS (rack 2.2.24
      # session/abstract/id.rb:234), which Ruby's lookup finds through the
      # superclass. Written out as a literal here so the analyzer types
      # `AbstractStore::DEFAULT_OPTIONS` (a subclass's
      # `DEFAULT_OPTIONS = AbstractStore::DEFAULT_OPTIONS.merge(...)`);
      # the test compares it with the rack constant.
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
        secure_random: ::SecureRandom
      }.freeze

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

    # activerecord-session_store 2.2.0 (rails/activerecord-session_store,
    # branch hm-xlyxlmxnwnrtyypm, Core's Gemfile.lock),
    # lib/action_dispatch/session/active_record_store.rb, verbatim: the
    # database-backed store Core's ShopifyMysqlSessionStore subclasses at
    # load and registers as `MultiSessionStore.stores[:services_internal]`.
    # Requiring the gem loads 198 Rails framework files (extensions/core/
    # gems.yml), so the store is ported here; its default session class,
    # `ActiveRecord::SessionStore::Session`, is the lazily loaded
    # runtime/active_record_session_store.rb.
    #
    # One departure: the gem assigns `session_class` when Session loads
    # (`ActiveSupport.on_load(:active_record)`), so the class attribute is
    # always set once Active Record is up. Here Session loads on first use,
    # so `session_class` is a hand-written inheritable reader that falls
    # back to that constant (class_attribute's per-subclass override,
    # without its dependency on the descendants tracker).
    class ActiveRecordStore < ActionDispatch::Session::AbstractSecureStore
      # The class used for session storage. Defaults to
      # ActiveRecord::SessionStore::Session; a subclass inherits and may
      # override it (`class_attribute :session_class`).
      class << self
        def session_class
          if instance_variable_defined?(:@session_class)
            @session_class
          elsif superclass.respond_to?(:session_class)
            superclass.session_class
          else
            ActiveRecord::SessionStore::Session
          end
        end

        def session_class=(value)
          @session_class = value
        end

        def session_class?
          !!session_class
        end
      end

      def session_class
        instance_variable_defined?(:@session_class) ? @session_class : self.class.session_class
      end

      def session_class=(value)
        @session_class = value
      end

      def session_class?
        !!session_class
      end

      SESSION_RECORD_KEY = 'rack.session.record'
      ENV_SESSION_OPTIONS_KEY = Rack::RACK_SESSION_OPTIONS

      def initialize(app, options = {})
        @secure_session_only = options.delete(:secure_session_only) { false }
        super(app, options)
      end

    private
      def get_session(request, sid)
        logger.silence do
          unless sid and session = get_session_with_fallback(sid)
            # If the sid was nil or if there is no pre-existing session under the sid,
            # force the generation of a new sid and associate a new session associated with the new sid
            sid = generate_sid
            session = session_class.new(:session_id => sid.private_id, :data => {})
          end
          request.env[SESSION_RECORD_KEY] = session
          [sid, session.data]
        end
      end

      def write_session(request, sid, session_data, options)
        logger.silence do
          record, sid = get_session_model(request, sid)
          record.data = session_data
          return false unless record.save

          session_data = record.data
          if session_data && session_data.respond_to?(:each_value)
            session_data.each_value do |obj|
              obj.clear_association_cache if obj.respond_to?(:clear_association_cache)
            end
          end

          sid
        end
      end

      def delete_session(request, session_id, options)
        logger.silence do
          if sid = current_session_id(request)
            if model = get_session_with_fallback(sid)
              data = model.data
              model.destroy
            end
          end

          request.env[SESSION_RECORD_KEY] = nil

          unless options[:drop]
            new_sid = generate_sid

            if options[:renew]
              new_model = session_class.new(:session_id => new_sid.private_id, :data => data)
              new_model.save
              request.env[SESSION_RECORD_KEY] = new_model
            end
            new_sid
          end
        end
      end

      def get_session_model(request, id)
        logger.silence do
          model = get_session_with_fallback(id)
          unless model
            id = generate_sid
            model = session_class.new(:session_id => id.private_id, :data => {})
            model.save
          end
          if request.env[ENV_SESSION_OPTIONS_KEY][:id].nil?
            request.env[SESSION_RECORD_KEY] = model
          else
            request.env[SESSION_RECORD_KEY] ||= model
          end
          [model, id]
        end
      end

      def get_session_with_fallback(sid)
        if sid && !self.class.private_session_id?(sid.public_id)
          if (secure_session = session_class.find_by_session_id(sid.private_id))
            secure_session
          elsif !@secure_session_only && (insecure_session = session_class.find_by_session_id(sid.public_id))
            insecure_session.session_id = sid.private_id # this causes the session to be secured
            insecure_session
          end
        end
      end

      def find_session(request, id)
        model, id = get_session_model(request, id)
        [id, model.data]
      end

      module NilLogger
        def self.silence
          yield
        end
      end

      def logger
        ActiveRecord::Base.logger || NilLogger
      end

      def self.private_session_id?(session_id)
        # user tried to retrieve a session by a private key?
        session_id =~ /\A\d+::/
      end

    end
  end

  # Rails 8.2's session object, ActionDispatch::Http::Session (Shopify/rails
  # 5eb8c4c45bee, actionpack/lib/action_dispatch/http/session.rb), verbatim:
  # a lazily loaded view of one request's session in a store, which the
  # stores above build in `prepare_session` and a test support store builds
  # with `Http::Session.create(store, request, options)`. It is a class of
  # its own, not the shared runtime's `ActionDispatch::Session` (the
  # controller-test session object that holds its data up front).
  # identity_client 59.3.0's session_class.rb picks `ActionDispatch::Http::Session`
  # by `defined?` at load and names it in its sigs.
  module Http
    # Session is responsible for lazily loading the session from store.
    class Session # :nodoc:
      DisabledSessionError    = Class.new(StandardError)
      ENV_SESSION_KEY         = Rack::RACK_SESSION # :nodoc:
      ENV_SESSION_OPTIONS_KEY = Rack::RACK_SESSION_OPTIONS # :nodoc:

      # Singleton object used to determine if an optional param wasn't specified.
      Unspecified = Object.new

      # Creates a session hash, merging the properties of the previous session if any.
      def self.create(store, req, default_options)
        session_was = find req
        session     = new(store, req)
        session.merge! session_was if session_was

        set(req, session)
        Options.set(req, Options.new(store, default_options))
        session
      end

      def self.disabled(req)
        new(nil, req, enabled: false).tap do
          Options.set(req, Options.new(nil, { id: nil }))
        end
      end

      def self.find(req)
        req.get_header ENV_SESSION_KEY
      end

      def self.set(req, session)
        req.set_header ENV_SESSION_KEY, session
      end

      def self.delete(req)
        req.delete_header ENV_SESSION_KEY
      end

      class Options # :nodoc:
        def self.set(req, options)
          req.set_header ENV_SESSION_OPTIONS_KEY, options
        end

        def self.find(req)
          req.get_header ENV_SESSION_OPTIONS_KEY
        end

        def initialize(by, default_options)
          @by       = by
          @delegate = default_options.dup
        end

        def [](key)
          @delegate[key]
        end

        def id(req)
          @delegate.fetch(:id) {
            @by.send(:extract_session_id, req)
          }
        end

        def []=(k, v);        @delegate[k] = v; end
        def to_hash;          @delegate.dup; end
        def values_at(*args); @delegate.values_at(*args); end
      end

      def initialize(by, req, enabled: true)
        @by       = by
        @req      = req
        @delegate = {}
        @loaded   = false
        @exists   = nil # We haven't checked yet.
        @enabled  = enabled
        @id_was = nil
        @id_was_initialized = false
      end

      def id
        options.id(@req)
      end

      def enabled?
        @enabled
      end

      def options
        Options.find @req
      end

      def destroy
        clear

        if enabled?
          options = self.options || {}
          @by.send(:delete_session, @req, options.id(@req), options)

          # Load the new sid to be written with the response.
          @loaded = false
          load_for_write!
        end
      end

      # Returns value of the key stored in the session or `nil` if the given key is
      # not found in the session.
      def [](key)
        load_for_read!
        key = key.to_s

        if key == "session_id"
          id&.public_id
        else
          @delegate[key]
        end
      end

      # Returns the nested value specified by the sequence of keys, returning `nil` if
      # any intermediate step is `nil`.
      def dig(*keys)
        load_for_read!
        keys = keys.map.with_index { |key, i| i.zero? ? key.to_s : key }
        @delegate.dig(*keys)
      end

      # Returns true if the session has the given key or false.
      def has_key?(key)
        load_for_read!
        @delegate.key?(key.to_s)
      end
      alias :key? :has_key?
      alias :include? :has_key?

      # Returns keys of the session as Array.
      def keys
        load_for_read!
        @delegate.keys
      end

      # Returns values of the session as Array.
      def values
        load_for_read!
        @delegate.values
      end

      # Writes given value to given key of the session.
      def []=(key, value)
        load_for_write!
        @delegate[key.to_s] = value
      end
      alias store []=

      # Clears the session.
      def clear
        load_for_delete!
        @delegate.clear
      end

      # Returns the session as Hash.
      def to_hash
        load_for_read!
        @delegate.dup.delete_if { |_, v| v.nil? }
      end
      alias :to_h :to_hash

      # Updates the session with given Hash.
      #
      #     session.to_hash
      #     # => {"session_id"=>"e29b9ea315edf98aad94cc78c34cc9b2"}
      #
      #     session.update({ "foo" => "bar" })
      #     # => {"session_id"=>"e29b9ea315edf98aad94cc78c34cc9b2", "foo" => "bar"}
      #
      #     session.to_hash
      #     # => {"session_id"=>"e29b9ea315edf98aad94cc78c34cc9b2", "foo" => "bar"}
      def update(hash)
        unless hash.respond_to?(:to_hash)
          raise TypeError, "no implicit conversion of #{hash.class.name} into Hash"
        end

        load_for_write!
        @delegate.update hash.to_hash.stringify_keys
      end
      alias :merge! :update

      # Deletes given key from the session.
      def delete(key)
        load_for_delete!
        @delegate.delete key.to_s
      end

      # Returns value of the given key from the session, or raises `KeyError` if can't
      # find the given key and no default value is set. Returns default value if
      # specified.
      #
      #     session.fetch(:foo)
      #     # => KeyError: key not found: "foo"
      #
      #     session.fetch(:foo, :bar)
      #     # => :bar
      #
      #     session.fetch(:foo) do
      #       :bar
      #     end
      #     # => :bar
      def fetch(key, default = Unspecified, &block)
        load_for_read!
        if default == Unspecified
          @delegate.fetch(key.to_s, &block)
        else
          @delegate.fetch(key.to_s, default, &block)
        end
      end

      def inspect
        if loaded?
          super
        else
          "#<#{self.class}:0x#{(object_id << 1).to_s(16)} not yet loaded>"
        end
      end

      def exists?
        return false unless enabled?
        return @exists unless @exists.nil?
        @exists = @by.send(:session_exists?, @req)
      end

      def loaded?
        @loaded
      end

      def empty?
        load_for_read!
        @delegate.empty?
      end

      def each(&block)
        to_hash.each(&block)
      end

      def id_was
        load_for_read!
        @id_was
      end

      private
        def load_for_read!
          load! if !loaded? && exists?
        end

        def load_for_write!
          if enabled?
            load! unless loaded?
          else
            raise DisabledSessionError, "Your application has sessions disabled. To write to the session you must first configure a session store"
          end
        end

        def load_for_delete!
          load! if enabled? && !loaded?
        end

        def load!
          if enabled?
            @id_was_initialized = true unless exists?
            id, session = @by.load_session @req
            options[:id] = id
            @delegate.replace(session.stringify_keys)
            @id_was = id unless @id_was_initialized
          end
          @id_was_initialized = true
          @loaded = true
        end
    end
  end
end
