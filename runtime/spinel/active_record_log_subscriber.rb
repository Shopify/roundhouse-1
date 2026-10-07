# frozen_string_literal: true

# ActiveRecord::LogSubscriber's backtrace cleaner, from the Rails native
# Core runs (Shopify/rails 5813559f4aed, 8.2.0.alpha; docs/RAILS_VERSION.md):
#
# * activerecord lib/active_record/log_subscriber.rb:9,
#   `class_attribute :backtrace_cleaner, default: ActiveSupport::BacktraceCleaner.new`.
#   The class reads its nearest assigned value up the superclass chain;
#   assigning on a class sets it for that class and the subclasses that
#   did not assign their own; an instance reads its own value when it
#   assigned one, else the class's; `backtrace_cleaner?` is `!!value`
#   (activesupport core_ext/class/attribute.rb, class_attribute.rb).
# * activerecord lib/active_record/railtie.rb:113-115, the
#   `active_record.backtrace_cleaner` initializer:
#   `ActiveSupport.on_load(:active_record) { LogSubscriber.backtrace_cleaner = ::Rails.backtrace_cleaner }`.
#   Active Record's railtie initializers run before any engine's or the
#   app's config/initializers, so it is registered when this file loads,
#   ahead of the boot's initializer programs (`ingest::initializer_programs`)
#   and before boot.rb fires `:active_record` for them.
#
# Only that surface. The subscriber's event logging (`sql`,
# `strict_loading_violation`, its ActiveSupport::EventReporter::LogSubscriber
# superclass and its event_reporter subscription) is not ported; the
# runtime's query log is runtime/spinel/db_pg_cruby.rb's.
#
# CRuby-only (the PostgreSQL tree, `project::apply_postgres_bootstrap`),
# after runtime/active_support_backtrace_cleaner.rb.
require_relative "active_support_lazy_load_hooks"

module ActiveRecord
  class LogSubscriber # :nodoc:
    class << self
      def backtrace_cleaner
        owner = self
        owner = owner.superclass until owner.instance_variable_defined?(:@__class_attr_backtrace_cleaner)
        owner.instance_variable_get(:@__class_attr_backtrace_cleaner)
      end

      def backtrace_cleaner=(value)
        @__class_attr_backtrace_cleaner = value
      end

      def backtrace_cleaner?
        !!backtrace_cleaner
      end
    end

    self.backtrace_cleaner = ActiveSupport::BacktraceCleaner.new

    def backtrace_cleaner
      defined?(@backtrace_cleaner) ? @backtrace_cleaner : self.class.backtrace_cleaner
    end

    attr_writer :backtrace_cleaner

    def backtrace_cleaner?
      !!backtrace_cleaner
    end
  end
end

ActiveSupport.on_load(:active_record) { ActiveRecord::LogSubscriber.backtrace_cleaner = ::Rails.backtrace_cleaner }
