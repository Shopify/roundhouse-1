# frozen_string_literal: true

# ActiveSupport::Deprecation on the PostgreSQL CRuby tree: activesupport
# 8.2.0.alpha (Shopify/rails 5813559f, lib/active_support/deprecation.rb,
# deprecation/{behaviors,reporting,disallowed,deprecators}.rb,
# deprecator.rb) and railties' `Rails.deprecator` (rails/deprecator.rb)
# and `Rails.application.deprecators` (rails/application.rb:248), ported
# over this tree's runtime. `require "active_support"` loads the class
# before any app code, as boot.rb loads this file.
#
# A gem builds its own deprecator at load (`ActiveSupport::Deprecation.new
# ("5.0.0", "ShopifyI18n")`), registers it (`app.deprecators[:hedwig] =
# Hedwig.deprecator`) and warns through it; the app's environment
# configures every registered one (`config.active_support.deprecation =
# :silence`), which boot applies after the environment program as the
# `active_support.deprecation_behavior` initializer does.
#
# Kept verbatim: the behaviors (:raise, :stderr, :log, :notify, :silence,
# :report and a callable of any arity), `warn`/`silence`/`allow`/
# `deprecation_warning`, disallowed warnings and the caller message,
# which skips frames under the Rails gem root and Ruby's libdir. Here the
# Rails root is this runtime's directory, the frames Rails' own code
# would have contributed. The per-thread silence counter and allowance are
# concurrent-ruby's ThreadLocalVar, as Rails keeps them (the gem is in
# this tree's bundle).
#
# Not ported: `deprecate` method wrappers (core_ext/module/deprecation),
# DeprecatedConstantAccessor and the deprecation proxies. A read of one
# is refused by name.
require "concurrent/atomic/thread_local_var"
require "rbconfig"

module ActiveSupport
  # Raised when ActiveSupport::Deprecation::Behavior#behavior is set with
  # <tt>:raise</tt>.
  class DeprecationException < StandardError
  end

  class Deprecation
    # Default warning behaviors per Rails.env.
    DEFAULT_BEHAVIORS = {
      raise: ->(message, callstack, deprecator) do
        e = DeprecationException.new(message)
        e.set_backtrace(callstack.map(&:to_s))
        raise e
      end,

      stderr: ->(message, callstack, deprecator) do
        $stderr.puts(message)
        $stderr.puts callstack.join("\n  ") if deprecator.debug
      end,

      log: ->(message, callstack, deprecator) do
        logger =
            if defined?(Rails.logger) && Rails.logger
              Rails.logger
            else
              ActiveSupport::Logger.new($stderr)
            end
        logger.warn message
        logger.debug callstack.join("\n  ") if deprecator.debug
      end,

      notify: ->(message, callstack, deprecator) do
        ActiveSupport::Notifications.instrument(
          "deprecation.#{Deprecation.underscore(deprecator.gem_name).tr("/", "_")}",
          message: message,
          callstack: callstack,
          gem_name: deprecator.gem_name,
          deprecation_horizon: deprecator.deprecation_horizon,
        )
      end,

      silence: ->(message, callstack, deprecator) { },

      report: ->(message, callstack, deprecator) do
        error = DeprecationException.new(message)
        error.set_backtrace(callstack.map(&:to_s))
        ActiveSupport.error_reporter.report(error)
      end
    }.freeze

    # String#underscore with no acronyms registered (the :notify event
    # name, `"Shopify/Money"` -> `"shopify/money"`).
    def self.underscore(name) # :nodoc:
      return name unless /[A-Z-]|::/.match?(name)
      word = name.gsub("::", "/")
      word.gsub!(/(?<=[A-Z])(?=[A-Z][a-z])|(?<=[a-z\d])(?=[A-Z])/, "_")
      word.tr!("-", "_")
      word.downcase!
      word
    end

    module Behavior
      # Whether to print a backtrace along with the warning.
      attr_accessor :debug

      # Returns the current behavior or if one isn't set, defaults to +:stderr+.
      def behavior
        @behavior ||= [DEFAULT_BEHAVIORS[:stderr]]
      end

      # Returns the current behavior for disallowed deprecations or if one isn't set, defaults to +:raise+.
      def disallowed_behavior
        @disallowed_behavior ||= [DEFAULT_BEHAVIORS[:raise]]
      end

      def behavior=(behavior)
        @behavior = Array(behavior).map { |b| DEFAULT_BEHAVIORS[b] || arity_coerce(b) }
      end

      def disallowed_behavior=(behavior)
        @disallowed_behavior = Array(behavior).map { |b| DEFAULT_BEHAVIORS[b] || arity_coerce(b) }
      end

      private
        def arity_coerce(behavior)
          unless behavior.respond_to?(:call)
            raise ArgumentError, "#{behavior.inspect} is not a valid deprecation behavior."
          end

          case arity_of_callable(behavior)
          when 2
            ->(message, callstack, deprecator) do
              behavior.call(message, callstack)
            end
          when -2..3
            behavior
          else
            ->(message, callstack, deprecator) do
              behavior.call(message, callstack, deprecator.deprecation_horizon, deprecator.gem_name)
            end
          end
        end

        def arity_of_callable(callable)
          callable.respond_to?(:arity) ? callable.arity : callable.method(:call).arity
        end
    end

    module Reporting
      # Whether to print a message (silent mode)
      attr_writer :silenced
      # Name of gem where method is deprecated
      attr_accessor :gem_name

      # Outputs a deprecation warning to the output configured by
      # ActiveSupport::Deprecation#behavior.
      def warn(message = nil, callstack = nil)
        return if silenced

        callstack ||= caller_locations(2)
        deprecation_message(callstack, message).tap do |full_message|
          if deprecation_disallowed?(message)
            disallowed_behavior.each { |b| b.call(full_message, callstack, self) }
          else
            behavior.each { |b| b.call(full_message, callstack, self) }
          end
        end
      end

      # Silence deprecation warnings within the block.
      def silence(&block)
        begin_silence
        block.call
      ensure
        end_silence
      end

      def begin_silence # :nodoc:
        @silence_counter.value += 1
      end

      def end_silence # :nodoc:
        @silence_counter.value -= 1
      end

      def silenced
        @silenced || @silence_counter.value.nonzero?
      end

      # Allow previously disallowed deprecation warnings within the block.
      def allow(allowed_warnings = :all, if: true, &block)
        conditional = binding.local_variable_get(:if)
        conditional = conditional.call if conditional.respond_to?(:call)
        if conditional
          @explicitly_allowed_warnings.bind(allowed_warnings, &block)
        else
          yield
        end
      end

      def deprecation_warning(deprecated_method_name, message = nil, caller_backtrace = nil)
        caller_backtrace ||= caller_locations(2)
        deprecated_method_warning(deprecated_method_name, message).tap do |msg|
          warn(msg, caller_backtrace)
        end
      end

      private
        # Outputs a deprecation warning message
        def deprecated_method_warning(method_name, message = nil)
          warning = "#{method_name} is deprecated and will be removed from #{gem_name} #{deprecation_horizon}"
          case message
          when Symbol then "#{warning} (use #{message} instead)"
          when String then "#{warning} (#{message})"
          else warning
          end
        end

        def deprecation_message(callstack, message = nil)
          message ||= "You are using deprecated behavior which will be removed from the next major or minor release."
          "DEPRECATION WARNING: #{message} #{deprecation_caller_message(callstack)}"
        end

        def deprecation_caller_message(callstack)
          file, line, method = extract_callstack(callstack)
          if file
            if line && method
              "(called from #{method} at #{file}:#{line})"
            else
              "(called from #{file}:#{line})"
            end
          end
        end

        def extract_callstack(callstack)
          return [] if callstack.empty?

          offending_line = callstack.find { |frame|
            path = frame.absolute_path || frame.path
            path && !ignored_callstack?(path)
          } || callstack.first

          [offending_line.path, offending_line.lineno, offending_line.label]
        end

        # Rails' `File.expand_path("../../../..", __dir__)` is the
        # directory of the Rails gems, whose frames the caller message
        # skips. The frames standing in for Rails' own are this runtime's.
        RAILS_GEM_ROOT = File.expand_path(__dir__) + "/"
        private_constant :RAILS_GEM_ROOT
        LIB_DIR = RbConfig::CONFIG["libdir"]
        private_constant :LIB_DIR

        def ignored_callstack?(path)
          path.start_with?(RAILS_GEM_ROOT, LIB_DIR) || path.include?("<internal:")
        end
    end

    module Disallowed
      # Sets the criteria used to identify deprecation messages which should be
      # disallowed. Can be an array containing strings, symbols, or regular
      # expressions. (Symbols are treated as strings.) These are compared against
      # the text of the generated deprecation warning.
      #
      # Additionally the scalar symbol +:all+ may be used to treat all
      # deprecations as disallowed.
      attr_writer :disallowed_warnings

      # Returns the configured criteria used to identify deprecation messages
      # which should be treated as disallowed.
      def disallowed_warnings
        @disallowed_warnings ||= []
      end

      private
        def deprecation_disallowed?(message)
          return false if explicitly_allowed?(message)
          return true if disallowed_warnings == :all
          message && disallowed_warnings.any? do |rule|
            case rule
            when String, Symbol
              message.include?(rule.to_s)
            when Regexp
              rule.match?(message)
            end
          end
        end

        def explicitly_allowed?(message)
          allowances = @explicitly_allowed_warnings.value
          return false unless allowances
          return true if allowances == :all
          message && Array(allowances).any? do |rule|
            case rule
            when String, Symbol
              message.include?(rule.to_s)
            when Regexp
              rule.match?(message)
            end
          end
        end
    end

    # A managed collection of deprecators. Configuration methods, such as
    # #behavior=, affect all deprecators in the collection. Additionally, the
    # #silence method silences all deprecators in the collection for the
    # duration of a given block.
    class Deprecators
      def initialize
        @options = {}
        @deprecators = {}
      end

      # Returns a deprecator added to this collection via #[]=.
      def [](name)
        @deprecators[name]
      end

      # Adds a given +deprecator+ to this collection. The deprecator will be
      # immediately configured with any options previously set on this
      # collection.
      def []=(name, deprecator)
        apply_options(deprecator)
        @deprecators[name] = deprecator
      end

      # Iterates over all deprecators in this collection. If no block is given,
      # returns an +Enumerator+.
      def each(&block)
        return to_enum(__method__) unless block
        @deprecators.each_value(&block)
      end

      # Sets the silenced flag for all deprecators in this collection.
      def silenced=(silenced)
        set_option(:silenced, silenced)
      end

      # Sets the debug flag for all deprecators in this collection.
      def debug=(debug)
        set_option(:debug, debug)
      end

      # Sets the deprecation warning behavior for all deprecators in this
      # collection.
      def behavior=(behavior)
        set_option(:behavior, behavior)
      end

      # Sets the disallowed deprecation warning behavior for all deprecators in
      # this collection.
      def disallowed_behavior=(disallowed_behavior)
        set_option(:disallowed_behavior, disallowed_behavior)
      end

      # Sets the disallowed deprecation warnings for all deprecators in this
      # collection.
      def disallowed_warnings=(disallowed_warnings)
        set_option(:disallowed_warnings, disallowed_warnings)
      end

      # Silences all deprecators in this collection for the duration of the
      # given block.
      def silence(&block)
        each { |deprecator| deprecator.begin_silence }
        block.call
      ensure
        each { |deprecator| deprecator.end_silence }
      end

      private
        def set_option(name, value)
          @options[name] = value
          each { |deprecator| deprecator.public_send("#{name}=", value) }
        end

        def apply_options(deprecator)
          @options.each do |name, value|
            deprecator.public_send("#{name}=", value)
          end
        end
    end

    include Behavior
    include Reporting
    include Disallowed

    MUTEX = Mutex.new # :nodoc:
    private_constant :MUTEX

    def self._instance # :nodoc:
      @_instance ||= MUTEX.synchronize { @_instance ||= new }
    end

    # The version number in which the deprecated behavior will be removed, by default.
    attr_accessor :deprecation_horizon

    # It accepts two parameters on initialization. The first is a version of library
    # and the second is a library name.
    #
    #   ActiveSupport::Deprecation.new('2.0', 'MyLibrary')
    def initialize(deprecation_horizon = "9.0", gem_name = "Rails")
      self.gem_name = gem_name
      self.deprecation_horizon = deprecation_horizon
      # By default, warnings are not silenced and debugging is off.
      self.silenced = false
      self.debug = false
      @silence_counter = Concurrent::ThreadLocalVar.new(0)
      @explicitly_allowed_warnings = Concurrent::ThreadLocalVar.new(nil)
    end
  end

  # activesupport deprecator.rb.
  def self.deprecator # :nodoc:
    ActiveSupport::Deprecation._instance
  end
end

module Rails
  # railties rails/deprecator.rb.
  def self.deprecator # :nodoc:
    @deprecator ||= ActiveSupport::Deprecation.new
  end

  # railties application.rb:248. The runtime's `Rails.application` answers
  # a fresh Application per call (rails.rb), so the once-per-process
  # collection Rails keeps in the application's `@deprecators` lives on
  # the class, as `revision_state` does (rails_railtie.rb). Reopened
  # through its singleton and `class_eval`, so this file declares no
  # Rails::Application of its own (runtime_reads gates the names a port
  # declares). activesupport's `active_support.deprecator` initializer
  # (railtie.rb:15) registers ActiveSupport.deprecator ahead of every
  # other initializer.
  class << Application
    def deprecators # :nodoc:
      @deprecators ||= ActiveSupport::Deprecation::Deprecators.new.tap do |deprecators|
        deprecators[:railties] = Rails.deprecator
        deprecators[:active_support] = ActiveSupport.deprecator
      end
    end

    # activesupport railtie.rb:92-110, `active_support.deprecation_behavior`,
    # over the values the app's config gives (nil where it gives none).
    def apply_deprecation_behavior(report_deprecations:, deprecation:, disallowed_deprecation:, disallowed_deprecation_warnings:) # :nodoc:
      if report_deprecations == false
        deprecators.silenced = true
        deprecators.behavior = :silence
        deprecators.disallowed_behavior = :silence
      else
        if deprecation
          deprecators.behavior = deprecation
        end

        if disallowed_deprecation
          deprecators.disallowed_behavior = disallowed_deprecation
        end

        if disallowed_deprecation_warnings
          deprecators.disallowed_warnings = disallowed_deprecation_warnings
        end
      end
    end
  end

  Application.class_eval do
    def deprecators
      self.class.deprecators
    end
  end
end
