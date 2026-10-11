# frozen_string_literal: true

# ActiveRecord::QueryLogs and ActiveRecord.query_transformers, from the Rails
# native Core runs (Shopify/rails 5eb8c4c45bee, 8.2.0.alpha;
# docs/RAILS_VERSION.md):
#
# * activerecord lib/active_record.rb:517, `singleton_class.attr_accessor
#   :query_transformers; self.query_transformers = []`: callables of
#   `(sql, connection) -> sql` an adapter runs over every statement it sends
#   (connection_adapters/query_intent.rb `preprocess_query`; here
#   `Db.transform_query`, runtime/db_pg_cruby.rb).
# * lib/active_record/query_logs.rb and query_logs_formatter.rb, verbatim
#   apart from `ActiveSupport::Ractors.shareable_proc` (no Ractors here):
#   the tags/taggings tables, the handler classes, the legacy and
#   sqlcommenter formatters, comment caching and escaping.
# * the railtie's `active_record.query_log_tags_config` initializer
#   (railtie.rb:405-437), registered by `QueryLogs.apply_railtie_config`
#   from the app's `config.active_record.query_log_tags_*` values
#   (project::query_log_tags_block). Rails also sets
#   `ActiveRecord.disable_prepared_statements` there; every statement a
#   transformer rewrites is sent unprepared (`Db.execute_handle`), which is
#   what that setting is for.
# * ActiveSupport::ExecutionContext.after_change (execution_context.rb,
#   ported in active_support_reporters.rb): QueryLogs clears its cached
#   comment when the context changes.
#
# CRuby-only (the PostgreSQL tree, `project::apply_postgres_bootstrap`),
# after runtime/active_support_backtrace_cleaner.rb and
# runtime/active_record_log_subscriber.rb (`query_source_location` reads the
# subscriber's cleaner).
require "erb"
require_relative "active_support_lazy_load_hooks"
require_relative "active_support_attribute_accessors_per_thread"

module ActiveRecord
  singleton_class.attr_accessor :query_transformers
  self.query_transformers = []

  module QueryLogs
    module LegacyFormatter # :nodoc:
      class << self
        # Formats the key value pairs into a string.
        def format(key, value)
          "#{key}:#{value}"
        end

        def join(pairs)
          pairs.join(",")
        end
      end
    end

    class SQLCommenter # :nodoc:
      class << self
        def format(key, value)
          "#{key}='#{ERB::Util.url_encode(value)}'"
        end

        def join(pairs)
          pairs.join(",")
        end
      end
    end

    class GetKeyHandler # :nodoc:
      def initialize(name)
        @name = name
      end

      def call(context)
        context[@name]
      end
    end

    class IdentityHandler # :nodoc:
      def initialize(value)
        @value = value
      end

      def call(_context)
        @value
      end
    end

    class ZeroArityHandler # :nodoc:
      def initialize(proc)
        @proc = proc
      end

      def call(_context)
        @proc.call
      end
    end

    @taggings = {}.freeze
    @tags = [ :application ].freeze
    @prepend_comment = false
    @cache_query_log_tags = false
    @tags_formatter = false

    thread_mattr_accessor :cached_comments, instance_accessor: false

    class << self
      attr_reader :tags, :taggings, :tags_formatter # :nodoc:
      attr_accessor :prepend_comment, :cache_query_log_tags # :nodoc:

      def taggings=(taggings) # :nodoc:
        @taggings = taggings.freeze
        @handlers = rebuild_handlers
      end

      def tags=(tags) # :nodoc:
        @tags = tags.freeze
        @handlers = rebuild_handlers
      end

      def tags_formatter=(format) # :nodoc:
        @formatter = formatter_for(format)
        @tags_formatter = format
      end

      def call(sql, connection) # :nodoc:
        return sql if connection.pool.db_config.query_log_tags_config == false

        comment = self.comment(sql: sql, connection: connection)

        if comment.blank?
          sql
        elsif resolve_prepend_comment(connection)
          "#{comment} #{sql}"
        else
          "#{sql} #{comment}"
        end
      end

      def clear_cache # :nodoc:
        self.cached_comments = nil
      end

      def query_source_location # :nodoc:
        LogSubscriber.backtrace_cleaner.first_clean_frame
      end

      # railtie.rb:405-437, `active_record.query_log_tags_config`, over the
      # values the app's config gives (nil where it gives none): once the app
      # has initialized, an enabled config puts QueryLogs among the query
      # transformers and applies the app's tags, format and comment position.
      def apply_railtie_config(enabled:, tags:, format:, cache:, prepend_comment:) # :nodoc:
        Rails.application.config.after_initialize do
          if enabled
            ActiveRecord.query_transformers << ActiveRecord::QueryLogs
            ActiveRecord::QueryLogs.taggings = ActiveRecord::QueryLogs.taggings.merge(
              application:  Rails.application.class.name.to_s.split("::").first,
              pid:          -> { Process.pid.to_s },
              socket:       ->(context) { context[:connection].pool.db_config.socket },
              db_host:      ->(context) { context[:connection].pool.db_config.host },
              database:     ->(context) { context[:connection].pool.db_config.database },
              source_location: -> { QueryLogs.query_source_location }
            )

            if tags.present?
              ActiveRecord::QueryLogs.tags = tags
            end

            if format
              ActiveRecord::QueryLogs.tags_formatter = format
            end

            if cache
              ActiveRecord::QueryLogs.cache_query_log_tags = true
            end

            if prepend_comment
              ActiveRecord::QueryLogs.prepend_comment = true
            end
          end
        end
      end

      private
        def formatter_for(format)
          case format
          when :legacy
            LegacyFormatter
          when :sqlcommenter
            SQLCommenter
          else
            raise ArgumentError, "Formatter is unsupported: #{format}"
          end
        end

        def resolve_formatter(connection)
          if (format = connection.pool.db_config.query_log_tags_format)
            formatter_for(format)
          else
            @formatter
          end
        end

        def resolve_prepend_comment(connection)
          prepend = connection.pool.db_config.query_log_tags_prepend_comment
          prepend.nil? ? prepend_comment : prepend
        end

        def rebuild_handlers
          handlers = []
          @tags.each do |i|
            if i.is_a?(Hash)
              i.each do |k, v|
                handlers << [k, build_handler(k, v)]
              end
            else
              handlers << [i, build_handler(i)]
            end
          end
          handlers.sort_by! { |(key, _)| key.to_s }
        end

        def build_handler(name, handler = nil)
          handler ||= @taggings[name]
          if handler.nil?
            GetKeyHandler.new(name)
          elsif handler.respond_to?(:call)
            if handler.arity == 0
              ZeroArityHandler.new(handler)
            else
              handler
            end
          else
            IdentityHandler.new(handler)
          end
        end

        # Returns an SQL comment +String+ containing the query log tags.
        # Sets and returns a cached comment if <tt>cache_query_log_tags</tt> is +true+.
        def comment(extra_context)
          formatter = resolve_formatter(extra_context[:connection])

          if cache_query_log_tags
            cache = (self.cached_comments ||= {})
            cache.fetch(formatter) do
              cache[formatter] = uncached_comment(extra_context, formatter)
            end
          else
            uncached_comment(extra_context, formatter)
          end
        end

        def uncached_comment(extra_context, formatter)
          content = tag_content(extra_context, formatter)

          if content.present?
            "/*#{escape_sql_comment(content)}*/"
          end
        end

        def escape_sql_comment(content)
          # Sanitize a string to appear within an SQL comment
          # For compatibility, this also surrounding "/*+", "/*", and "*/"
          # characters, possibly with single surrounding space.
          # Then follows that by replacing any internal "*/" or "/ *" with
          # "* /" or "/ *"
          comment = content.to_s.dup
          comment.gsub!(%r{\A\s*/\*\+?\s?|\s?\*/\s*\Z}, "")
          comment.gsub!("*/", "* /")
          comment.gsub!("/*", "/ *")
          comment
        end

        def tag_content(extra_context, formatter)
          context = ActiveSupport::ExecutionContext.to_h
          context.reverse_merge!(extra_context)

          pairs = @handlers.filter_map do |(key, handler)|
            val = handler.call(context)
            formatter.format(key, val) unless val.nil?
          end
          formatter.join(pairs)
        end
    end

    ActiveSupport::ExecutionContext.after_change { ActiveRecord::QueryLogs.clear_cache }

    @handlers = rebuild_handlers
    self.tags_formatter = :legacy
  end
end
