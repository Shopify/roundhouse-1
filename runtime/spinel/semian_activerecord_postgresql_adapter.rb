# semian 0.28.3 `require "semian/activerecord_postgresql_adapter"`:
# lib/semian/adapter.rb, lib/semian/activerecord_adapter.rb and
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

# lib/semian/adapter.rb
module Semian
  module Adapter
    def semian_identifier
      raise NotImplementedError, "Semian adapters must implement a `semian_identifier` method"
    end

    def semian_resource
      return @semian_resource if @semian_resource

      case semian_options
      when false
        @semian_resource = UnprotectedResource.new(semian_identifier)
      when nil
        Semian.logger.info("Semian is not configured for #{self.class.name}: #{semian_identifier}")
        @semian_resource = UnprotectedResource.new(semian_identifier)
      else
        options = semian_options.dup
        options.delete(:name)
        options[:consumer] = self
        options[:exceptions] ||= []
        options[:exceptions] += resource_exceptions
        resource = ::Semian.retrieve_or_register(semian_identifier, **options)

        @semian_resource = resource unless options.fetch(:dynamic, false)

        resource
      end
    end

    def clear_semian_resource
      @semian_resource = nil
    end

    private

    def acquire_semian_resource(scope:, adapter:, &block)
      return yield if resource_already_acquired?

      semian_resource.acquire(scope: scope, adapter: adapter, resource: self) do
        mark_resource_as_acquired(&block)
      end
    rescue ::Semian::OpenCircuitError => error
      last_error = semian_resource.circuit_breaker.last_error
      message = "#{error.message} caused by #{last_error&.message}"
      last_error = nil unless last_error.is_a?(Exception) # Net::HTTPServerError is not an exception
      raise self.class::CircuitOpenError.new(semian_identifier, message), cause: last_error
    rescue ::Semian::BaseError => error
      raise self.class::ResourceBusyError.new(semian_identifier, error.message)
    rescue *resource_exceptions => error
      error.semian_identifier = semian_identifier if error.respond_to?(:semian_identifier=)
      raise
    end

    def semian_options
      return @semian_options if defined? @semian_options

      options = raw_semian_options

      symbolized_options = options && options.transform_keys(&:to_sym) # rubocop:disable Style/SafeNavigation
      symbolized_options.tap do
        @semian_options = symbolized_options if !symbolized_options || !symbolized_options.fetch(:dynamic, false)
      end
    end

    def raw_semian_options
      raise NotImplementedError, "Semian adapters must implement a `raw_semian_options` method"
    end

    def resource_exceptions
      raise NotImplementedError, "Semian adapters must implement a `resource_exceptions` method"
    end

    def resource_already_acquired?
      @resource_acquired
    end

    def mark_resource_as_acquired
      previous = @resource_acquired
      @resource_acquired = true
      yield
    ensure
      @resource_acquired = previous
    end
  end

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
