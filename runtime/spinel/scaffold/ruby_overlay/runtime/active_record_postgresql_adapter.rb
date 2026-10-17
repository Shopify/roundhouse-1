# frozen_string_literal: true
# PostgreSQL connection identity and transaction statements. Query and result
# behavior stays in the shared Connection and SqlAdapterQueries framework.
#
# The adapter is Rails' unit of connection: `Db` builds one per pooled
# PostgreSQL session (`Db.adapter_class`) and drives Rails' lifecycle on
# it (Db::ConnectionLifecycle: connect!, verify!, reconnect!, active?,
# raw_execute), so a module prepended to this class (semian's
# ActiveRecordPostgreSQLAdapter) wraps every connect, statement and ping
# of the tree. `Base.connection` is the adapter its class's pool leases
# to the current thread/fiber, as Rails' lease_connection
# (runtime/active_record_connection_handling.rb); before the database is
# configured it is a per-thread adapter without a session.
#
# Rails' ancestry: PostgreSQLAdapter < AbstractAdapter, so a reopen of
# AbstractAdapter (Core's podding patch: `abstract_connection_name`,
# `allow_preconnect`) reaches the live connection, and a module prepended
# to AbstractAdapter (Core's TransactionConnectionTracker around
# `transaction`, the test helper's FollowerReadWriteGuard around
# `preventing_writes?`) wraps what Rails defines there. So the members
# Rails defines on AbstractAdapter and its modules (DatabaseStatements,
# Savepoints) live on AbstractAdapter here too (this file, and the
# AbstractAdapter reopens of runtime/active_record_transactions.rb and
# runtime/active_record_connection_handling.rb); PostgreSQLAdapter keeps
# what Rails' PostgreSQLAdapter and its PostgreSQL:: modules override.
# The shared `Connection` facade stays the parent of AbstractAdapter, so
# every raw-SQL member it already has is unchanged.
#
# NOT modeled: the lifecycle module (Db::ConnectionLifecycle) mixes Rails'
# abstract members (connect!, verify!, reconnect!, disconnect!) with the
# PostgreSQL overrides (active?, reconnect, configure_connection) at
# AbstractAdapter's level, so a module prepended to AbstractAdapter wraps
# a PostgreSQL override Rails' PostgreSQLAdapter would shadow. The
# adapter's checkout/checkin callbacks (`define_callbacks :checkout,
# :checkin`) are not run by this runtime's pool; `set_callback` and
# `skip_callback` on the adapter refuse by name (below).
#
# Schema reflection: `columns(table_name)` reads the live catalog as
# Rails' PostgreSQL adapter does (column_definitions, a "SCHEMA" query)
# into Rails' Column / PostgreSQL::Column objects with their
# SqlTypeMetadata (name, sql_type, type, limit, precision, scale, null,
# default, default_function, collation, comment, serial, identity,
# generated). The pool's schema cache (runtime/active_record_connection_
# handling.rb) keeps them per table, and Base's columns_hash / columns /
# column_names read them through it. A column's `cast_type` is the type
# object of the attribute type port (runtime/active_model_type.rb) for the
# types it carries; for any other (json, jsonb, an array, an enum, ...)
# or where the port is not carried, `cast_type` refuses by name, and the
# column's type, limit, precision and scale are still Rails'.
require "delegate"
require_relative "active_record_statement_errors"

module ActiveRecord
  module ConnectionAdapters
    # sql_type_metadata.rb.
    class SqlTypeMetadata
      attr_reader :sql_type, :type, :limit, :precision, :scale

      def initialize(sql_type: nil, type: nil, limit: nil, precision: nil, scale: nil)
        @sql_type = sql_type
        @type = type
        @limit = limit
        @precision = precision
        @scale = scale
      end

      def ==(other)
        other.is_a?(SqlTypeMetadata) &&
          sql_type == other.sql_type &&
          type == other.type &&
          limit == other.limit &&
          precision == other.precision &&
          scale == other.scale
      end
      alias eql? ==

      def hash
        [SqlTypeMetadata, @sql_type, @type, @limit, @precision, @scale].hash
      end

      def as_schema_json
        { "sql_type" => sql_type, "type" => type, "limit" => limit, "precision" => precision, "scale" => scale }
      end
    end

    # column.rb: an abstract representation of a column in a table.
    class Column
      attr_reader :name, :default, :sql_type_metadata, :null, :default_function, :collation, :comment

      # Rails' signature. `cast_type_refusal:` (this runtime's) names why
      # the column has no cast type object: `cast_type` raises with it.
      def initialize(name, cast_type, default, sql_type_metadata = nil, null = true, default_function = nil, collation: nil, comment: nil, cast_type_refusal: nil, **)
        @name = name.freeze
        @cast_type = cast_type
        @cast_type_refusal = cast_type_refusal
        @sql_type_metadata = sql_type_metadata
        @null = null
        @default = default
        @default_function = default_function
        @collation = collation
        @comment = comment
      end

      def cast_type
        if @cast_type.nil? && @cast_type_refusal
          raise NotImplementedError, "roundhouse: error[framework_feature_not_ported]: Column#cast_type for `#{@name}`: #{@cast_type_refusal}"
        end
        @cast_type
      end

      # `delegate :precision, :scale, :limit, :type, :sql_type, to:
      # :sql_type_metadata, allow_nil: true`.
      def precision = sql_type_metadata&.precision
      def scale = sql_type_metadata&.scale
      def limit = sql_type_metadata&.limit
      def type = sql_type_metadata&.type
      def sql_type = sql_type_metadata&.sql_type

      def has_default?
        !default.nil? || default_function
      end

      def bigint?
        /\Abigint\b/.match?(sql_type)
      end

      def human_name
        Base.human_attribute_name(@name)
      end

      def init_with(coder)
        @name = coder["name"]
        @cast_type = coder["cast_type"]
        @sql_type_metadata = coder["sql_type_metadata"]
        @null = coder["null"]
        @default = coder["default"]
        @default_function = coder["default_function"]
        @collation = coder["collation"]
        @comment = coder["comment"]
      end

      def encode_with(coder)
        coder["name"] = @name
        coder["cast_type"] = cast_type
        coder["sql_type_metadata"] = @sql_type_metadata
        coder["null"] = @null
        coder["default"] = @default
        coder["default_function"] = @default_function
        coder["collation"] = @collation
        coder["comment"] = @comment
      end

      def auto_incremented_by_db?
        false
      end

      def auto_populated_on_insert?
        auto_incremented_by_db? || default_function
      end

      def auto_populated_on_update?
        virtual?
      end

      def ==(other)
        other.is_a?(Column) &&
          name == other.name &&
          cast_type_for_comparison == other.cast_type_for_comparison &&
          default == other.default &&
          sql_type_metadata == other.sql_type_metadata &&
          null == other.null &&
          default_function == other.default_function &&
          collation == other.collation &&
          comment == other.comment
      end
      alias :eql? :==

      def hash
        [Column, @name, @name.encoding, @cast_type, @default, @sql_type_metadata, @null, @default_function, @collation, @comment].hash
      end

      def virtual?
        false
      end

      protected
        # The cast type as Rails' == compares it, without raising for a
        # column whose type object this runtime does not build.
        def cast_type_for_comparison
          @cast_type
        end
    end

    class NullColumn < Column
      def initialize(name, **)
        super(name, nil, nil)
      end
    end

    # abstract/schema_definitions.rb's IndexDefinition: a table's index as
    # `indexes` reads it from the catalog.
    class IndexDefinition
      attr_reader :table, :name, :unique, :columns, :lengths, :orders, :opclasses, :where, :type, :using, :include, :nulls_not_distinct, :comment

      def initialize(table, name, unique = false, columns = [], lengths: {}, orders: {}, opclasses: {}, where: nil, type: nil, using: nil, include: nil, nulls_not_distinct: nil, comment: nil, valid: true)
        @table = table
        @name = name
        @unique = unique
        @columns = columns
        @lengths = concise_options(lengths)
        @orders = concise_options(orders)
        @opclasses = concise_options(opclasses)
        @where = where
        @type = type
        @using = using
        @include = include
        @nulls_not_distinct = nulls_not_distinct
        @comment = comment
        @valid = valid
      end

      def valid?
        @valid
      end

      private

      # One value shared by every column is that value.
      def concise_options(options)
        if columns.size == options.size && options.values.uniq.size == 1
          options.values.first
        else
          options
        end
      end
    end

    module PostgreSQL
      # postgresql/type_metadata.rb: the SqlTypeMetadata with the
      # column's type OID and modifier.
      class TypeMetadata < DelegateClass(SqlTypeMetadata)
        attr_reader :oid, :fmod

        def initialize(type_metadata, oid: nil, fmod: nil)
          super(type_metadata)
          @oid = oid
          @fmod = fmod
        end

        def ==(other)
          other.is_a?(TypeMetadata) &&
            __getobj__ == other.__getobj__ &&
            oid == other.oid &&
            fmod == other.fmod
        end
        alias eql? ==

        def hash
          [TypeMetadata, __getobj__, @oid, @fmod].hash
        end

        def as_schema_json
          { "type_metadata" => __getobj__, "oid" => oid, "fmod" => fmod }
        end
      end

      # postgresql/column.rb.
      class Column < ConnectionAdapters::Column # :nodoc:
        def initialize(*, serial: nil, identity: nil, generated: nil, **)
          super
          @serial = serial
          @identity = identity
          @generated = generated
        end

        def oid = sql_type_metadata.oid
        def fmod = sql_type_metadata.fmod

        def identity?
          @identity
        end

        def serial?
          @serial
        end

        def auto_incremented_by_db?
          serial? || identity?
        end

        def virtual?
          !@generated.nil? && !@generated.empty?
        end

        def virtual_stored?
          @generated == "s"
        end

        def has_default?
          super && !virtual?
        end

        def array
          sql_type_metadata.sql_type.end_with?("[]")
        end
        alias :array? :array

        def enum?
          type == :enum
        end

        def sql_type
          super.delete_suffix("[]")
        end

        def init_with(coder)
          @serial = coder["serial"]
          @identity = coder["identity"]
          @generated = coder["generated"]
          super
        end

        def encode_with(coder)
          coder["serial"] = @serial
          coder["identity"] = @identity
          coder["generated"] = @generated
          super
        end

        def ==(other)
          other.is_a?(Column) &&
            super &&
            identity? == other.identity? &&
            serial? == other.serial? &&
            generated == other.generated
        end
        alias :eql? :==

        def hash
          [Column, super, @identity, @serial, @generated].hash
        end

        protected
          attr_reader :generated
      end
    end
    PostgreSQLColumn = PostgreSQL::Column # :nodoc:
    PostgreSQLTypeMetadata = PostgreSQL::TypeMetadata

    class AbstractAdapter < Connection
      ADAPTER_NAME = "Abstract"

      include Db::ConnectionLifecycle

      # abstract_adapter.rb:46, `include ActiveSupport::Callbacks` and
      # `define_callbacks :checkout, :checkin`: Rails runs the chains when
      # its pool checks a connection out (`clean!`) and in (`expire`). This
      # runtime's pool (Db.checkout / Db.checkin_to) runs neither, so a
      # callback set on them would never run: refused by name instead.
      class << self
        def set_callback(name, *filter_list, &block)
          raise NotImplementedError, "roundhouse: error[framework_feature_not_ported]: " \
            "#{self.name}.set_callback(#{name.inspect}, ...): the adapter's checkout/checkin callbacks " \
            "(AbstractAdapter `define_callbacks :checkout, :checkin`) are not run by this runtime's connection pool"
        end

        def skip_callback(name, *filter_list, &block)
          raise NotImplementedError, "roundhouse: error[framework_feature_not_ported]: " \
            "#{self.name}.skip_callback(#{name.inspect}, ...): the adapter's checkout/checkin callbacks " \
            "(AbstractAdapter `define_callbacks :checkout, :checkin`) are not run by this runtime's connection pool"
        end
      end

      # Returns the human-readable name of the adapter (Rails:
      # `self.class::ADAPTER_NAME`).
      def adapter_name
        self.class::ADAPTER_NAME
      end

      # AbstractAdapter's feature defaults the runtime reads; the
      # PostgreSQL adapter overrides them as Rails' does.
      def supports_savepoints?
        false
      end

      def supports_lazy_transactions?
        false
      end

      def supports_restart_db_transaction?
        false
      end

      # abstract_adapter.rb: INSERT ... RETURNING is opt-in per adapter.
      def supports_insert_returning?
        false
      end

      # abstract/database_statements.rb query_command: a statement without
      # rows on this adapter's session, recorded and instrumented as every
      # statement is.
      def query_command(sql, name = nil, materialize_transactions: true) # :nodoc:
        Db.record_query(sql)
        Db.context[:cache]&.clear
        result = raw_execute(sql, name, [], materialize_transactions: materialize_transactions) { |conn| perform_query(conn, sql) }
        nil
      ensure
        result&.clear
      end

      # abstract/database_statements.rb query_all / query_rows /
      # query_value: rows of a statement that reads, on this adapter's
      # session, named "SCHEMA" unless told otherwise and materializing no
      # lazy transaction. Rows are Arrays decoded as postgresql_adapter.rb
      # add_pg_decoders decodes them: integers, oids, floats, numerics and
      # booleans become Ruby values and every other type (name, "char",
      # regproc, text, ...) stays its text, without the warning
      # PG::BasicTypeMapForResults (the session's general result map)
      # gives an unmapped OID.
      def query_rows(sql, name = "SCHEMA", materialize_transactions: false) # :nodoc:
        result = raw_execute(sql, name, [], materialize_transactions: materialize_transactions) { |conn| perform_query(conn, sql) }
        result.type_map = AbstractAdapter.schema_result_type_map
        result.values
      ensure
        result&.clear
      end

      # The add_pg_decoders coders for the built-in scalar OIDs (fixed in
      # pg_type.dat on every server version), shared by every adapter.
      def self.schema_result_type_map # :nodoc:
        @schema_result_type_map ||= begin
          map = PG::TypeMapByOid.new
          { "int2" => [21, PG::TextDecoder::Integer], "int4" => [23, PG::TextDecoder::Integer],
            "int8" => [20, PG::TextDecoder::Integer], "oid" => [26, PG::TextDecoder::Integer],
            "float4" => [700, PG::TextDecoder::Float], "float8" => [701, PG::TextDecoder::Float],
            "numeric" => [1700, PG::TextDecoder::Numeric], "bool" => [16, PG::TextDecoder::Boolean] }.each do |type_name, (oid, coder)|
            map.add_coder(coder.new(oid: oid, name: type_name))
          end
          map
        end
      end

      def query_value(sql, name = "SCHEMA", materialize_transactions: false) # :nodoc:
        row = query_rows(sql, name, materialize_transactions: materialize_transactions).first
        row && row.first
      end

      # abstract/schema_statements.rb: the table's columns, in the order
      # the database keeps them.
      def columns(table_name)
        table_name = table_name.to_s
        definitions = column_definitions(table_name)
        definitions.map do |field|
          new_column_from_field(table_name, field, definitions)
        end
      end

      # abstract_adapter.rb: the schema cache of the pool this adapter
      # was checked out of (runtime/active_record_connection_handling.rb).
      def schema_cache
        current_pool = pool
        unless current_pool
          raise NotImplementedError, "roundhouse: error[framework_feature_not_ported]: #{self.class.name}#schema_cache: " \
            "this adapter belongs to no connection pool, and a lone connection's schema cache is not ported"
        end
        current_pool.schema_cache
      end

      private
        def column_definitions(table_name)
          raise NotImplementedError, "#{self.class.name}#column_definitions"
        end
      public
    end

    class PostgreSQLAdapter < AbstractAdapter
      ADAPTER_NAME = "PostgreSQL"

      # activerecord 8.1.4 postgresql_adapter.rb:105,
      # `class_attribute :create_unlogged_tables, default: false`: class
      # reader/writer/predicate inherited by subclasses until one assigns,
      # and the instance reader/predicate. Rails reads it in
      # PostgreSQL::TableDefinition#initialize (schema_definitions.rb:241)
      # for `create_table`. This runtime creates tables only from the
      # compiled schema (Schema.statements), never through create_table,
      # so the value is held and readable but changes no DDL. The
      # per-instance writer is not ported.
      class << self
        attr_writer :create_unlogged_tables

        def create_unlogged_tables
          return @create_unlogged_tables if instance_variable_defined?(:@create_unlogged_tables)
          superclass.respond_to?(:create_unlogged_tables) ? superclass.create_unlogged_tables : false
        end

        def create_unlogged_tables?
          !!create_unlogged_tables
        end

        # postgresql_adapter.rb initialize_type_map (and the instance's
        # time / timestamp registrations), by registered type name: the
        # cast type a column of that type gets, as its type, limit,
        # precision and scale, the Rails class, and why this runtime does
        # not build that class (nil where the attribute type port does).
        # nil for a name Rails registers no type under.
        def pg_named_cast(typname, fmod, sql_type) # :nodoc:
          case typname
          when "int2" then PgCast.new(:integer, 2, nil, nil, "Type::Integer", { limit: 2 }, nil)
          when "int4" then PgCast.new(:integer, 4, nil, nil, "Type::Integer", { limit: 4 }, nil)
          when "int8" then PgCast.new(:integer, 8, nil, nil, "Type::Integer", { limit: 8 }, nil)
          when "oid" then pg_unported_cast(:oid, "OID::Oid")
          when "float4" then PgCast.new(:float, 24, nil, nil, "Type::Float", { limit: 24 }, nil)
          when "float8" then PgCast.new(:float, nil, nil, nil, "Type::Float", {}, nil)
          when "text" then PgCast.new(:text, nil, nil, nil, "Type::Text", {}, nil)
          when "varchar", "char", "name", "bpchar"
            limit = extract_limit(sql_type)
            PgCast.new(:string, limit, nil, nil, "Type::String", { limit: limit }, nil)
          when "bool" then PgCast.new(:boolean, nil, nil, nil, "Type::Boolean", {}, nil)
          when "bit" then pg_unported_cast(:bit, "OID::Bit", limit: extract_limit(sql_type))
          when "varbit" then pg_unported_cast(:bit_varying, "OID::BitVarying", limit: extract_limit(sql_type))
          when "date" then PgCast.new(:date, nil, nil, nil, "OID::Date", {}, nil)
          when "money" then pg_unported_cast(:money, "OID::Money", scale: 2)
          when "bytea" then PgCast.new(:binary, nil, nil, nil, "OID::Bytea", {}, nil)
          when "point" then pg_unported_cast(:point, "OID::Point")
          when "hstore" then pg_unported_cast(:hstore, "OID::Hstore")
          when "json" then PgCast.new(:json, nil, nil, nil, "Type::Json", {}, nil)
          when "jsonb" then PgCast.new(:jsonb, nil, nil, nil, "OID::Jsonb", {}, nil)
          when "cidr" then pg_unported_cast(:cidr, "OID::Cidr")
          when "inet" then pg_unported_cast(:inet, "OID::Inet")
          when "uuid" then PgCast.new(:uuid, nil, nil, nil, "OID::Uuid", {}, nil)
          when "xml" then pg_unported_cast(:xml, "OID::Xml")
          when "macaddr" then pg_unported_cast(:macaddr, "OID::Macaddr")
          when "tsvector", "citext", "ltree", "line", "lseg", "box", "path", "polygon", "circle"
            pg_unported_cast(typname.to_sym, "OID::SpecializedString")
          when "numeric"
            precision = extract_precision(sql_type)
            scale = extract_scale(sql_type)
            # places after decimal = fmod - 4 & 0xffff
            if fmod && (fmod - 4 & 0xffff).zero?
              PgCast.new(:decimal, nil, precision, nil, "Type::DecimalWithoutScale", { precision: precision }, nil)
            else
              PgCast.new(:decimal, nil, precision, scale, "OID::Decimal", { precision: precision, scale: scale }, nil)
            end
          when "interval" then pg_unported_cast(:interval, "OID::Interval", precision: extract_precision(sql_type))
          when "time", "timetz"
            precision = fmod == -1 ? 6 : fmod
            PgCast.new(:time, nil, precision, nil, "Type::Time", { precision: precision }, nil)
          when "timestamp"
            precision = fmod == -1 ? 6 : fmod
            # OID::Timestamp#type: :datetime while `datetime_type` is
            # :timestamp, which the tree does not change.
            PgCast.new(:datetime, nil, precision, nil, "OID::Timestamp", { precision: precision }, nil)
          when "timestamptz" then pg_unported_cast(:timestamptz, "OID::TimestampWithTimeZone", precision: fmod == -1 ? 6 : fmod)
          end
        end

        # The attribute type port's object for a cast type it carries; nil
        # where the port is not in the tree.
        def pg_cast_object(rails_class, args) # :nodoc:
          return nil unless defined?(::ActiveRecord::Type::Text) && defined?(::ActiveRecord::ConnectionAdapters::PostgreSQL::OID::Timestamp)

          oid = ::ActiveRecord::ConnectionAdapters::PostgreSQL::OID
          case rails_class
          when "Type::Integer" then ::ActiveRecord::Type::Integer.new(**args)
          when "Type::Float" then ::ActiveRecord::Type::Float.new(**args)
          when "Type::Text" then ::ActiveRecord::Type::Text.new
          when "Type::String" then ::ActiveRecord::Type::String.new(**args)
          when "Type::Boolean" then ::ActiveRecord::Type::Boolean.new
          when "Type::Value" then ::ActiveModel::Type.default_value
          when "OID::Date" then oid::Date.new
          when "OID::Bytea" then oid::Bytea.new
          when "OID::Uuid" then oid::Uuid.new
          when "OID::Decimal" then oid::Decimal.new(**args)
          when "Type::DecimalWithoutScale" then ::ActiveRecord::Type::DecimalWithoutScale.new(**args)
          when "OID::Timestamp" then oid::Timestamp.new(**args)
          when "Type::Time" then ::ActiveRecord::Type::Time.new(**args)
          when "Type::Json" then ::ActiveRecord::Type::Json.new
          when "OID::Jsonb" then oid::Jsonb.new
          end
        end

        private
          def pg_unported_cast(type, rails_class, limit: nil, precision: nil, scale: nil)
            PgCast.new(type, limit, precision, scale, rails_class, {}, "`#{rails_class}` is not ported")
          end

          # abstract_adapter.rb extract_limit / extract_precision /
          # extract_scale, over the column's format_type text.
          def extract_limit(sql_type)
            $1.to_i if sql_type =~ /\((.*)\)/
          end

          def extract_precision(sql_type)
            $1.to_i if sql_type =~ /\((\d+)(,\d+)?\)/
          end

          def extract_scale(sql_type)
            case sql_type
            when /\((\d+)\)/ then 0
            when /\((\d+)(,(\d+))\)/ then $3.to_i
            end
          end
      end

      def create_unlogged_tables
        self.class.create_unlogged_tables
      end

      def create_unlogged_tables?
        !!create_unlogged_tables
      end

      def quote(value)
        return Db.escape_bool(value) if value == true || value == false
        return quoted_binary(value) if value.is_a?(ActiveModel::Type::Binary::Data)
        super(value)
      end

      # postgresql/quoting.rb: a binary value as the hex literal
      # `escape_bytea` writes; the column's own type reads it as bytea.
      def quoted_binary(value) # :nodoc:
        "'\\x#{value.to_s.unpack1("H*")}'"
      end

      # postgresql/quoting.rb: an identifier, and a table name that may
      # be schema-qualified (postgresql/utils.rb's
      # extract_schema_qualified_name and Name#quoted).
      def quote_column_name(name) # :nodoc:
        PG::Connection.quote_ident(name.to_s)
      end

      def quote_table_name(name) # :nodoc:
        schema, table = name.to_s.scan(/[^".]+|"[^"]*"/)
        if table.nil?
          table = schema
          schema = nil
        end
        unquote = ->(identifier) { identifier && identifier.start_with?('"') ? identifier[1..-2] : identifier }
        schema = unquote.(schema)
        table = unquote.(table)
        if schema
          PG::Connection.quote_ident(schema) << "." << PG::Connection.quote_ident(table)
        else
          PG::Connection.quote_ident(table)
        end
      end

      # postgresql/schema_statements.rb's data_source_exists? over
      # data_source_sql: a table, view, materialized view, partitioned or
      # foreign table of that name in the search path (or the named
      # schema).
      def data_source_exists?(name)
        schema, table = name.to_s.scan(/[^".]+|"[^"]*"/).map { |part| part.delete('"') }
        if table.nil?
          table = schema
          schema = nil
        end
        scope = schema ? "n.nspname = #{quote(schema)}" : "n.nspname = ANY (current_schemas(false))"
        sql = "SELECT c.relname FROM pg_class c LEFT JOIN pg_namespace n ON n.oid = c.relnamespace " \
              "WHERE #{scope} AND c.relkind IN ('r','v','m','p','f') AND c.relname = #{quote(table)}"
        !query_value(sql, "SCHEMA").nil?
      end

      # postgresql/schema_statements.rb's primary_keys over
      # fetch_primary_keys: the table's primary key columns in key order
      # (one table name answers its list, an Array of names a Hash of
      # lists), from its primary key constraint.
      def primary_keys(table_name) # :nodoc:
        names = Array(table_name).map(&:to_s)
        result = names.to_h do |name|
          rows = query_rows(<<~SQL, "SCHEMA")
            SELECT a.attname
              FROM pg_constraint c
              JOIN pg_attribute a ON a.attrelid = c.conrelid AND a.attnum = ANY(c.conkey)
             WHERE c.contype = 'p' AND c.conrelid = #{quote(quote_table_name(name))}::regclass
             ORDER BY array_position(c.conkey, a.attnum)
          SQL
          [name, rows.map(&:first)]
        end
        table_name.is_a?(Array) ? result : result[table_name.to_s]
      end

      # abstract/schema_statements.rb's primary_key: the sole key column,
      # or the list of a composite key.
      def primary_key(table_name)
        pk = primary_keys(table_name)
        pk = pk.first unless pk.size > 1
        pk
      end

      # postgresql/schema_statements.rb's indexes: the table's indexes
      # (the primary key's excluded) from pg_index, each parsed from its
      # `pg_get_indexdef` as Rails does, ordered by index name.
      def indexes(table_name) # :nodoc:
        schema, table = table_name.to_s.scan(/[^".]+|"[^"]*"/).map { |part| part.delete('"') }
        if table.nil?
          table = schema
          schema = nil
        end
        scope = schema ? "n.nspname = #{quote(schema)}" : "n.nspname = ANY (current_schemas(false))"
        rows = query_rows(<<~SQL, "SCHEMA")
          SELECT distinct i.relname, d.indisunique, d.indkey, pg_get_indexdef(d.indexrelid), t.oid,
                          pg_catalog.obj_description(i.oid, 'pg_class') AS comment, d.indisvalid
          FROM pg_class t
          INNER JOIN pg_index d ON t.oid = d.indrelid
          INNER JOIN pg_class i ON d.indexrelid = i.oid
          LEFT JOIN pg_namespace n ON n.oid = t.relnamespace
          WHERE i.relkind IN ('i', 'I')
            AND d.indisprimary = 'f'
            AND t.relname = #{quote(table)}
            AND #{scope}
          ORDER BY i.relname
        SQL
        rows.map do |index_name, unique, indkey, inddef, oid, comment, valid|
          indkey = indkey.split(" ").map(&:to_i)
          using, expressions, include, nulls_not_distinct, where =
            inddef.scan(/ USING (\w+?) \((.+?)\)(?: INCLUDE \((.+?)\))?( NULLS NOT DISTINCT)?(?: WHERE (.+))?\z/m).flatten
          orders = {}
          opclasses = {}
          include_columns = include ? include.split(",").map { |c| c.strip.delete('"').split(".").last } : []
          if indkey.include?(0)
            columns = expressions
          else
            names = query_rows(<<~SQL, "SCHEMA").to_h
              SELECT a.attnum, a.attname FROM pg_attribute a WHERE a.attrelid = #{oid.to_i} AND a.attnum IN (#{indkey.join(",")})
            SQL
            columns = names.values_at(*indkey).compact
            # Only DESC is spelled out (ASC is the default), as are non-default operator classes.
            expressions.scan(/(?<column>\w+)"?\s?(?<opclass>\w+_ops(_\w+)?)?\s?(?<desc>DESC)?\s?(?<nulls>NULLS (?:FIRST|LAST))?/).each do |column, opclass, desc, nulls|
              opclasses[column] = opclass.to_sym if opclass
              if nulls
                orders[column] = [desc, nulls].compact.join(" ")
              elsif desc
                orders[column] = :desc
              end
            end
          end
          IndexDefinition.new(
            table_name.to_s, index_name, unique, columns,
            orders: orders, opclasses: opclasses, where: where, using: using.to_sym,
            include: include_columns.empty? ? nil : include_columns,
            nulls_not_distinct: !nulls_not_distinct.nil? && !nulls_not_distinct.empty?,
            comment: comment.nil? || comment.empty? ? nil : comment, valid: valid
          )
        end
      end

      def supports_identity_columns? # :nodoc:
        true
      end

      def supports_virtual_columns?
        database_version >= 12_00_00 # >= 12.0
      end

      def max_identifier_length
        @max_identifier_length ||= query_value("SHOW max_identifier_length").to_i
      end

      # postgresql/database_statements.rb: the transaction statements, on
      # this adapter's own session, named "TRANSACTION". BEGIN does not
      # materialize the lazy transactions it is materializing; COMMIT and
      # ROLLBACK do (Rails' `materialize_transactions: true`).
      def begin_db_transaction # :nodoc:
        query_command("BEGIN", "TRANSACTION", materialize_transactions: false)
      end

      def begin_isolated_db_transaction(isolation) # :nodoc:
        query_command("BEGIN ISOLATION LEVEL #{transaction_isolation_levels.fetch(isolation)}", "TRANSACTION", materialize_transactions: false)
      end

      def commit_db_transaction # :nodoc:
        query_command("COMMIT", "TRANSACTION", materialize_transactions: true)
      end

      def exec_rollback_db_transaction # :nodoc:
        cancel_any_running_query
        query_command("ROLLBACK", "TRANSACTION", materialize_transactions: true)
      end

      def exec_restart_db_transaction # :nodoc:
        cancel_any_running_query
        query_command("ROLLBACK AND CHAIN", "TRANSACTION", materialize_transactions: true)
      end

      private
        # postgresql_adapter.rb column_definitions: one row per live
        # column of the table, in attnum order.
        def column_definitions(table_name)
          query_rows(<<~SQL)
              SELECT a.attname, format_type(a.atttypid, a.atttypmod),
                     pg_get_expr(d.adbin, d.adrelid), a.attnotnull, a.atttypid, a.atttypmod,
                     c.collname, col_description(a.attrelid, a.attnum) AS comment,
                     #{supports_identity_columns? ? 'attidentity' : quote('')} AS identity,
                     #{supports_virtual_columns? ? 'attgenerated' : quote('')} as attgenerated
                FROM pg_attribute a
                LEFT JOIN pg_attrdef d ON a.attrelid = d.adrelid AND a.attnum = d.adnum
                LEFT JOIN pg_type t ON a.atttypid = t.oid
                LEFT JOIN pg_collation c ON a.attcollation = c.oid AND a.attcollation <> t.typcollation
               WHERE a.attrelid = #{quote(quote_table_name(table_name))}::regclass
                 AND a.attnum > 0 AND NOT a.attisdropped
               ORDER BY a.attnum
          SQL
        end

        # postgresql/schema_statements.rb new_column_from_field.
        def new_column_from_field(table_name, field, _definitions)
          column_name, type, default, notnull, oid, fmod, collation, comment, identity, attgenerated = field
          notnull = notnull == true || notnull == "t"
          cast = pg_cast_type(oid.to_i, fmod.to_i, column_name, type)
          cast_object = cast.cast_type
          type_metadata = PostgreSQL::TypeMetadata.new(
            SqlTypeMetadata.new(sql_type: type, type: cast.type, limit: cast.limit, precision: cast.precision, scale: cast.scale),
            oid: oid.to_i, fmod: fmod.to_i
          )
          default_value = extract_value_from_default(default)

          if !attgenerated.nil? && !attgenerated.to_s.empty?
            default_function = default
          else
            default_function = extract_default_function(default_value, default)
          end

          if match = default_function&.match(/\Anextval\('"?(?<sequence_name>.+_(?<suffix>seq\d*))"?'::regclass\)\z/)
            serial = sequence_name_from_parts(table_name, column_name, match[:suffix]) == match[:sequence_name]
          end

          PostgreSQL::Column.new(
            column_name,
            cast_object,
            default_value,
            type_metadata,
            !notnull,
            default_function,
            collation: collation,
            comment: comment.nil? || comment.empty? ? nil : comment,
            serial: serial,
            identity: identity.nil? || identity.empty? ? nil : identity,
            generated: attgenerated,
            cast_type_refusal: cast.cast_type_refusal(cast_object)
          )
        end

        # postgresql/schema_statements.rb sequence_name_from_parts.
        def sequence_name_from_parts(table_name, column_name, suffix)
          over_length = [table_name, column_name, suffix].sum(&:length) + 2 - max_identifier_length

          if over_length > 0
            column_name_length = [(max_identifier_length - suffix.length - 2) / 2, column_name.length].min
            over_length -= column_name.length - column_name_length
            column_name = column_name[0, column_name_length - [over_length, 0].min]
          end

          if over_length > 0
            table_name = table_name[0, table_name.length - over_length]
          end

          "#{table_name}_#{column_name}_#{suffix}"
        end

        # postgresql_adapter.rb extract_value_from_default.
        def extract_value_from_default(default)
          case default
            # Quoted types
          when /\A[(B]?'(.*)'.*::"?([\w. ]+)"?(?:\[\])?\z/m
            # The default 'now'::date is CURRENT_DATE
            if $1 == "now" && $2 == "date"
              nil
            else
              $1.gsub("''", "'")
            end
            # Boolean types
          when "true", "false"
            default
            # Numeric types
          when /\A\(?(-?\d+(\.\d*)?)\)?(::bigint)?\z/
            $1
            # Object identifier types
          when /\A-?\d+\z/
            $1
          else
            # Anything else is blank, some user type, or some function
            # and we can't know the value of that, so return nil.
            nil
          end
        end

        def extract_default_function(default_value, default)
          default if has_default_function?(default_value, default)
        end

        def has_default_function?(default_value, default)
          !default_value && %r{\w+\(.*\)|\(.*\)::\w+|CURRENT_DATE|CURRENT_TIMESTAMP}.match?(default)
        end

        # The cast type Rails' type map answers for a column (get_oid_type):
        # its type, limit, precision and scale, and the type object where
        # the attribute type port carries its class.
        PgCast = Struct.new(:type, :limit, :precision, :scale, :rails_class, :args, :refusal) do
          def cast_type
            return nil if refusal
            PostgreSQLAdapter.pg_cast_object(rails_class, args)
          end

          # Why `object` (this cast's #cast_type) is nil, if it is.
          def cast_type_refusal(object)
            return refusal if refusal
            return nil unless object.nil?
            "`#{rails_class}` comes from the attribute type port (runtime/active_model_type.rb), which this tree does not carry"
          end
        end
        private_constant :PgCast

        # postgresql_adapter.rb get_oid_type over the type map: the OID's
        # pg_type row (load_additional_types; a domain, range or array is
        # followed to the type it is built on) and the cast type
        # initialize_type_map registers for the type's name.
        def pg_cast_type(oid, fmod, column_name, sql_type)
          pg_load_types([oid])
          cast = pg_cast_for_oid(oid, fmod, sql_type)
          return cast if cast

          # register_unknown_oid_type
          @rh_unknown_oids ||= {}
          unless @rh_unknown_oids[oid]
            warn "unknown OID #{oid}: failed to recognize type of '#{column_name}'. It will be treated as String."
            @rh_unknown_oids[oid] = true
          end
          PgCast.new(nil, nil, nil, nil, "Type::Value", {}, nil)
        end

        def pg_types
          @rh_pg_types ||= {}
        end

        # load_additional_types / load_types_queries: the pg_type rows of
        # the OIDs not yet known, and of the types they are built on.
        def pg_load_types(oids)
          pending = oids.map(&:to_i).uniq.reject { |oid| pg_types.key?(oid) }
          until pending.empty?
            rows = query_rows("SELECT t.oid, t.typname, t.typelem, t.typdelim, t.typinput, r.rngsubtype, t.typtype, t.typbasetype " \
              "FROM pg_type AS t LEFT JOIN pg_range AS r ON oid = rngtypid\nWHERE t.oid IN (#{pending.join(", ")})")
            pending.each { |oid| pg_types[oid] = nil }
            rows.each do |oid, typname, typelem, typdelim, typinput, rngsubtype, typtype, typbasetype|
              pg_types[oid.to_i] = {
                typname: typname.to_s, typelem: typelem.to_i, typdelim: typdelim.to_s, typinput: typinput.to_s,
                rngsubtype: rngsubtype.to_i, typtype: typtype.to_s, typbasetype: typbasetype.to_i
              }
            end
            pending = pg_types.values_at(*pending).compact.flat_map do |row|
              case row[:typtype]
              when "r" then [row[:rngsubtype]]
              when "d" then [row[:typbasetype]]
              else row[:typelem] != 0 ? [row[:typelem]] : []
              end
            end.uniq.reject { |oid| oid == 0 || pg_types.key?(oid) }
          end
        end

        # type_map_initializer.rb run: a registered name answers its own
        # type; a range, enum, domain or array (or vector) is built from
        # its row.
        def pg_cast_for_oid(oid, fmod, sql_type)
          row = pg_types[oid]
          return nil unless row

          named = PostgreSQLAdapter.pg_named_cast(row[:typname], fmod, sql_type)
          return named if named

          case row[:typtype]
          when "r"
            subtype = pg_cast_for_oid(row[:rngsubtype], fmod, sql_type)
            subtype && PgCast.new(row[:typname].to_sym, nil, nil, nil, "OID::Range", {}, "PostgreSQL range type `#{row[:typname]}` (OID::Range) is not ported")
          when "e"
            PgCast.new(:enum, nil, nil, nil, "OID::Enum", {}, "PostgreSQL enum type `#{row[:typname]}` (OID::Enum) is not ported")
          when "d"
            pg_cast_for_oid(row[:typbasetype], fmod, sql_type)
          else
            return nil if row[:typelem] == 0
            subtype = pg_cast_for_oid(row[:typelem], fmod, sql_type)
            return nil unless subtype
            if row[:typinput] == "array_in"
              PgCast.new(subtype.type, subtype.limit, subtype.precision, subtype.scale, "OID::Array", {}, "PostgreSQL array type `#{row[:typname]}` (OID::Array) is not ported")
            else
              PgCast.new(nil, nil, nil, nil, "OID::Vector", {}, "PostgreSQL vector type `#{row[:typname]}` (OID::Vector) is not ported")
            end
          end
        end

        IDLE_TRANSACTION_STATUSES = [PG::PQTRANS_IDLE, PG::PQTRANS_INTRANS, PG::PQTRANS_INERROR].freeze
        private_constant :IDLE_TRANSACTION_STATUSES

        def cancel_any_running_query
          return if @raw_connection.nil? || IDLE_TRANSACTION_STATUSES.include?(@raw_connection.transaction_status)

          # pg < 1.6.0 cannot obtain the backend key under libpq >= 18.
          if !(PG.library_version >= 18_00_00 && Gem::Version.new(PG::VERSION) < Gem::Version.new("1.6.0"))
            @raw_connection.cancel
          end
          @raw_connection.block
        rescue PG::Error
        end
      public
    end
  end

  Db.adapter_class = ConnectionAdapters::PostgreSQLAdapter
end

# Base.connection, connection_pool, connects_to, connected_to, ...:
# Rails' connection handling, one pool per configuration.
require_relative "active_record_connection_handling"

module ActiveRecord
  # A boolean over an integer column with no declared attribute type
  # (`where(is_not_deleted: true)` on a smallint) is the Integer type's
  # 1/0, as for Rails' QueryAttribute: PostgreSQL's literal TRUE does not
  # compare with a smallint.
  module IntegerColumnBooleans
    def _attribute_for_database(name, value)
      if (value == true || value == false) && !_declared_attribute_types[name]
        spec = schema_column_types[name.to_s]
        return value ? 1 : 0 if spec && spec[0] == :integer
      end
      super
    end
  end
  Base.singleton_class.prepend(IntegerColumnBooleans)

  # A json/jsonb column the compiled model retypes only for a `default:`
  # (`attribute :signal_value, default: {}`) keeps the column's own type, as
  # Rails' default-only declaration does: its writer casts, hydration
  # parses and every bind serializes through Type::Json. Without it the
  # compiled path passes the Hash through and the insert binds
  # `Hash#to_s`. Only the names the compiler routed to the dynamic path
  # (`_retypable_attribute_names`) are touched; any other json column is
  # still the compiled string.
  module JsonDefaultAttributeType
    def _attribute_cast(name, value)
      type = _json_default_attribute_type(name)
      return super if type.nil?
      type.assert_valid_value(value)
      type.cast(value)
    end

    def _attribute_deserialize(name, value)
      type = _json_default_attribute_type(name)
      type.nil? ? super : type.deserialize(value)
    end

    def _attribute_serialize(name, value)
      type = _json_default_attribute_type(name)
      type.nil? ? super : _bind_type_cast(name, type.serialize(value))
    end

    def _attribute_for_database(name, value)
      type = _json_default_attribute_type(name)
      type.nil? ? super : _bind_type_cast(name, type.serialize(type.cast(value)))
    end

    def _json_default_attribute_type(name)
      key = name.to_s
      return nil unless _retypable_attribute_names.include?(key)
      return nil if _declared_attribute_types[key]
      spec = begin
        schema_column_types[key]
      rescue NotImplementedError
        nil
      end
      return nil unless spec && (spec[0] == :json || spec[0] == :jsonb)
      type_for_attribute(key)
    end
    private :_json_default_attribute_type
  end
  Base.singleton_class.prepend(JsonDefaultAttributeType)

  # insert_all.rb's `on_duplicate: :skip` (Rails 7.2+) is `DO NOTHING`, and
  # `:update` the default assignment of every column. The shared
  # `_upsert_rows` takes the SET clause as SQL, so `:skip` is a write that
  # updates no column (an empty `update_only` leaves nothing to assign).
  module UpsertOnDuplicateSymbols
    def _upsert_rows(rows, unique_by, on_duplicate, update_only, returning, record_timestamps = nil)
      return super(rows, unique_by, nil, [], returning, record_timestamps) if on_duplicate == :skip
      return super(rows, unique_by, nil, update_only, returning, record_timestamps) if on_duplicate == :update
      super
    end
  end
  Base.singleton_class.prepend(UpsertOnDuplicateSymbols)
end
