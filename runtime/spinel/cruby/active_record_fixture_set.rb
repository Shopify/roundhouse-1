# frozen_string_literal: true
# ActiveRecord::FixtureSet, FixtureSet::File, FixtureSet::RenderContext
# and ActiveRecord::Fixture, ported from the Rails native Core runs
# (Shopify/rails 5813559f4aed, 8.2.0.alpha,
# activerecord/lib/active_record/fixtures.rb, fixture_set/file.rb and
# fixture_set/render_context.rb, with ActiveSupport::ConfigurationFile's
# parse).
#
# This is the class surface an app's own fixture code builds on (Shopify
# Core's FasterFixtures subclasses FixtureSet::File and Fixture, and calls
# FixtureSet.identify and the per-pool fixture cache): identify and
# composite_identify, default_fixture_model_name / _table_name,
# all_loaded_fixtures, the cache (reset_cache, fixture_is_cached?,
# cached_fixtures, cache_fixtures), instantiate_fixtures, context_class,
# a FixtureSet read from its YAML files, a File and a Fixture with #find.
#
# Not ported, and refused by name: FixtureSet.create_fixtures and
# FixtureSet#table_rows (TableRows, TableRow, ModelMetadata). The tree's
# test fixtures load through ActiveRecord::TestFixtures
# (runtime/active_record_test_fixtures.rb), which inserts the rows the
# compiler built from the app's fixture files.
#
# Active Record autoloads FixtureSet in any process, so the PostgreSQL
# boot (runtime/database_configuration.rb) requires this file.
require "erb"
require "yaml"
require "zlib"
require "digest/sha1"
require "base64"

module ActiveRecord
  class FixtureClassNotFound < ActiveRecord::ActiveRecordError # :nodoc:
  end

  # ActiveRecord::ModelSchema's class attributes the fixture names read,
  # at Rails' defaults: the compiler names each model's table by them.
  class Base
    class << self
      def pluralize_table_names
        true
      end unless method_defined?(:pluralize_table_names)

      def table_name_prefix
        ""
      end unless method_defined?(:table_name_prefix)

      def table_name_suffix
        ""
      end unless method_defined?(:table_name_suffix)
    end
  end

  class FixtureSet
    # 2**30 - 1, written as a literal so the analyzer types a read of it.
    MAX_ID = 1_073_741_823

    # Digest::UUID::OID_NAMESPACE.
    OID_NAMESPACE = "k\xA7\xB8\x12\x9D\xAD\x11\xD1\x80\xB4\x00\xC0O\xD40\xC8".b.freeze

    @@all_cached_fixtures = Hash.new { |h, k| h[k] = {} }
    @@parsing_cache ||= {}
    @@all_loaded_fixtures = {}

    # cattr_accessor :all_loaded_fixtures, default: {}.
    def self.all_loaded_fixtures
      @@all_loaded_fixtures
    end

    def self.all_loaded_fixtures=(value)
      @@all_loaded_fixtures = value
    end

    def all_loaded_fixtures
      @@all_loaded_fixtures
    end

    def all_loaded_fixtures=(value)
      @@all_loaded_fixtures = value
    end

    class << self
      def without_parsing_cache
        parsing_cache_was = @@parsing_cache
        @@parsing_cache = {}
        yield
      ensure
        @@parsing_cache = parsing_cache_was
      end

      def default_fixture_model_name(fixture_set_name, config = ActiveRecord::Base) # :nodoc:
        config.pluralize_table_names ?
          fixture_set_name.singularize.camelize :
          fixture_set_name.camelize
      end

      def default_fixture_table_name(fixture_set_name, config = ActiveRecord::Base) # :nodoc:
        "#{ config.table_name_prefix }"\
        "#{ fixture_set_name.tr('/', '_') }"\
        "#{ config.table_name_suffix }".to_sym
      end

      def reset_cache
        @@all_cached_fixtures.clear
      end

      def cache_for_connection_pool(connection_pool)
        @@all_cached_fixtures[connection_pool]
      end

      def fixture_is_cached?(connection_pool, table_name)
        cache_for_connection_pool(connection_pool)[table_name]
      end

      def cached_fixtures(connection_pool, keys_to_fetch = nil)
        if keys_to_fetch
          cache_for_connection_pool(connection_pool).values_at(*keys_to_fetch)
        else
          cache_for_connection_pool(connection_pool).values
        end
      end

      def cache_fixtures(connection_pool, fixtures_map)
        cache_for_connection_pool(connection_pool).update(fixtures_map)
      end

      def instantiate_fixtures(object, fixture_set, load_instances = true)
        return unless load_instances
        fixture_set.each do |fixture_name, fixture|
          object.instance_variable_set "@#{fixture_name}", fixture.find
        rescue FixtureClassNotFound
          nil
        end
      end

      def instantiate_all_loaded_fixtures(object, load_instances = true)
        all_loaded_fixtures.each_value do |fixture_set|
          instantiate_fixtures(object, fixture_set, load_instances)
        end
      end

      # Rails reads and inserts the YAML files here (read_and_insert,
      # TableRows); this tree's fixtures are compiled and inserted by
      # ActiveRecord::TestFixtures.
      def create_fixtures(fixtures_directories, fixture_set_names, class_names = {}, config = ActiveRecord::Base)
        raise NotImplementedError,
          "roundhouse: ActiveRecord::FixtureSet.create_fixtures is not ported: this tree's fixtures are compiled " \
          "from the app's fixture files and loaded by ActiveRecord::TestFixtures"
      end

      # Returns a consistent, platform-independent identifier for +label+.
      #
      # \Integer identifiers are values less than 2^30. UUIDs are RFC 4122 version 5 SHA-1 hashes.
      def identify(label, column_type = :integer)
        if column_type == :uuid
          uuid_v5(OID_NAMESPACE, label.to_s)
        else
          Zlib.crc32(label.to_s) % MAX_ID
        end
      end

      # Returns a consistent, platform-independent hash representing a mapping
      # between the label and the subcomponents of the provided composite key.
      #
      #   composite_identify("label", [:a, :b, :c]) # => { a: hash_1, b: hash_2, c: hash_3 }
      def composite_identify(label, key)
        key
          .index_with
          .with_index { |sub_key, index| (identify(label) << index) % MAX_ID }
          .with_indifferent_access
      end

      # Superclass for the evaluation contexts used by \ERB fixtures.
      def context_class
        @context_class ||= Class.new
      end

      private
        # Digest::UUID.uuid_v5 (uuid_from_hash over Digest::SHA1).
        def uuid_v5(uuid_namespace, name)
          hash = Digest::SHA1.new
          hash.update(uuid_namespace)
          hash.update(name)
          ary = hash.digest.unpack("NnnnnN")
          ary[2] = (ary[2] & 0x0FFF) | (5 << 12)
          ary[3] = (ary[3] & 0x3FFF) | 0x8000
          "%08x-%04x-%04x-%04x-%04x%08x" % ary
        end
    end

    attr_reader :table_name, :name, :fixtures, :model_class, :ignored_fixtures, :config

    def initialize(_, name, class_name, path, config = ActiveRecord::Base)
      @name     = name
      @path     = path
      @config   = config

      self.model_class = class_name
      @fixtures = read_fixture_files(path)

      @table_name = model_class&.table_name || self.class.default_fixture_table_name(name, config)
    end

    def [](x)
      fixtures[x]
    end

    def []=(k, v)
      fixtures[k] = v
    end

    def each(&block)
      fixtures.each(&block)
    end

    def size
      fixtures.size
    end

    # Rails builds the rows with TableRows (associations, timestamps,
    # primary keys from labels), which this tree does at build time.
    def table_rows
      raise NotImplementedError,
        "roundhouse: ActiveRecord::FixtureSet#table_rows is not ported: this tree's fixture rows are compiled " \
        "from the app's fixture files and loaded by ActiveRecord::TestFixtures"
    end

    private
      def model_class=(class_name)
        if class_name.is_a?(Class) # TODO: Should be an AR::Base type class, or any?
          @model_class = class_name
        else
          @model_class = class_name.safe_constantize if class_name
        end
      end

      def ignored_fixtures=(base)
        @ignored_fixtures =
            case base
            when Array
              base
            when String
              [base]
            else
              []
            end

        @ignored_fixtures += ["DEFAULTS"] unless @ignored_fixtures.include?("DEFAULTS")
        @ignored_fixtures.compact
      end

      # Loads the fixtures from the YAML file at +path+.
      # If the file sets the +model_class+ and current instance value is not set,
      # it uses the file value.
      def read_fixture_files(path)
        yaml_files = Dir["#{path}{.yml,/**/*.yml}"].select { |f|
          ::File.file?(f)
        }

        raise ArgumentError, "No fixture files found for #{@name}" if yaml_files.empty?

        yaml_files.each_with_object({}) do |file, fixtures|
          fh = (@@parsing_cache[file] ||= FixtureSet::File.open(file))
          self.model_class ||= fh.model_class if fh.model_class
          self.model_class ||= default_fixture_model_class
          self.ignored_fixtures ||= fh.ignored_fixtures
          fh.each do |fixture_name, row|
            fixtures[fixture_name] = ActiveRecord::Fixture.new(row.dup, model_class)
          end
        end
      end

      def default_fixture_model_class
        klass = ActiveRecord::FixtureSet.default_fixture_model_name(@name, @config).safe_constantize
        klass if klass && klass < ActiveRecord::Base
      end

    class File # :nodoc:
      include Enumerable

      ##
      # Open a fixture file named +file+.  When called with a block, the block
      # is called with the filehandle and the filehandle is automatically closed
      # when the block finishes.
      def self.open(file)
        x = new file
        block_given? ? yield(x) : x
      end

      def initialize(file)
        @file = file
      end

      def each(&block)
        rows.each(&block)
      end

      def model_class
        config_row["model_class"]
      end

      def ignored_fixtures
        config_row["ignore"]
      end

      private
        def rows
          @rows ||= raw_rows.reject { |fixture_name, _| fixture_name == "_fixture" }
        end

        def config_row
          @config_row ||= begin
            row = raw_rows.find { |fixture_name, _| fixture_name == "_fixture" }
            if row
              validate_config_row(row.last)
            else
              { 'model_class': nil, 'ignore': nil }
            end
          end
        end

        def raw_rows
          @raw_rows ||= begin
            data = parse_configuration_file(@file, freeze: true, context:
              ActiveRecord::FixtureSet::RenderContext.create_subclass.new.get_binding)
            data ? validate(data).to_a : []
          end
        end

        # ActiveSupport::ConfigurationFile.parse: the file's ERB rendered
        # in +context+ when it has a tag, then YAML.unsafe_load; a YAML
        # syntax error is a Fixture::FormatError (ConfigurationFile's
        # FormatError, rescued in raw_rows).
        def parse_configuration_file(content_path, context: nil, **options)
          content_path = content_path.to_s
          content = ::File.read(content_path)
          if content.include?(" ")
            warn "#{content_path} contains invisible non-breaking spaces, you may want to remove those"
          end
          source =
            if content.include?("<%")
              erb = ERB.new(content).tap { |e| e.filename = content_path }
              context ? erb.result(context) : erb.result
            else
              content
            end

          if source == content
            YAML.unsafe_load_file(content_path, **options) || {}
          else
            YAML.unsafe_load(source, **options) || {}
          end
        rescue Psych::SyntaxError => error
          raise Fixture::FormatError, "YAML syntax error occurred while parsing #{content_path}. " \
                "Please note that YAML must be consistently indented using spaces. Tabs are not allowed. " \
                "Error: #{error.message}"
        end

        def validate_config_row(data)
          unless Hash === data
            raise Fixture::FormatError, "Invalid `_fixture` section: `_fixture` must be a hash: #{@file}"
          end

          begin
            data.assert_valid_keys("model_class", "ignore")
          rescue ArgumentError => error
            raise Fixture::FormatError, "Invalid `_fixture` section: #{error.message}: #{@file}"
          end

          data
        end

        # Validate our unmarshalled data.
        def validate(data)
          unless Hash === data || YAML::Omap === data
            raise Fixture::FormatError, "fixture is not a hash: #{@file}"
          end

          invalid = data.reject { |_, row| Hash === row }
          if invalid.any?
            raise Fixture::FormatError, "fixture key is not a hash: #{@file}, keys: #{invalid.keys.inspect}"
          end
          data
        end
    end
  end

  class Fixture # :nodoc:
    include Enumerable

    class FixtureError < StandardError # :nodoc:
    end

    class FormatError < FixtureError # :nodoc:
    end

    attr_reader :model_class, :fixture

    def initialize(fixture, model_class)
      @fixture     = fixture
      @model_class = model_class
    end

    def class_name
      model_class.name if model_class
    end

    def each(&block)
      fixture.each(&block)
    end

    def [](key)
      fixture[key]
    end

    alias :to_hash :fixture

    # Rails finds under `model_class.all_queries_scope`: the model's
    # default scopes declared `all_queries: true`. This tree declares no
    # such scope, so that scope is the unscoped one.
    def find
      raise FixtureClassNotFound, "No class attached to find." unless model_class
      object = model_class.unscoped do
        pk_clauses = fixture.slice(*Array(model_class.primary_key))
        model_class.find_by!(pk_clauses)
      end
      # Fixtures can't be eagerly loaded
      object.instance_variable_set(:@strict_loading, false)
      object
    end
  end
end

# NOTE: This class has to be defined in compact style in
# order for rendering context subclassing to work correctly.
class ActiveRecord::FixtureSet::RenderContext # :nodoc:
  def self.create_subclass
    Class.new(ActiveRecord::FixtureSet.context_class) do
      def get_binding
        binding()
      end

      def binary(path)
        %(!!binary "#{Base64.strict_encode64(File.binread(path))}")
      end
    end
  end
end

ActiveSupport.run_load_hooks :active_record_fixture_set, ActiveRecord::FixtureSet
