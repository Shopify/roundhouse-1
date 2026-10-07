# semian 0.28.3 `require "semian/activerecord_postgresql_adapter"`:
# lib/semian/activerecord_adapter.rb and (semian_adapter.rb is lib/semian/adapter.rb)
# lib/semian/activerecord_postgresql_adapter.rb, ported onto the
# PostgreSQL tree's adapter (ruby_overlay active_record_postgresql_adapter.rb
# over Db::ConnectionLifecycle in db.rb). Loading this file is the whole
# install, as requiring the gem file is: it prepends
# Semian::ActiveRecordPostgreSQLAdapter onto
# ActiveRecord::ConnectionAdapters::PostgreSQLAdapter.
#
# The wrap points are the gem's, on the same Rails 8.1 members:
#
#   connect      each PG.connect (Db.new_client)          scope :connection
#   raw_execute  each statement Db runs, unless it is      scope :query
#                COMMIT / ROLLBACK / RELEASE SAVEPOINT
#                (QUERY_ALLOWLIST)
#   active?      each ping (`;`), false when the circuit   scope :ping
#                is open or the bulkhead is busy
#
# and `semian:` comes off the adapter's configuration in `initialize`, as
# the gem takes it off Rails' (Db.configure_database's `semian:` keyword or
# Hash entry).
#
# Differences from the gem, each forced by the runtime:
#
# * ORDER. Db builds and connects every pooled adapter in
#   Db.configure_database, so this file must load before it; loading it
#   afterwards raises, since adapters built earlier would carry no semian
#   identity (the gem's on_load hook guarantees the same order in Rails).
# * ADAPTER ERROR. The gem includes Semian::AdapterError into
#   ActiveRecord::ActiveRecordError. This runtime has no ActiveRecordError
#   (runtime/ruby/active_record/errors.rb), so it is included into the
#   roots the runtime's adapter raises: StatementInvalid,
#   ConnectionNotEstablished and TransactionIsolationError.
# * RAILS 8.2's `execute_intent` branch is not ported; the runtime's
#   adapter is Rails 8.1's `raw_execute` shape.
require_relative "semian"
require_relative "semian_adapter"

unless defined?(ActiveRecord::ConnectionAdapters::PostgreSQLAdapter) &&
    defined?(Db::ConnectionLifecycle) &&
    ActiveRecord::ConnectionAdapters::PostgreSQLAdapter.include?(Db::ConnectionLifecycle)
  raise LoadError, "semian/activerecord_postgresql_adapter needs the PostgreSQL tree's " \
    "ActiveRecord::ConnectionAdapters::PostgreSQLAdapter (runtime/active_record_postgresql_adapter.rb) loaded first"
end
unless Db.configured_databases.empty?
  raise "semian/activerecord_postgresql_adapter must be installed before Db.configure: " \
    "databases #{Db.configured_databases.inspect} already hold unprotected adapters"
end

module Semian
  # lib/semian/activerecord_adapter.rb
  module ActiveRecordAdapter
    QUERY_ALLOWLIST = %r{\A(?:/\*.*?\*/)?\s*(ROLLBACK|COMMIT|RELEASE\s+SAVEPOINT)}i

    module ClassMethods
      def query_allowlisted?(sql, *)
        # COMMIT, ROLLBACK
        tx_command_statement = sql.end_with?("T", "K")

        # RELEASE SAVEPOINT. Nesting past _3 levels won't get bypassed.
        # Active Record does not send trailing spaces or `;`, so we are in the realm of hand crafted queries here.
        savepoint_statement = sql.end_with?("_1", "_2")
        unclear = sql.end_with?(" ", ";")

        if !tx_command_statement && !savepoint_statement && !unclear
          false
        else
          QUERY_ALLOWLIST.match?(sql)
        end
      rescue ArgumentError
        return false unless sql.valid_encoding?

        raise
      end
    end

    class << self
      def included(base)
        base.extend(ClassMethods)
        base.class_eval do
          attr_reader(:raw_semian_options, :semian_identifier)
        end
      end
    end

    def initialize(*options)
      *, config = options
      config = config.dup
      @raw_semian_options = config.delete(:semian)
      @semian_identifier = begin
        name = semian_options && semian_options[:name]
        unless name
          host = config[:host] || "localhost"
          port = config[:port] || semian_adapter_default_port
          name = "#{host}:#{port}"
        end
        :"#{semian_adapter_identifier_prefix}_#{name}"
      end
      super
    end

    def raw_execute(sql, *args, **kwargs, &block)
      if self.class.query_allowlisted?(sql)
        super
      else
        acquire_semian_resource(adapter: semian_adapter_name, scope: :query) do
          super(sql, *args, **kwargs, &block)
        end
      end
    end

    def active?
      acquire_semian_resource(adapter: semian_adapter_name, scope: :ping) do
        super
      end
    rescue resource_busy_error_class, circuit_open_error_class
      false
    end

    def with_resource_timeout
      raise NotImplementedError, "#{self.class} must implement a `with_resource_timeout` method"
    end

    private

    def resource_exceptions
      [
        ActiveRecord::AdapterTimeout,
        ActiveRecord::ConnectionFailed,
        ActiveRecord::ConnectionNotEstablished,
      ]
    end

    def resource_busy_error_class
      self.class::ResourceBusyError
    end

    def circuit_open_error_class
      self.class::CircuitOpenError
    end

    def connect(*args)
      acquire_semian_resource(adapter: semian_adapter_name, scope: :connection) do
        super
      end
    end

    def semian_adapter_name
      raise NotImplementedError, "#{self.class} must implement an `semian_adapter_name` method"
    end

    def semian_adapter_default_port
      raise NotImplementedError, "#{self.class} must implement an `semian_adapter_default_port` method"
    end

    def semian_adapter_identifier_prefix
      raise NotImplementedError, "#{self.class} must implement an `semian_adapter_identifier_prefix` method"
    end
  end
end

# lib/semian/activerecord_postgresql_adapter.rb
module ActiveRecord
  module ConnectionAdapters
    class PostgreSQLAdapter
      if defined?(ActiveRecord::ActiveRecordError)
        ActiveRecord::ActiveRecordError.include(::Semian::AdapterError)
      else
        ActiveRecord::StatementInvalid.include(::Semian::AdapterError)
        ActiveRecord::ConnectionNotEstablished.include(::Semian::AdapterError)
        ActiveRecord::TransactionIsolationError.include(::Semian::AdapterError)
      end
      ::PG::ConnectionBad.include(::Semian::AdapterError)

      class SemianError < ConnectionNotEstablished
        def initialize(semian_identifier, *args)
          super(*args)
          @semian_identifier = semian_identifier
        end
      end

      ResourceBusyError = Class.new(SemianError)
      CircuitOpenError = Class.new(SemianError)
    end
  end
end

module Semian
  module ActiveRecordPostgreSQLAdapter
    include Semian::Adapter
    include Semian::ActiveRecordAdapter

    class << self
      def prepended(base)
        base.extend(Semian::ActiveRecordAdapter::ClassMethods)
      end
    end

    def with_resource_timeout(_temp_timeout)
      # Resource timeouts aren't possible with PostgreSQL because there is no
      # IO level timeout configuration, so we just yield.
      yield
    end

    private

    def resource_exceptions
      super + [::PG::ConnectionBad]
    end

    def semian_adapter_name = :postgresql_adapter

    def semian_adapter_default_port = 5432

    def semian_adapter_identifier_prefix = :postgresql
  end
end

ActiveRecord::ConnectionAdapters::PostgreSQLAdapter.prepend(Semian::ActiveRecordPostgreSQLAdapter)
