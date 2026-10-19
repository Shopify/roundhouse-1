# frozen_string_literal: true

# ActiveRecord::SessionStore and its default session class,
# ActiveRecord::SessionStore::Session (activerecord-session_store 2.2.0,
# rails/activerecord-session_store branch hm-xlyxlmxnwnrtyypm, Core's
# Gemfile.lock: lib/active_record/session_store.rb and
# lib/active_record/session_store/session.rb), verbatim over the runtime's
# ActiveRecord::Base. ActionDispatch::Session::ActiveRecordStore
# (runtime/action_dispatch_session_store.rb) names it as its default
# `session_class`, and Core's ShopifyMysqlSessionStore reads sessions
# through it (`ActiveRecord::SessionStore::Session.find_by_session_id`).
#
# Loaded on first reference to the constant (the port's `autoload`), not at
# boot: the gem loads it from `ActiveSupport.on_load(:active_record)`,
# which in this tree runs with the app's models.
#
# Departures, each named where it stands: the gem's `session_store/sql_bypass`
# and railtie (rake tasks and a deprecator registration) are not ported, nor
# `drop_table!`/`create_table!`'s migrations caller.
#
# The PostgreSQL CRuby tree only (`project::apply_postgres_bootstrap`).
require "base64"
require "json"
require "thread"

module ActiveRecord
  module SessionStore
    module ClassMethods # :nodoc:
      mattr_accessor :serializer

      def serialize(data)
        serializer_class.dump(data) if data
      end

      def deserialize(data)
        serializer_class.load(data) if data
      end

      def drop_table!
        connection.schema_cache.clear_data_source_cache!(table_name)
        connection.drop_table table_name
      end

      def create_table!
        connection.schema_cache.clear_data_source_cache!(table_name)
        connection.create_table(table_name) do |t|
          t.string session_id_column, :limit => 255
          t.text data_column_name
        end
        connection.add_index table_name, session_id_column, :unique => true
      end

      def serializer_class
        case self.serializer
          when :marshal, nil then
            MarshalSerializer
          when :json then
            JsonSerializer
          when :hybrid then
            HybridSerializer
          when :null then
            NullSerializer
          else
            self.serializer
        end
      end

      # Use Marshal with Base64 encoding
      class MarshalSerializer
        def self.load(value)
          Marshal.load(::Base64.decode64(value))
        end

        def self.dump(value)
          ::Base64.encode64(Marshal.dump(value))
        end
      end

      # Uses built-in JSON library to encode/decode session
      class JsonSerializer
        def self.load(value)
          hash = JSON.parse(value)
          hash.is_a?(Hash) ? hash.with_indifferent_access[:value] : hash
        end

        def self.dump(value)
          JSON.dump(value: value)
        end
      end

      # Transparently migrates existing session values from Marshal to JSON
      class HybridSerializer < JsonSerializer
        MARSHAL_SIGNATURE = 'BAh'.freeze

        def self.load(value)
          if needs_migration?(value)
            Marshal.load(::Base64.decode64(value))
          else
            super
          end
        end

        def self.needs_migration?(value)
          value.start_with?(MARSHAL_SIGNATURE)
        end
      end

      # Defer serialization to the ActiveRecord database adapter
      class NullSerializer
        def self.load(value)
          value
        end

        def self.dump(value)
          value
        end
      end
    end

    # The default Active Record class.
    class Session < ActiveRecord::Base
      extend ClassMethods
      SEMAPHORE = Mutex.new

      ##
      # :singleton-method:
      # Customizable data column name. Defaults to 'data'.
      cattr_accessor :data_column_name
      self.data_column_name = 'data'

      before_save :serialize_data!
      before_save :raise_on_session_data_overflow!

      class << self
        def data_column_size_limit
          @data_column_size_limit ||= columns_hash[data_column_name].limit
        end

        def find_by_session_id(session_id)
          where(session_id: session_id).first
        end

        private
          def session_id_column
            'session_id'
          end
      end

      def initialize(*)
        @data = nil
        super
      end

      # Lazy-deserialize session state.
      def data
        @data ||= self.class.deserialize(read_attribute(@@data_column_name)) || {}
      end

      attr_writer :data

      # Has the session been loaded yet?
      def loaded?
        @data
      end

      # This method was introduced when addressing CVE-2019-16782
      # (see https://github.com/rack/rack/security/advisories/GHSA-hrqr-hxpp-chr3).
      # Sessions created on version <= 1.1.3 were guessable via a timing attack.
      # To secure sessions created on those old versions, this method can be called
      # on all existing sessions in the database. Users will not lose their session
      # when this is done.
      def secure!
        raw_session_id = self.session_id

        if ActionDispatch::Session::ActiveRecordStore.private_session_id?(raw_session_id)
          # is already private, nothing to do
        else
          session_id_object = Rack::Session::SessionId.new(raw_session_id)
          update_column(:session_id, session_id_object.private_id)
        end
      end

      private
        def serialize_data!
          unless loaded?
            throw :abort
          end
          write_attribute(@@data_column_name, self.class.serialize(data))
        end

        # Ensures that the data about to be stored in the database is not
        # larger than the data storage column. Raises
        # ActionController::SessionOverflowError.
        def raise_on_session_data_overflow!
          unless loaded?
            throw :abort
          end
          limit = self.class.data_column_size_limit
          if limit and read_attribute(@@data_column_name).size > limit
            raise ActionController::SessionOverflowError
          end
        end
    end
  end
end

ActionDispatch::Session::ActiveRecordStore.session_class = ActiveRecord::SessionStore::Session
