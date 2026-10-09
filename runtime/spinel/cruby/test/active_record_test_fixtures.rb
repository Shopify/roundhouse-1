# ActiveRecord::TestFixtures, ported from the Rails native Core runs
# (Shopify/rails 5813559f4aed, 8.2.0.alpha,
# activerecord/lib/active_record/test_fixtures.rb, with
# ActiveRecord::FixtureSet.create_fixtures and its insert from
# fixtures.rb).
#
# setup_fixtures loads the class's fixture sets once per process, keyed
# by [fixture_table_names, fixture_paths, fixture_class_names] in
# `@@already_loaded_fixtures`. A transactional test then pins every
# writing pool of the connection handler to one connection, inside a
# non-joinable transaction begun before the test's setup callbacks, as
# it does for a pool established while the test runs (the
# `!connection.active_record` subscription). Teardown rolls each one back
# and returns the connection, then clears every active connection. When a
# pin was not clean (something committed or rolled the transaction back),
# the loaded-fixture cache is cleared, so the next test loads again. A
# test that is not transactional loads its fixtures before it runs and
# invalidates the cache after.
#
# FixtureSet.insert groups the sets by their model's connection pool
# (`model_class&.connection_pool`, the model resolved as safe_constantize
# does, else ActiveRecord::Base's). For each pool, in one
# `transaction(requires_new: true)` on a connection from that pool (the
# writer that owns the model), it DELETEs FROM every set's table, inserts
# the sets' rows, and then caps each key sequence at its column's max.
# The rows here are the test helper's `FixtureLoader.fixture_sets`: [set
# name, compiled fixture module, model, table]. Each module's
# `_fixtures_load!` hands its table rows to `insert_fixture_rows`: as in
# Rails' insert_fixtures_set, one multi-row INSERT with each value cast
# by its column's type, so no model is built and no writer, callback or
# validation runs.
#
# A set whose rows do not load is refused by name: this tree says
# `error[fixture_set_not_loaded]` once on stderr, records the set in
# `TestFixtures.fixture_sets_not_loaded`, and makes each of its fixture
# accessors raise FixtureSetNotLoaded. Each table's DELETE and each set's
# rows run in their own savepoint, so the other sets in the pool still
# load. Rails would raise the load error from every test's setup.
#
# The runner (minitest_runner.rb) calls the hooks below where Rails'
# before_setup and after_teardown run setup_fixtures and
# teardown_fixtures: before the test's setup, and after its teardown and
# after_teardown. Each test starts with fresh fixture accessors, as with
# Rails' per-test `@fixture_cache`. Rails' `fixture_sets` accessors
# (`shops(:snowdevil)`, `fixture(:shops, :snowdevil)`) answer through the
# set's compiled module, where the compiler did not lower the call to it.
#
# Every emitted test class loads every set. The ingest drops Core's
# `use_active_record_fixtures`, and with it the classes that load none,
# so the default `fixture_table_names` is `fixtures :all`. A class's
# `self.use_transactional_tests = <literal>` compiles to its class
# reader. `use_instantiated_fixtures` and `pre_loaded_fixtures` are
# refused when set. Rails' asynchronous-queries session is not ported:
# this runtime runs no query asynchronously.
#
# Active Record autoloads the module in any process, so the PostgreSQL
# boot (runtime/database_configuration.rb) requires it; the test helper
# includes it into TestBase, as rails/test_help.rb does.
require_relative "active_support_class_attribute"
require "date"
require "digest/sha1"
require "json"
require "yaml"
require "zlib"

module ActiveRecord
  module TestFixtures
    # A fixture accessor of a set this tree could not load.
    class FixtureSetNotLoaded < StandardError; end

    # ActiveRecord::Fixture::FixtureError: a row naming a column the
    # table does not have.
    class FixtureError < StandardError; end

    # ActiveRecord::FixtureSet::MAX_ID.
    FIXTURE_MAX_ID = 2**30 - 1

    # Digest::UUID::OID_NAMESPACE.
    FIXTURE_OID_NAMESPACE = ["6ba7b8129dad11d180b400c04fd430c8"].pack("H*").freeze

    # ActiveModel::Type::Boolean::FALSE_VALUES.
    FIXTURE_FALSE_VALUES = [false, 0, "0", :"0", "f", :f, "F", :F, "false", :false, "FALSE", :FALSE, "off", :off, "OFF", :OFF].freeze

    # ActiveRecord::ConnectionAdapters::PostgreSQL::OID::Uuid::ACCEPTABLE_UUID.
    FIXTURE_ACCEPTABLE_UUID = %r{\A(\{)?([a-fA-F0-9]{4}-?){8}(?(1)\}|)\z}

    # Rails' test/fixtures/, which rails/test_help.rb appends to
    # ActiveSupport::TestCase.fixture_paths: the tree's own (this file is
    # <tree>/runtime/active_record_test_fixtures.rb).
    FIXTURE_PATH = File.expand_path("../test/fixtures", __dir__) + "/"

    @fixture_set_cache = {}
    @fixture_sets_not_loaded = {}

    class << self
      # set name => "Class: message" for each set that did not load.
      attr_reader :fixture_sets_not_loaded

      # ActiveRecord::FixtureSet.reset_cache.
      def reset_cache
        @fixture_set_cache.clear
      end

      # The test helper's rows, by set name.
      def fixture_set_rows
        return {} unless defined?(::FixtureLoader) && ::FixtureLoader.respond_to?(:fixture_sets)
        @fixture_set_rows ||= ::FixtureLoader.fixture_sets.to_h { |set, mod, model, table| [set, [set, mod, model, table]] }
      end

      # ActiveRecord::FixtureSet.create_fixtures: inserts each named set
      # that is not cached yet, and answers the named sets' rows.
      def create_fixtures(fixture_set_names, class_names = {})
        fixture_set_names = Array(fixture_set_names).map(&:to_s)
        rows = fixture_set_rows
        fixture_set_names.each do |name|
          row = rows[name]
          unless row
            raise NotImplementedError, "roundhouse: fixture set `#{name}`: this tree compiled no fixture module for it"
          end
          class_name = class_names[name] || class_names[name.to_sym]
          class_name = class_name.name if class_name.is_a?(Module)
          if class_name && class_name.to_s.delete_prefix("::") != row[2]
            raise NotImplementedError,
              "roundhouse: set_fixture_class #{name}: #{class_name}: the fixture module compiled for `#{name}` loads #{row[2]}"
          end
        end

        to_read = fixture_set_names.reject { |name| @fixture_set_cache.key?(name) }
        insert(to_read.map { |name| rows.fetch(name) }) unless to_read.empty?
        to_read.each { |name| @fixture_set_cache[name] = rows.fetch(name) }
        fixture_set_names.to_h { |name| [name, @fixture_set_cache.fetch(name)] }
      end

      # Rails' per-test `@fixture_cache = {}`: the compiled accessors
      # memoize their record in the module, so each test clears them.
      def reset_fixture_accessors(rows)
        rows.each do |_set, mod, _model, _table|
          next unless Object.const_defined?(mod, false)
          fixtures = Object.const_get(mod, false)
          fixtures.instance_variables.each do |ivar|
            fixtures.instance_variable_set(ivar, nil) if ivar.start_with?("@__fx_", "@__fxe_")
          end
        end
      end

      # A compiled accessor's cache slot (the compiler's
      # `fixture_cache_ivar`): `@__fx_<label>`, or for a label that is not
      # all [A-Za-z0-9_], `@__fxe_` and an escape.
      def fixture_cache_ivar(label)
        label = label.to_s
        return :"@__fx_#{label}" if label.match?(/\A[A-Za-z0-9_]*\z/)
        slot = +"@__fxe_"
        label.each_char do |c|
          slot << (c.match?(/[A-Za-z0-9]/) ? c : (c == "_" ? "__" : "_#{c.ord.to_s(16)}_"))
        end
        slot.to_sym
      end

      # ActiveRecord::FixtureSet.identify.
      def identify(label, column_type = "integer")
        if column_type.to_s == "uuid"
          hash = Digest::SHA1.digest(FIXTURE_OID_NAMESPACE + label.to_s.b)
          ary = hash.unpack("NnnnnN")
          ary[2] = (ary[2] & 0x0FFF) | (5 << 12)
          ary[3] = (ary[3] & 0x3FFF) | 0x8000
          "%08x-%04x-%04x-%04x-%04x%08x" % ary
        else
          Zlib.crc32(label.to_s) % FIXTURE_MAX_ID
        end
      end

      # A reference an ERB tag names: the referenced row's id. Under
      # FasterFixtures an Integer is the id itself.
      def fixture_reference(value, column_type, integer_passes)
        return nil if value.nil? || value == false
        return value if integer_passes && value.is_a?(Integer)
        text = value.to_s
        if integer_passes && (md = text.match(/^(\S+)\s*\(([^\)]*)\)\s*$/))
          text = md[1]
        end
        identify(text, column_type)
      end

      # One set's rows, as Rails' insert_fixtures_set writes them
      # (build_fixture_sql): every row's timestamps the loader fills with
      # `now` when it omits them, a FixtureError for a key the table has no
      # column for, then one INSERT naming every column, DEFAULT where a
      # row omits one (a single row names only its own), each value cast
      # by its column's type (`type.serialize`, YAML for a Hash or Array
      # the cast leaves). `columns` is column => cast type, in table order.
      def insert_fixture_rows(table, columns, timestamps, rows)
        return nil if rows.empty?
        now = Time.now.utc.strftime("%Y-%m-%d %H:%M:%S")
        rows = rows.map do |row|
          row = row.dup
          timestamps.each { |column| row[column] = now unless row.key?(column) }
          unknown = row.keys - columns.keys
          unless unknown.empty?
            raise FixtureError, %(table "#{table}" has no columns named #{unknown.map(&:inspect).join(", ")}.)
          end
          row
        end
        names = columns.keys
        names = names.select { |name| rows.first.key?(name) } if rows.size == 1
        sql = +"INSERT INTO #{quote_table_name(table)} "
        if names.empty?
          sql << "DEFAULT VALUES"
        else
          values = rows.map do |row|
            "(" + names.map { |name| row.key?(name) ? fixture_sql_value(row[name], columns.fetch(name)) : "DEFAULT" }.join(", ") + ")"
          end
          sql << "(#{names.map { |name| quote_identifier(name) }.join(", ")}) VALUES #{values.join(", ")}"
        end
        Db.exec(sql)
        nil
      end

      private
        # ActiveRecord::FixtureSet.insert.
        def insert(rows)
          by_pool = {}
          rows.each do |row|
            next if @fixture_sets_not_loaded.key?(row[0])
            pool = attempt(row) do
              Object.const_get(row[1])
              fixture_model_pool(row[2])
            end
            (by_pool[pool] ||= []) << row if pool
          end

          by_pool.each do |pool, sets|
            begin
              pool.with_connection do |conn|
                tables = sets.map { |row| row[3] }.uniq
                conn.transaction(requires_new: true) do
                  # Each table's DELETE in its own savepoint: a table that
                  # cannot be cleared refuses only the sets loading it.
                  failed = {}
                  tables.each do |table|
                    begin
                      conn.transaction(requires_new: true) { Db.exec("DELETE FROM #{quote_table_name(table)}") }
                    rescue NoMemoryError, SignalException, SystemExit
                      raise
                    rescue Exception => error # rubocop:disable Lint/RescueException
                      failed[table] = error
                    end
                  end
                  sets.each do |row|
                    if (error = failed[row[3]])
                      not_loaded(row, error)
                      next
                    end
                    attempt(row) do
                      conn.transaction(requires_new: true) { Object.const_get(row[1])._fixtures_load! }
                    end
                  end
                end
                # Cap each table's key sequences at the column's max.
                tables.each do |table|
                  loaded = sets.find { |row| row[3] == table && !@fixture_sets_not_loaded.key?(row[0]) }
                  reset_pk_sequences(conn, loaded[0], table) if loaded
                end
              end
            rescue NoMemoryError, SignalException, SystemExit
              raise
            rescue Exception => error # rubocop:disable Lint/RescueException
              # The pool's DELETEs or its transaction failed: none of its
              # sets loaded.
              sets.each { |row| not_loaded(row, error) unless @fixture_sets_not_loaded.key?(row[0]) }
            end
          end
        end

        # The set's step, or its refusal: nil when it raised.
        def attempt(row)
          yield
        rescue NoMemoryError, SignalException, SystemExit
          raise
        rescue Exception => error # rubocop:disable Lint/RescueException
          not_loaded(row, error)
          nil
        end

        def not_loaded(row, error)
          set, mod, model, table = row
          reason = "#{error.class}: #{error.message.to_s.lines.first.to_s.chomp}"
          @fixture_sets_not_loaded[set] = reason
          site = (error.backtrace || []).first
          $stderr.puts "roundhouse: error[fixture_set_not_loaded]: fixture set `#{set}` (#{mod}, #{model}, table #{table}) " \
            "was not loaded: #{reason}#{site ? " (#{site})" : ""}"
          return unless Object.const_defined?(mod, false)
          fixtures = Object.const_get(mod, false)
          message = "fixture set `#{set}` was not loaded: #{reason}"
          (fixtures.singleton_methods(false) - [:_fixtures_load!]).each do |label|
            fixtures.define_singleton_method(label) { |*| raise FixtureSetNotLoaded, message }
          end
        end

        # The pool a set inserts through: its model's, the model resolved
        # as safe_constantize does (a model constant that does not exist
        # is no model; an error loading it is the set's), else
        # ActiveRecord::Base's.
        def fixture_model_pool(name)
          model = begin
            Object.const_get(name)
          rescue NameError => error
            raise if error.name && !(name.to_s.split("::").include?(error.name.to_s) || error.name.to_s == name.to_s)
            nil
          end
          model.respond_to?(:connection_pool) ? model.connection_pool : ActiveRecord::Base.connection_pool
        end

        # PostgreSQL's reset_pk_sequence! (SequenceReset), for each key
        # column of the table (and `id`, which FasterFixtures resets) that
        # draws from a sequence: set to max(column), or to its minimum,
        # not yet called, for an empty table. The set's rows stay loaded
        # when this fails; the error is named.
        def reset_pk_sequences(conn, set, table)
          regclass = quote_string(quote_table_name(table))
          listing = conn.select_value(
            "SELECT string_agg(a.attname || chr(9) || s.seq, chr(10)) FROM pg_attribute a " \
            "CROSS JOIN LATERAL (SELECT pg_get_serial_sequence(#{regclass}, a.attname) AS seq) s " \
            "WHERE a.attrelid = #{regclass}::regclass AND a.attnum > 0 AND NOT a.attisdropped AND s.seq IS NOT NULL " \
            "AND (a.attname = 'id' OR EXISTS (SELECT 1 FROM pg_index i WHERE i.indrelid = a.attrelid " \
            "AND i.indisprimary AND a.attnum = ANY(i.indkey)))"
          )
          listing.to_s.split("\n").each do |line|
            column, sequence = line.split("\t", 2)
            max = conn.select_value("SELECT MAX(#{quote_identifier(column)}) FROM #{quote_table_name(table)}")
            min = conn.select_value("SELECT seqmin FROM pg_sequence WHERE seqrelid = #{quote_string(sequence)}::regclass")
            conn.select_value("SELECT setval(#{quote_string(sequence)}, #{Integer(max || min)}, #{max ? true : false})")
          end
        rescue NoMemoryError, SignalException, SystemExit
          raise
        rescue Exception => error # rubocop:disable Lint/RescueException
          $stderr.puts "roundhouse: error[fixture_sequence_not_reset]: fixture set `#{set}` (table #{table}): " \
            "#{error.class}: #{error.message.to_s.lines.first.to_s.chomp}"
        end

        # An ERB value reaches the row as the object the tag produced;
        # Rails reads the text ERB wrote. Values YAML reads back as
        # themselves stay; any other object is its `to_s`.
        def fixture_erb_text(value)
          case value
          when nil, true, false, ::Numeric, ::String, ::Hash, ::Array then value
          else value.to_s
          end
        end

        # `type.serialize(value)` for the column's cast type, then the
        # adapter's quoting, as an SQL literal.
        def fixture_sql_value(value, type)
          value = fixture_erb_text(value)
          case type
          when "integer"
            int = case value
                  when true then 1
                  when false then 0
                  when ::String then value.match?(/\A\s*[+-]?\d/) ? value.to_i : nil
                  when ::Float then value.finite? ? value.to_i : nil
                  when ::Numeric then value.to_i
                  end
            int.nil? ? "NULL" : int.to_s
          when "float"
            float = case value
                    when true then 1.0
                    when false then 0.0
                    when "Infinity" then Float::INFINITY
                    when "-Infinity" then -Float::INFINITY
                    when "NaN" then Float::NAN
                    when ::Numeric then value.to_f
                    when ::String then value.to_f
                    end
            if float.nil? then "NULL"
            elsif float.nan? then "'NaN'"
            elsif float.infinite? then float.positive? ? "'Infinity'" : "'-Infinity'"
            else float.to_s
            end
          when "decimal"
            case value
            when nil then "NULL"
            when true then "1"
            when false then "0"
            when ::Float then value.finite? ? value.to_s : "NULL"
            when ::Numeric then quote_string(value.to_s)
            when ::String then quote_string(value[/\A\s*[+-]?(?:\d+(?:\.\d*)?|\.\d+)(?:[eE][+-]?\d+)?/].to_s.strip.then { |n| n.empty? ? "0" : n })
            else "0"
            end
          when "boolean"
            return "NULL" if value.nil? || value == ""
            FIXTURE_FALSE_VALUES.include?(value) ? "FALSE" : "TRUE"
          when "string"
            text = case value
                   when nil then nil
                   when true then "t"
                   when false then "f"
                   when ::Hash, ::Array then YAML.dump(value)
                   else value.to_s
                   end
            text.nil? ? "NULL" : quote_string(text)
          when "json", "jsonb"
            return "NULL" if value.nil?
            json = defined?(::ActiveSupport::JSON) && ::ActiveSupport::JSON.respond_to?(:encode) ? ::ActiveSupport::JSON.encode(value) : JSON.generate(value)
            quote_string(json)
          when "datetime"
            time = fixture_time(value)
            time ? quote_string(time.strftime("%Y-%m-%d %H:%M:%S.%6N")) : "NULL"
          when "date"
            date = fixture_date(value)
            date ? quote_string(date.strftime("%Y-%m-%d")) : "NULL"
          when "time"
            time = value.is_a?(::String) ? fixture_time("2000-01-01 " + value.sub(/\A\d{4}-\d\d-\d\d(?:T|\s)/, "")) : fixture_time(value)
            time ? quote_string(time.strftime("%H:%M:%S.%6N")) : "NULL"
          when "binary"
            return "NULL" if value.nil?
            bytes = case value
                    when ::Array then (value.pack("C*") rescue YAML.dump(value))
                    when ::Hash then YAML.dump(value)
                    else value.to_s
                    end
            "'\\x#{bytes.unpack1("H*")}'::bytea"
          when "uuid"
            text = value.to_s
            return "NULL" if value.nil? || !text.match?(FIXTURE_ACCEPTABLE_UUID)
            uuid = text.delete("{}-").downcase
            quote_string("#{uuid[0, 8]}-#{uuid[8, 4]}-#{uuid[12, 4]}-#{uuid[16, 4]}-#{uuid[20, 12]}")
          else
            value.nil? ? "NULL" : quote_string((value.is_a?(::Hash) || value.is_a?(::Array)) ? YAML.dump(value) : value.to_s)
          end
        end

        # ActiveModel::Type::DateTime#cast in UTC (default_timezone :utc):
        # a string through Date._parse, its offset applied.
        def fixture_time(value)
          case value
          when nil, "" then nil
          when ::Time then value.getutc
          when ::String
            parts = Date._parse(value)
            year, mon, mday = parts.values_at(:year, :mon, :mday)
            return nil if year.nil? || (year == 0 && mon == 0 && mday == 0)
            usec = parts[:sec_fraction] ? (parts[:sec_fraction] * 1_000_000).to_i : 0
            time = (Time.utc(year, mon, mday, parts[:hour], parts[:min], parts[:sec], usec) rescue nil)
            return nil unless time
            parts[:offset] && parts[:offset] != 0 ? time - parts[:offset] : time
          else
            value.respond_to?(:to_time) ? value.to_time.getutc : nil
          end
        end

        # ActiveModel::Type::Date#cast.
        def fixture_date(value)
          case value
          when nil, "" then nil
          when ::Date then value
          when ::Time then value.to_date
          when ::String
            year, mon, mday = Date._parse(value, false).values_at(:year, :mon, :mday)
            return nil if year.nil? || (year == 0 && mon == 0 && mday == 0)
            Date.new(year, mon, mday) rescue nil
          end
        end

        def quote_table_name(name)
          name.to_s.split(".").map { |part| quote_identifier(part) }.join(".")
        end

        def quote_identifier(name)
          %("#{name.gsub('"', '""')}")
        end

        def quote_string(value)
          "'#{value.to_s.gsub("'", "''")}'"
        end
    end

    module ClassMethods
      def skip_transactional_tests_for_database(database_name)
        use_transactional_tests_for_database(database_name, false)
      end

      def use_transactional_tests_for_database(database_name, enabled = true)
        self.database_transactions_config = database_transactions_config.merge(database_name => enabled)
      end

      def set_fixture_class(class_names = {})
        self.fixture_class_names = fixture_class_names.merge(class_names.transform_keys(&:to_s))
      end

      # `fixtures :all` names the sets this tree compiled (Rails globs
      # fixture_paths for *.yml; the tree carries a compiled module per
      # set, and not every set's YAML).
      def fixtures(*fixture_set_names)
        if fixture_set_names.first == :all
          raise StandardError, "No fixture path found. Please set `#{self}.fixture_paths`." if fixture_paths.nil? || fixture_paths.empty?
          fixture_set_names = TestFixtures.fixture_set_rows.keys
        else
          fixture_set_names = fixture_set_names.flatten.map(&:to_s)
        end

        self.fixture_table_names = (fixture_table_names | fixture_set_names).sort
        setup_fixture_accessors(fixture_set_names)
      end

      def setup_fixture_accessors(fixture_set_names = nil)
        fixture_set_names = Array(fixture_set_names || fixture_table_names)
        unless fixture_set_names.empty?
          self.fixture_sets = fixture_sets.dup
          fixture_set_names.each do |fs_name|
            key = fs_name.to_s.include?("/") ? -fs_name.to_s.tr("/", "_") : fs_name
            key = -key.to_s if key.is_a?(Symbol)
            fs_name = -fs_name.to_s if fs_name.is_a?(Symbol)
            fixture_sets[key] = fs_name
          end
        end
      end

      def uses_transaction(*methods)
        @uses_transaction = [] unless defined?(@uses_transaction)
        @uses_transaction.concat methods.map(&:to_s)
      end

      def uses_transaction?(method)
        @uses_transaction = [] unless defined?(@uses_transaction)
        @uses_transaction.include?(method.to_s)
      end
    end

    def self.included(base)
      base.class_eval do
        class_attribute :fixture_paths, instance_writer: false, default: [FIXTURE_PATH]
        class_attribute :fixture_table_names, default: TestFixtures.fixture_set_rows.keys.sort
        class_attribute :fixture_class_names, default: {}
        class_attribute :use_transactional_tests, default: true
        class_attribute :use_instantiated_fixtures, default: false
        class_attribute :pre_loaded_fixtures, default: false
        class_attribute :lock_threads, default: true
        class_attribute :fixture_sets, default: {}
        class_attribute :database_transactions_config, default: {}
      end
      base.extend(ClassMethods)
      # The accessors of `fixtures :all` (fixture_table_names above).
      base.setup_fixture_accessors
      ActiveSupport.run_load_hooks(:active_record_fixtures, base) if ActiveSupport.respond_to?(:run_load_hooks)
    end

    # Generic fixture accessor for fixture names that may conflict with
    # other methods: `fixture(:web_sites, :rubyonrails)`.
    def fixture(fixture_set_name, *fixture_names)
      active_record_fixture(fixture_set_name, *fixture_names)
    end

    # setup_fixtures, for the test method `name`.
    def __roundhouse_setup_active_record_fixtures(name) # :nodoc:
      @__roundhouse_fixture_test_name = name
      if pre_loaded_fixtures && !use_transactional_tests
        raise RuntimeError, "pre_loaded_fixtures requires use_transactional_tests"
      end
      if pre_loaded_fixtures || use_instantiated_fixtures
        raise NotImplementedError, "roundhouse: #{self.class}: pre_loaded_fixtures and use_instantiated_fixtures are not ported"
      end

      @fixture_cache = {}
      @fixture_cache_key = [self.class.fixture_table_names.dup, self.class.fixture_paths.dup, self.class.fixture_class_names.dup]
      @fixture_connection_pools = []
      @@already_loaded_fixtures ||= {}
      @connection_subscriber = nil
      @saved_pool_configs = Hash.new { |hash, key| hash[key] = {} }

      if run_in_transaction?
        # Load fixtures once and begin transaction.
        @loaded_fixtures = @@already_loaded_fixtures[@fixture_cache_key]
        unless @loaded_fixtures
          invalidate_already_loaded_fixtures
          @loaded_fixtures = @@already_loaded_fixtures[@fixture_cache_key] = load_fixtures
        end
        TestFixtures.reset_fixture_accessors(@loaded_fixtures.values)

        setup_transactional_fixtures
      else
        # Load fixtures for every test.
        invalidate_already_loaded_fixtures
        @loaded_fixtures = load_fixtures
      end
    end

    # teardown_fixtures.
    def __roundhouse_teardown_fixtures(name) # :nodoc:
      return unless instance_variable_defined?(:@fixture_connection_pools) && @fixture_connection_pools

      # Rollback changes if a transaction is active.
      if run_in_transaction?
        teardown_transactional_fixtures
      else
        invalidate_already_loaded_fixtures
      end
    ensure
      ActiveRecord::Base.connection_handler.clear_active_connections!(:all)
    end

    private
      def run_in_transaction?
        has_explicit_config = database_transactions_config.any? { |_, enabled| enabled }
        (use_transactional_tests || has_explicit_config) &&
          !self.class.uses_transaction?(@__roundhouse_fixture_test_name)
      end

      def invalidate_already_loaded_fixtures
        TestFixtures.reset_cache
        @@already_loaded_fixtures.clear
      end

      def load_fixtures
        TestFixtures.create_fixtures(fixture_table_names, fixture_class_names)
      end

      def transactional_tests_for_pool?(pool)
        database_transactions_config.fetch(pool.db_config.name.to_sym, use_transactional_tests)
      end

      def setup_transactional_fixtures
        setup_shared_connection_pool

        # Begin transactions for connections already established
        pools = ActiveRecord::Base.connection_handler.connection_pool_list(:writing)

        # Filter to pools that want to use transactions
        pools.select! { |pool| transactional_tests_for_pool?(pool) }

        pools.each do |pool|
          pool.pin_connection!(lock_threads)
          @fixture_connection_pools << pool
          pool.lease_connection
        end

        # When connections are established in the future, begin a transaction too
        @connection_subscriber = ActiveSupport::Notifications.subscribe("!connection.active_record") do |_, _, _, _, payload|
          connection_name = payload[:connection_name] if payload.key?(:connection_name)
          shard = payload[:shard] if payload.key?(:shard)

          if connection_name
            pool = ActiveRecord::Base.connection_handler.retrieve_connection_pool(connection_name, shard: shard)
            if pool
              setup_shared_connection_pool

              if !@fixture_connection_pools.include?(pool) && transactional_tests_for_pool?(pool)
                pool.pin_connection!(lock_threads)
                @fixture_connection_pools << pool
                pool.lease_connection
              end
            end
          end
        end
      end

      def teardown_transactional_fixtures
        ActiveSupport::Notifications.unsubscribe(@connection_subscriber) if @connection_subscriber

        unless @fixture_connection_pools.map(&:unpin_connection!).all?
          # Something caused the transaction to be committed or rolled back
          # We can no longer trust the database is in a clean state.
          @@already_loaded_fixtures.clear
        end
        @fixture_connection_pools.clear
        teardown_shared_connection_pool
      end

      def setup_shared_connection_pool
        handler = ActiveRecord::Base.connection_handler

        handler.connection_pool_names.each do |name|
          pool_manager = handler.send(:connection_name_to_pool_manager)[name]
          pool_manager.shard_names.each do |shard_name|
            writing_pool_config = pool_manager.get_pool_config(ActiveRecord.writing_role, shard_name)
            @saved_pool_configs[name][shard_name] ||= {}
            pool_manager.role_names.each do |role|
              next unless pool_config = pool_manager.get_pool_config(role, shard_name)
              next if pool_config == writing_pool_config

              @saved_pool_configs[name][shard_name][role] = pool_config
              pool_manager.set_pool_config(role, shard_name, writing_pool_config)
            end
          end
        end
      end

      # `shops(:snowdevil)`: Rails' accessors, for the calls the compiler
      # did not lower to the set's module (`ShopsFixtures.snowdevil`).
      def method_missing(method, ...)
        if fixture_sets.key?(method.name)
          active_record_fixture(method, ...)
        else
          super
        end
      end

      def respond_to_missing?(method, include_private = false)
        if include_private && fixture_sets.key?(method.name)
          true
        else
          super
        end
      end

      def active_record_fixture(fixture_set_name, *fixture_names)
        if fs_name = fixture_sets[fixture_set_name.name]
          access_fixture(fs_name, *fixture_names)
        else
          raise StandardError, "No fixture set named '#{fixture_set_name.inspect}'"
        end
      end

      # Each label is a method of the set's compiled module, which finds
      # (and memoizes) its record; the module's memo is this test's, as
      # `@fixture_cache` is (reset_fixture_accessors).
      def access_fixture(fs_name, *fixture_names)
        force_reload = fixture_names.pop if fixture_names.last == true || fixture_names.last == :reload
        return_single_record = fixture_names.size == 1

        row = @loaded_fixtures[fs_name]
        raise NoMethodError, "undefined method 'fixtures' for nil" unless row
        fixtures = Object.const_get(row[1], false)
        labels = (fixtures.singleton_methods(false) - [:_fixtures_load!]).map(&:to_s)
        if fixture_names.empty?
          fixture_names = labels.sort_by { |label| fixtures.method(label).source_location&.last.to_i }
        end
        @fixture_cache[fs_name] ||= {}

        instances = fixture_names.map do |f_name|
          f_name = f_name.to_s if f_name.is_a?(Symbol)
          @fixture_cache[fs_name].delete(f_name) if force_reload

          if labels.include?(f_name)
            @fixture_cache[fs_name][f_name] ||= begin
              memo = TestFixtures.fixture_cache_ivar(f_name)
              fixtures.instance_variable_set(memo, nil) if force_reload && fixtures.instance_variable_defined?(memo)
              fixtures.public_send(f_name)
            end
          else
            raise StandardError, "No fixture named '#{f_name}' found for fixture set '#{fs_name}'"
          end
        end

        return_single_record ? instances.first : instances
      end

      def teardown_shared_connection_pool
        handler = ActiveRecord::Base.connection_handler
        removed_pool_configs = []
        pool_managers = handler.send(:connection_name_to_pool_manager)

        @saved_pool_configs.each_pair do |name, shards|
          pool_manager = pool_managers[name]
          shards.each_pair do |shard_name, roles|
            roles.each_pair do |role, pool_config|
              if pool_manager.get_pool_config(role, shard_name)
                pool_manager.set_pool_config(role, shard_name, pool_config)
              else
                removed_pool_configs << pool_config
              end
            end
          end
        end

        @saved_pool_configs.clear

        remaining_pool_configs = pool_managers.values.flat_map(&:pool_configs)
        removed_pool_configs.compact.uniq.each do |pool_config|
          pool_config.disconnect! unless remaining_pool_configs.include?(pool_config)
        end
      end
  end
end
