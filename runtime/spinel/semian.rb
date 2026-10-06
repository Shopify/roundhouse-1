# semian 0.28.3 core (lib/semian.rb and the files it requires), ported for
# the CRuby PostgreSQL tree. Shopify Core's essentials pg.rb initializer
# installs `semian/activerecord_postgresql_adapter` (see
# semian_activerecord_postgresql_adapter.rb beside this file), and its test
# database configuration enables a ticketed bulkhead and a classic circuit
# breaker for the primary connection.
#
# The gem's own lib/semian.rb cannot be loaded in an emitted tree: it ends
# with `ActiveSupport.on_load(:active_record) { require "semian/rails" }`,
# which pulls native Active Record. This file is that library, method for
# method, with these deliberate differences:
#
# * BULKHEAD. As in the gem, `Semian::Resource` is a Ruby class whose
#   ticket methods are replaced by the gem's compiled extension
#   (`require "semian/semian"`, ext/semian/semian.c) when
#   `Semian.semaphores_enabled?`. The extension is the gem's real SysV
#   semaphore implementation, so tickets are shared across processes
#   exactly as in Rails. Only lib/semian/semian.so is loaded; it looks up
#   `Semian`, `Semian::Resource` and the four error classes defined here.
# * concurrent-ruby. The gem's thread-safe primitives are concurrent-ruby
#   types; on CRuby `Concurrent::Array` is `::Array`, `Concurrent::Map` is
#   a mutex-guarded Hash and `Concurrent::AtomicFixnum` is a mutex-guarded
#   Integer. Those same semantics are written out here (ConcurrentMap,
#   ThreadSafe::Integer) so the tree does not need the gem.
# * ADAPTIVE / DUAL CIRCUIT BREAKERS (PID controller) are not ported.
#   Configuring one raises Semian::UnsupportedCircuitBreakerError instead
#   of falling back to the classic breaker.
# * LOGGER. The gem's default is `Logger.new($stderr)`. The emitted tree's
#   `::Logger` is the runtime port, which has only the formatter (see
#   runtime/ruby/logger.rb), so there the default is the same line format
#   through `ActiveSupport::Logger.new($stderr)`. Ruby's own logger is
#   required only when neither exists, so this file never puts a second
#   `Logger` into a tree that has the port.

class LRUHash
  # This LRU (Least Recently Used) hash will allow
  # the cleaning of resources as time goes on.
  # The goal is to remove the least recently used resources
  # everytime we set a new resource. A default window of
  # 5 minutes will allow empty item to stay in the hash
  # for a maximum of 5 minutes
  class NoopMutex
    def synchronize(*)
      yield
    end

    def try_lock
      true
    end

    def unlock
      true
    end

    def locked?
      true
    end

    def owned?
      true
    end
  end

  def keys
    @lock.synchronize { @table.keys }
  end

  def clear
    @lock.synchronize { @table.clear }
  end

  def initialize(max_size: Semian.maximum_lru_size, min_time: Semian.minimum_lru_time)
    @max_size = max_size
    @min_time = min_time
    @table = {}
    @lock =
      if Semian.thread_safe?
        ::Thread::Mutex.new
      else
        ::LRUHash::NoopMutex.new
      end
  end

  def size
    @lock.synchronize { @table.size }
  end

  def count(&block)
    @lock.synchronize { @table.count(&block) }
  end

  def empty?
    @lock.synchronize { @table.empty? }
  end

  def values
    @lock.synchronize { @table.values }
  end

  def set(key, resource)
    @lock.synchronize do
      @table.delete(key)
      @table[key] = resource
      resource.updated_at = Process.clock_gettime(Process::CLOCK_MONOTONIC)
    end
    clear_unused_resources if @table.length > @max_size
  end

  def get(key)
    @lock.synchronize do
      found = @table.delete(key)
      if found
        @table[key] = found
        found.updated_at = Process.clock_gettime(Process::CLOCK_MONOTONIC)
      end
      found
    end
  end

  def delete(key)
    @lock.synchronize do
      @table.delete(key)
    end
  end

  def []=(key, resource)
    set(key, resource)
  end

  def [](key)
    get(key)
  end

  private

  def clear_unused_resources
    payload = {
      size: @table.size,
      examined: 0,
      cleared: 0,
      elapsed: nil,
    }
    timer_start = Process.clock_gettime(Process::CLOCK_MONOTONIC)

    ran = try_synchronize do
      stop_time = Process.clock_gettime(Process::CLOCK_MONOTONIC) - @min_time
      @table.each do |_, resource|
        payload[:examined] += 1
        break if resource.updated_at > stop_time

        next if resource.in_use?

        resource = @table.delete(resource.name)
        if resource
          payload[:cleared] += 1
          resource.destroy
        end
      end
    end

    if ran
      payload[:elapsed] = Process.clock_gettime(Process::CLOCK_MONOTONIC) - timer_start
      Semian.notify(:lru_hash_gc, self, nil, nil, payload)
    end
  end

  EXCEPTION_NEVER = { Exception => :never }.freeze
  EXCEPTION_IMMEDIATE = { Exception => :immediate }.freeze
  private_constant :EXCEPTION_NEVER
  private_constant :EXCEPTION_IMMEDIATE

  def try_synchronize(&block)
    Thread.handle_interrupt(EXCEPTION_NEVER) do
      return false unless @lock.try_lock

      Thread.handle_interrupt(EXCEPTION_IMMEDIATE, &block)
      true
    ensure
      @lock.unlock if @lock.owned?
    end
  end
end

module Semian
  # concurrent-ruby's `Concurrent::Map` on CRuby (MriMapBackend): reads
  # go straight to the Hash, writes and compute_if_absent hold a mutex.
  class ConcurrentMap
    def initialize
      @table = {}
      @write = ::Thread::Mutex.new
    end

    def [](key)
      @table[key]
    end

    def []=(key, value)
      @write.synchronize { @table[key] = value }
    end

    def delete(key)
      @write.synchronize { @table.delete(key) }
    end

    def compute_if_absent(key)
      return @table[key] if @table.key?(key)

      @write.synchronize do
        @table.key?(key) ? @table[key] : (@table[key] = yield)
      end
    end

    def key?(key)
      @table.key?(key)
    end

    def keys
      @table.keys
    end

    def values
      @table.values
    end

    def size
      @table.size
    end

    def empty?
      @table.empty?
    end

    def each_pair(&block)
      @table.dup.each_pair(&block)
      self
    end

    def clear
      @write.synchronize { @table.clear }
      self
    end
  end

  module Instrumentable
    SUBSCRIBERS = ConcurrentMap.new

    def subscribe(name = rand, &block)
      SUBSCRIBERS[name] = block
      name
    end

    def unsubscribe(name)
      SUBSCRIBERS.delete(name)
    end

    def subscribers
      SUBSCRIBERS
    end

    # Args:
    #   event (string)
    #   resource (Object)
    #   scope (string)
    #   adapter (string)
    #   payload (optional)
    def notify(*args)
      SUBSCRIBERS.values.each { |subscriber| subscriber.call(*args) }
    end
  end

  extend self
  extend Instrumentable

  BaseError = Class.new(StandardError)
  SyscallError = Class.new(BaseError)
  TimeoutError = Class.new(BaseError)
  InternalError = Class.new(BaseError)
  OpenCircuitError = Class.new(BaseError)
  SemaphoreMissingError = Class.new(BaseError)

  # Not in the gem: raised where the gem would build an
  # AdaptiveCircuitBreaker or DualCircuitBreaker, which this port does not
  # model. A ScriptError, so no `rescue StandardError` swallows it.
  class UnsupportedCircuitBreakerError < NotImplementedError
  end

  # lib/semian/platform.rb
  def sysv_semaphores_supported?
    /linux/.match(RUBY_PLATFORM)
  end

  def semaphores_enabled?
    !disabled? && sysv_semaphores_supported?
  end

  def disabled?
    ENV.key?("SEMIAN_SEMAPHORES_DISABLED") || ENV.key?("SEMIAN_DISABLED")
  end

  attr_accessor :maximum_lru_size, :minimum_lru_time, :default_permissions, :namespace, :default_force_config_validation

  self.maximum_lru_size = 500
  self.minimum_lru_time = 300 # 300 seconds / 5 minutes
  self.default_permissions = 0660
  self.default_force_config_validation = false

  def thread_safe?
    return @thread_safe if defined?(@thread_safe)

    @thread_safe = true
  end

  def thread_safe=(thread_safe)
    @thread_safe = thread_safe
  end

  @reset_mutex = Mutex.new

  def issue_disabled_semaphores_warning
    return if defined?(@warning_issued)

    @warning_issued = true
    if !sysv_semaphores_supported?
      logger.info("Semian sysv semaphores are not supported on #{RUBY_PLATFORM} - all operations will no-op")
    elsif disabled?
      logger.info("Semian semaphores are disabled, is this what you really want? - all operations will no-op")
    end
  end

  module AdapterError
    attr_accessor :semian_identifier

    def to_s
      message = super
      if @semian_identifier
        prefix = "[#{@semian_identifier}] "
        # When an error is created from another error's message it might
        # already have a semian identifier in their message
        unless message.start_with?(prefix)
          message = "#{prefix}#{message}"
        end
      end
      message
    end
  end

  attr_accessor :logger

  self.logger =
    if defined?(::Logger) && ::Logger.method_defined?(:info)
      ::Logger.new($stderr)
    elsif defined?(::ActiveSupport::Logger)
      ::ActiveSupport::Logger.new($stderr)
    else
      require "logger"
      ::Logger.new($stderr)
    end

  def register(name, **options)
    return UnprotectedResource.new(name) if ENV.key?("SEMIAN_DISABLED")

    # Validate configuration before proceeding
    ConfigurationValidator.new(name, options).validate!

    circuit_breaker = if options[:dual_circuit_breaker]
      create_dual_circuit_breaker(name, **options)
    elsif options[:adaptive_circuit_breaker]
      create_adaptive_circuit_breaker(name, **options)
    else
      create_circuit_breaker(name, **options)
    end

    bulkhead = create_bulkhead(name, **options)

    resources[name] = ProtectedResource.new(name, bulkhead, circuit_breaker)
  end

  def retrieve_or_register(name, **args)
    # If consumer who retrieved / registered by a Semian::Adapter, keep track
    # of who the consumer was so that we can clear the resource reference if needed.
    consumer = args.delete(:consumer)
    if consumer&.class&.include?(Semian::Adapter) && !args[:dynamic]
      consumer_set = consumers.compute_if_absent(name) { ObjectSpace::WeakMap.new }
      consumer_set[consumer] = true
    end
    self[name] || register(name, **args)
  end

  def [](name)
    resources[name]
  end

  def destroy(name)
    resource = resources.delete(name)
    resource&.destroy
  end

  def destroy_all_resources
    resources.values.each(&:destroy)
    resources.clear
  end

  def unregister(name)
    resource = resources.delete(name)
    if resource
      resource.bulkhead&.unregister_worker
      consumers_for_resource = consumers.delete(name) || ObjectSpace::WeakMap.new
      consumers_for_resource.each_key(&:clear_semian_resource)
    end
  end

  def unregister_all_resources
    resources.keys.each do |resource|
      unregister(resource)
    end
  end

  def reset!
    @reset_mutex.synchronize do
      @consumers = ConcurrentMap.new
      @resources = LRUHash.new
    end
  end

  THREAD_BULKHEAD_DISABLED_VAR = :semian_bulkheads_disabled
  private_constant(:THREAD_BULKHEAD_DISABLED_VAR)

  def bulkheads_disabled_in_thread?(thread)
    thread.thread_variable_get(THREAD_BULKHEAD_DISABLED_VAR)
  end

  def disable_bulkheads_for_thread(thread)
    old_value = thread.thread_variable_get(THREAD_BULKHEAD_DISABLED_VAR)
    thread.thread_variable_set(THREAD_BULKHEAD_DISABLED_VAR, true)
    yield
  ensure
    thread.thread_variable_set(THREAD_BULKHEAD_DISABLED_VAR, old_value)
  end

  def resources
    return @resources if defined?(@resources) && @resources

    @reset_mutex.synchronize do
      @resources ||= LRUHash.new
    end
  end

  def consumers
    return @consumers if defined?(@consumers) && @consumers

    @reset_mutex.synchronize do
      @consumers ||= ConcurrentMap.new
    end
  end

  private

  def create_dual_circuit_breaker(name, **options)
    return if ENV.key?("SEMIAN_CIRCUIT_BREAKER_DISABLED")

    raise UnsupportedCircuitBreakerError,
      "Semian resource #{name}: dual_circuit_breaker (semian 0.28.3 DualCircuitBreaker) is not modeled by this runtime"
  end

  def create_adaptive_circuit_breaker(name, is_child: false, **options)
    return if ENV.key?("SEMIAN_CIRCUIT_BREAKER_DISABLED")

    raise UnsupportedCircuitBreakerError,
      "Semian resource #{name}: adaptive_circuit_breaker (semian 0.28.3 AdaptiveCircuitBreaker) is not modeled by this runtime"
  end

  def create_circuit_breaker(name, is_child: false, **options)
    return if ENV.key?("SEMIAN_CIRCUIT_BREAKER_DISABLED")
    return unless options.fetch(:circuit_breaker, true)

    exceptions = options[:exceptions] || []
    CircuitBreaker.new(
      name,
      success_threshold: options[:success_threshold],
      error_threshold: options[:error_threshold],
      error_threshold_timeout: options[:error_threshold_timeout],
      error_timeout: options[:error_timeout],
      error_threshold_timeout_enabled: if options[:error_threshold_timeout_enabled].nil?
                                         true
                                       else
                                         options[:error_threshold_timeout_enabled]
                                       end,
      lumping_interval: if options[:lumping_interval].nil?
                          0
                        else
                          options[:lumping_interval]
                        end,
      exceptions: Array(exceptions) + [::Semian::BaseError],
      half_open_resource_timeout: options[:half_open_resource_timeout],
      exponential_backoff_error_timeout: options[:exponential_backoff_error_timeout] || false,
      exponential_backoff_initial_timeout: options[:exponential_backoff_initial_timeout] || 1,
      exponential_backoff_multiplier: options[:exponential_backoff_multiplier] || 2,
      implementation: implementation(**options),
    )
  end

  def implementation(**options)
    unless options[:thread_safety_disabled].nil?
      logger.info(
        "NOTE: thread_safety_disabled will be replaced by a global setting" \
          "Semian is thread safe by default. It is possible" \
          "to modify the value by using Semian.thread_safe=",
      )
    end

    thread_safe = options[:thread_safety_disabled].nil? ? Semian.thread_safe? : !options[:thread_safety_disabled]
    thread_safe ? ::Semian::ThreadSafe : ::Semian::Simple
  end

  def create_bulkhead(name, **options)
    return if ENV.key?("SEMIAN_BULKHEAD_DISABLED") || bulkheads_disabled_in_thread?(Thread.current)
    return unless options.fetch(:bulkhead, true)

    permissions = options[:permissions] || default_permissions
    timeout = options[:timeout] || 0
    ::Semian::Resource.new(
      name,
      tickets: options[:tickets],
      quota: options[:quota],
      permissions: permissions,
      timeout: timeout,
    )
  end

  public

  # lib/semian/resource.rb. The extension replaces every `redefinable`
  # method below with its SysV implementation.
  class Resource # :nodoc:
    attr_reader :name

    class << Semian::Resource
      # Ensure that there can only be one resource of a given type
      def instance(name, **kwargs)
        Semian.resources[name] ||= ProtectedResource.new(name, new(name, **kwargs), nil)
      end

      private

      def redefinable(method_name)
        alias_method(method_name, method_name) # Silence method redefinition warnings
      end
    end

    def initialize(name, tickets: nil, quota: nil, permissions: Semian.default_permissions, timeout: 0)
      unless name.is_a?(String) || name.is_a?(Symbol)
        raise TypeError, "name must be a string or symbol, got: #{name.class}"
      end

      if Semian.semaphores_enabled?
        if respond_to?(:initialize_semaphore)
          initialize_semaphore("#{Semian.namespace}#{name}", tickets, quota, permissions, timeout)
        end
      else
        Semian.issue_disabled_semaphores_warning
      end
      @name = name
    end

    redefinable def reset_registered_workers!
    end

    redefinable def destroy
    end

    redefinable def unregister_worker
    end

    redefinable def acquire(*)
      wait_time = 0
      yield wait_time
    end

    redefinable def acquire_semaphore
      0
    end

    redefinable def release_semaphore
    end

    redefinable def count
      0
    end

    redefinable def tickets
      0
    end

    redefinable def registered_workers
      0
    end

    redefinable def semid
      0
    end

    redefinable def key
      "0x00000000"
    end

    redefinable def in_use?
      false
    end
  end

  # lib/semian/circuit_breaker_behaviour.rb
  module CircuitBreakerBehaviour
    attr_reader :name, :last_error
    attr_accessor :exceptions

    def initialize_behaviour(name:)
      @name = name.to_sym
      @last_error = nil
    end

    def acquire(resource = nil, scope: nil, adapter: nil, &block)
      raise NotImplementedError, "#{self.class} must implement #acquire"
    end

    def reset(scope: nil, adapter: nil)
      raise NotImplementedError, "#{self.class} must implement #reset"
    end

    def destroy
      raise NotImplementedError, "#{self.class} must implement #destroy"
    end

    def open?
      raise NotImplementedError, "#{self.class} must implement #open?"
    end

    def closed?
      raise NotImplementedError, "#{self.class} must implement #closed?"
    end

    def half_open?
      raise NotImplementedError, "#{self.class} must implement #half_open?"
    end

    def request_allowed?
      raise NotImplementedError, "#{self.class} must implement #request_allowed?"
    end

    def mark_failed(error, scope: nil, adapter: nil)
      raise NotImplementedError, "#{self.class} must implement #mark_failed"
    end

    def mark_success(scope: nil, adapter: nil)
      raise NotImplementedError, "#{self.class} must implement #mark_success"
    end

    def in_use?
      raise NotImplementedError, "#{self.class} must implement #in_use?"
    end
  end

  # lib/semian/simple_sliding_window.rb
  module SlidingWindowBehavior
    attr_reader :max_size

    def size
      @window.size
    end

    def last
      @window.last
    end

    def empty?
      @window.empty?
    end

    def reject!(&block)
      @window.reject!(&block)
      self
    end

    def push(value)
      resize_to(@max_size - 1) # make room
      @window << value
      self
    end
    alias_method :<<, :push

    def clear
      @window.clear
      self
    end
    alias_method :destroy, :clear

    private

    def resize_to(size)
      @window = @window.last(size) if @window.size >= size
    end
  end

  module Simple
    class SlidingWindow # :nodoc:
      include SlidingWindowBehavior

      def initialize(max_size:)
        @max_size = max_size
        @window = []
      end
    end

    # lib/semian/simple_integer.rb
    class Integer # :nodoc:
      attr_accessor :value

      def initialize
        reset
      end

      def increment(val = 1)
        @value += val
      end

      def reset
        @value = 0
      end

      def destroy
        reset
      end
    end

    # lib/semian/simple_state.rb
    class State # :nodoc:
      def initialize
        reset
      end

      attr_reader :value

      def open?
        value == :open
      end

      def closed?
        value == :closed
      end

      def half_open?
        value == :half_open
      end

      def open!
        @value = :open
      end

      def close!
        @value = :closed
      end

      def half_open!
        @value = :half_open
      end

      def reset
        close!
      end

      def destroy
        reset
      end
    end
  end

  module ThreadSafe
    # Concurrent::Array is ::Array on CRuby.
    class SlidingWindow
      include SlidingWindowBehavior

      def initialize(max_size:)
        @max_size = max_size
        @window = []
      end
    end

    # Concurrent::AtomicFixnum on CRuby: a mutex-guarded Integer.
    class Integer
      def initialize
        @lock = ::Thread::Mutex.new
        @value = 0
      end

      def value
        @lock.synchronize { @value }
      end

      def value=(new_value)
        @lock.synchronize { @value = new_value }
      end

      def increment(val = 1)
        @lock.synchronize { @value += val }
      end

      def reset
        self.value = 0
      end

      def destroy
        reset
      end
    end

    class State < Simple::State
      # These operations are already safe for a threaded environment since it's
      # a simple assignment.
    end
  end

  # lib/semian/circuit_breaker.rb
  class CircuitBreaker # :nodoc:
    include CircuitBreakerBehaviour

    def closed?
      @state.closed?
    end

    def open?
      @state.open?
    end

    def half_open?
      @state.half_open?
    end

    attr_reader(
      :half_open_resource_timeout,
      :error_timeout,
      :state,
      :error_threshold_timeout_enabled,
      :exponential_backoff_error_timeout,
      :exponential_backoff_initial_timeout,
      :exponential_backoff_multiplier,
    )

    def initialize(name, exceptions:, success_threshold:, error_threshold:,
      error_timeout:, implementation:, half_open_resource_timeout: nil,
      error_threshold_timeout: nil, error_threshold_timeout_enabled: true,
      lumping_interval: 0, exponential_backoff_error_timeout: false,
      exponential_backoff_initial_timeout: 1, exponential_backoff_multiplier: 2)
      initialize_behaviour(name: name)

      @exceptions = exceptions
      @success_count_threshold = success_threshold
      @error_count_threshold = error_threshold
      @error_threshold_timeout = error_threshold_timeout || error_timeout
      @error_threshold_timeout_enabled = error_threshold_timeout_enabled.nil? ? true : error_threshold_timeout_enabled
      @error_timeout = error_timeout
      @half_open_resource_timeout = half_open_resource_timeout
      @lumping_interval = lumping_interval
      @exponential_backoff_error_timeout = exponential_backoff_error_timeout
      @exponential_backoff_initial_timeout = exponential_backoff_initial_timeout
      @exponential_backoff_multiplier = exponential_backoff_multiplier
      @current_error_timeout = exponential_backoff_error_timeout ? exponential_backoff_initial_timeout : error_timeout

      @errors = implementation::SlidingWindow.new(max_size: @error_count_threshold)
      @successes = implementation::Integer.new
      @state = implementation::State.new

      reset
    end

    def acquire(resource = nil, scope: nil, adapter: nil, &block)
      transition_to_half_open(scope: scope, adapter: adapter) if transition_to_half_open?

      raise OpenCircuitError unless request_allowed?

      result = nil
      begin
        result = maybe_with_half_open_resource_timeout(resource, &block)
      rescue *@exceptions => error
        if !error.respond_to?(:marks_semian_circuits?) || error.marks_semian_circuits?
          mark_failed(error, scope: scope, adapter: adapter)
        end
        raise error
      else
        mark_success(scope: scope, adapter: adapter)
      end
      result
    end

    def transition_to_half_open?
      open? && error_timeout_expired? && !half_open?
    end

    def request_allowed?
      closed? || half_open? || transition_to_half_open?
    end

    def mark_failed(error, scope: nil, adapter: nil)
      push_error(error)
      if closed?
        transition_to_open(scope: scope, adapter: adapter) if error_threshold_reached?
      elsif half_open?
        transition_to_open(scope: scope, adapter: adapter)
      end
    end

    def mark_success(scope: nil, adapter: nil)
      return unless half_open?

      @successes.increment
      transition_to_close(scope: scope, adapter: adapter) if success_threshold_reached?
    end

    def reset(scope: nil, adapter: nil)
      @errors.clear
      @successes.reset
      transition_to_close(scope: scope, adapter: adapter)
    end

    def destroy
      @errors.destroy
      @successes.destroy
      @state.destroy
    end

    def in_use?
      !error_timeout_expired? && !@errors.empty?
    end

    private

    def transition_to_close(scope: nil, adapter: nil)
      notify_state_transition(:closed, scope: scope, adapter: adapter)
      log_state_transition(:closed)
      @state.close!
      @errors.clear
      # Reset exponential backoff when circuit closes
      @current_error_timeout = @exponential_backoff_error_timeout ? @exponential_backoff_initial_timeout : @error_timeout
    end

    def transition_to_open(scope: nil, adapter: nil)
      notify_state_transition(:open, scope: scope, adapter: adapter)
      log_state_transition(:open)
      @state.open!
    end

    def transition_to_half_open(scope: nil, adapter: nil)
      notify_state_transition(:half_open, scope: scope, adapter: adapter)
      log_state_transition(:half_open)
      @state.half_open!
      @successes.reset
      # Multiply the backoff timeout when circuit opens (up to the max error_timeout)
      if @exponential_backoff_error_timeout && @current_error_timeout < @error_timeout
        @current_error_timeout = [@current_error_timeout * @exponential_backoff_multiplier, @error_timeout].min
      end
    end

    def success_threshold_reached?
      @successes.value >= @success_count_threshold
    end

    def error_threshold_reached?
      @errors.size == @error_count_threshold
    end

    def error_timeout_expired?
      last_error_time = @errors.last
      return false unless last_error_time

      last_error_time + @current_error_timeout < Process.clock_gettime(Process::CLOCK_MONOTONIC)
    end

    def push_error(error)
      time = Process.clock_gettime(Process::CLOCK_MONOTONIC)

      if error_threshold_timeout_enabled
        @errors.reject! { |err_time| err_time + @error_threshold_timeout < time }
      end

      if @errors.empty? || @errors.last <= time - @lumping_interval
        @last_error = error
        @errors << time
      end
    end

    def log_state_transition(new_state)
      return if @state.nil? || new_state == @state.value

      str = "[#{self.class.name}] State transition from #{@state.value} to #{new_state}."
      str += " success_count=#{@successes.value} error_count=#{@errors.size}"
      str += " success_count_threshold=#{@success_count_threshold}"
      str += " error_count_threshold=#{@error_count_threshold}"
      str += " error_timeout=#{@error_timeout} error_last_at=\"#{@errors.last}\""
      if @exponential_backoff_error_timeout
        str += " current_error_timeout=#{@current_error_timeout}"
      end
      str += " name=\"#{@name}\""
      if new_state == :open && @last_error
        str += " last_error_message=#{@last_error.message.inspect}"
      end

      Semian.logger.info(str)
    end

    def notify_state_transition(new_state, scope: nil, adapter: nil)
      Semian.notify(:state_change, self, scope, adapter, state: new_state)
    end

    def maybe_with_half_open_resource_timeout(resource, &block)
      if half_open? && @half_open_resource_timeout && resource.respond_to?(:with_resource_timeout)
        resource.with_resource_timeout(@half_open_resource_timeout) do
          block.call
        end
      else
        block.call
      end
    end
  end

  # lib/semian/protected_resource.rb
  class ProtectedResource
    attr_reader :bulkhead, :circuit_breaker, :name
    attr_accessor :updated_at

    def initialize(name, bulkhead, circuit_breaker)
      @name = name
      @bulkhead = bulkhead
      @circuit_breaker = circuit_breaker
      @updated_at = Process.clock_gettime(Process::CLOCK_MONOTONIC)
    end

    # def_delegators :@bulkhead, :count, :semid, :tickets, :registered_workers
    def count(*args, &block)
      @bulkhead.count(*args, &block)
    end

    def semid(*args, &block)
      @bulkhead.semid(*args, &block)
    end

    def tickets(*args, &block)
      @bulkhead.tickets(*args, &block)
    end

    def registered_workers(*args, &block)
      @bulkhead.registered_workers(*args, &block)
    end

    # def_delegators :@circuit_breaker, :reset, :mark_failed, :mark_success,
    #   :request_allowed?, :open?, :closed?, :half_open?
    def reset(*args, **kwargs, &block)
      @circuit_breaker.reset(*args, **kwargs, &block)
    end

    def mark_failed(*args, **kwargs, &block)
      @circuit_breaker.mark_failed(*args, **kwargs, &block)
    end

    def mark_success(*args, **kwargs, &block)
      @circuit_breaker.mark_success(*args, **kwargs, &block)
    end

    def request_allowed?(*args, &block)
      @circuit_breaker.request_allowed?(*args, &block)
    end

    def open?(*args, &block)
      @circuit_breaker.open?(*args, &block)
    end

    def closed?(*args, &block)
      @circuit_breaker.closed?(*args, &block)
    end

    def half_open?(*args, &block)
      @circuit_breaker.half_open?(*args, &block)
    end

    def destroy
      @bulkhead&.destroy
      @circuit_breaker&.destroy
    end

    def acquire(timeout: nil, scope: nil, adapter: nil, resource: nil)
      acquire_circuit_breaker(scope, adapter, resource) do
        acquire_bulkhead(timeout, scope, adapter) do |_, wait_time|
          Semian.notify(:success, self, scope, adapter, wait_time)
          yield self
        end
      end
    end

    def in_use?
      circuit_breaker&.in_use? || bulkhead&.in_use?
    end

    private

    def acquire_circuit_breaker(scope, adapter, resource)
      if @circuit_breaker.nil?
        yield self
      else
        @circuit_breaker.acquire(resource, scope: scope, adapter: adapter) do
          yield self
        end
      end
    rescue ::Semian::OpenCircuitError
      Semian.notify(:circuit_open, self, scope, adapter)
      raise
    end

    def acquire_bulkhead(timeout, scope, adapter)
      if @bulkhead.nil?
        yield self, 0
      else
        @bulkhead.acquire(timeout: timeout) do |wait_time|
          yield self, wait_time
        end
      end
    rescue ::Semian::TimeoutError
      Semian.notify(:busy, self, scope, adapter)
      raise
    end
  end

  # lib/semian/unprotected_resource.rb
  class UnprotectedResource
    attr_reader :name
    attr_accessor :updated_at

    def initialize(name)
      @name = name
      @updated_at = Process.clock_gettime(Process::CLOCK_MONOTONIC)
    end

    def registered_workers
      0
    end

    def tickets
      -1
    end

    def destroy
    end

    def acquire(*)
      yield self
    end

    def count
      0
    end

    def semid
      0
    end

    def reset(**)
    end

    def open?
      false
    end

    def closed?
      true
    end

    def half_open?
      false
    end

    def request_allowed?
      true
    end

    def mark_failed(_error, **)
    end

    def mark_success(**)
    end

    def bulkhead
      nil
    end

    def circuit_breaker
      nil
    end

    def in_use?
      true
    end
  end

  # lib/semian/configuration_validator.rb
  class ConfigurationValidator
    def initialize(name, configuration)
      @name = name
      @configuration = configuration
      @adapter = configuration[:adapter]
      @force_config_validation = force_config_validation?
    end

    def validate!
      validate_circuit_breaker_or_bulkhead!
      validate_bulkhead_configuration!
      validate_circuit_breaker_configuration!
      validate_resource_name!
    end

    private

    def hint_format(message)
      "\n\nHINT: #{message}\n---"
    end

    def raise_or_log_validation_required!(message)
      message = "Semian Resource #{@name}: #{message}"
      if @force_config_validation
        raise ArgumentError, message
      else
        Semian.logger.warn("[SEMIAN_CONFIG_WARNING]: #{message}")
      end
    end

    def require_keys!(required, options)
      diff = required - options.keys
      unless diff.empty?
        raise_or_log_validation_required!("Missing required arguments for Semian: #{diff}")
      end
    end

    def validate_circuit_breaker_or_bulkhead!
      if (@configuration[:circuit_breaker] == false || ENV.key?("SEMIAN_CIRCUIT_BREAKER_DISABLED")) && (@configuration[:bulkhead] == false || ENV.key?("SEMIAN_BULKHEAD_DISABLED"))
        raise_or_log_validation_required!("Both bulkhead and circuitbreaker cannot be disabled.")
      end
    end

    def validate_bulkhead_configuration!
      return if ENV.key?("SEMIAN_BULKHEAD_DISABLED") || !Semian.semaphores_enabled?
      return unless @configuration.fetch(:bulkhead, true)

      tickets = @configuration[:tickets]
      quota = @configuration[:quota]

      if tickets.nil? && quota.nil?
        raise_or_log_validation_required!("Bulkhead configuration require either the :tickets or :quota parameter, you provided neither")
      end

      if tickets && quota
        raise_or_log_validation_required!("Bulkhead configuration require either the :tickets or :quota parameter, you provided both")
      end

      validate_quota!(quota) if quota
      validate_tickets!(tickets) if tickets
    end

    def validate_circuit_breaker_configuration!
      return if ENV.key?("SEMIAN_CIRCUIT_BREAKER_DISABLED")
      return unless @configuration.fetch(:circuit_breaker, true)
      return if @configuration[:adaptive_circuit_breaker] # Skip traditional validation if using adaptive

      require_keys!([:success_threshold, :error_threshold, :error_timeout], @configuration)
      validate_thresholds!
      validate_timeouts!
    end

    def validate_thresholds!
      success_threshold = @configuration[:success_threshold]
      error_threshold = @configuration[:error_threshold]

      unless success_threshold.is_a?(::Integer) && success_threshold > 0
        err = "success_threshold must be a positive integer, got #{success_threshold}"

        if success_threshold == 0
          err += hint_format("Are you sure that this is what you want? This will close the circuit breaker immediately after `error_timeout` seconds without checking the resource!")
        end

        raise_or_log_validation_required!(err)
      end

      unless error_threshold.is_a?(::Integer) && error_threshold > 0
        err = "error_threshold must be a positive integer, got #{error_threshold}"

        if error_threshold == 0
          err += hint_format("Are you sure that this is what you want? This can result in the circuit opening up at unpredictable times!")
        end

        raise_or_log_validation_required!(err)
      end
    end

    def validate_timeouts!
      error_timeout = @configuration[:error_timeout]
      error_threshold_timeout_enabled = @configuration[:error_threshold_timeout_enabled].nil? ? true : @configuration[:error_threshold_timeout_enabled]
      error_threshold = @configuration[:error_threshold]
      lumping_interval = @configuration[:lumping_interval]
      half_open_resource_timeout = @configuration[:half_open_resource_timeout]
      exponential_backoff_error_timeout = @configuration[:exponential_backoff_error_timeout]

      unless error_timeout.is_a?(Numeric) && error_timeout > 0
        err = "error_timeout must be a positive number, got #{error_timeout}"

        if error_timeout == 0
          err += hint_format("Are you sure that this is what you want? This will close the circuit breaker immediately after opening it!")
        end

        raise_or_log_validation_required!(err)
      end

      unless error_threshold_timeout_enabled || !@configuration[:error_threshold_timeout]
        err = "error_threshold_timeout_enabled and error_threshold_timeout must not contradict each other, got error_threshold_timeout_enabled: #{error_threshold_timeout_enabled}, error_threshold_timeout: #{@configuration[:error_threshold_timeout]}"
        err += hint_format("Are you sure this is what you want? This will set error_threshold_timeout_enabled to #{error_threshold_timeout_enabled} while error_threshold_timeout is #{@configuration[:error_threshold_timeout] ? "truthy" : "falsy"}")

        raise_or_log_validation_required!(err)
      end

      error_threshold_timeout = @configuration[:error_threshold_timeout] || error_timeout
      unless error_threshold_timeout.is_a?(Numeric) && error_threshold_timeout > 0
        err = "error_threshold_timeout must be a positive number, got #{error_threshold_timeout}"

        if error_threshold_timeout == 0
          err += hint_format("Are you sure that this is what you want? This will almost never open the circuit breaker since the time interval to catch errors is 0!")
        end

        raise_or_log_validation_required!(err)
      end

      unless half_open_resource_timeout.nil? || (half_open_resource_timeout.is_a?(Numeric) && half_open_resource_timeout > 0)
        err = "half_open_resource_timeout must be a positive number, got #{half_open_resource_timeout}"

        if half_open_resource_timeout == 0
          err += hint_format("Are you sure that this is what you want? This will never half-open the circuit breaker! If that's what you want, you can omit the option instead")
        end

        raise_or_log_validation_required!(err)
      end

      unless lumping_interval.nil? || (lumping_interval.is_a?(Numeric) && lumping_interval > 0)
        err = "lumping_interval must be a positive number, got #{lumping_interval}"

        if lumping_interval == 0
          err += hint_format("Are you sure that this is what you want? This will never lump errors! If that's what you want, you can omit the option instead")
        end

        raise_or_log_validation_required!(err)
      end

      unless lumping_interval.nil? || error_threshold_timeout.nil? || lumping_interval * (error_threshold - 1) <= error_threshold_timeout
        err = "constraint violated, this circuit breaker can never open! lumping_interval * (error_threshold - 1) should be <= error_threshold_timeout, got lumping_interval: #{lumping_interval}, error_threshold: #{error_threshold}, error_threshold_timeout: #{error_threshold_timeout}"
        err += hint_format("lumping_interval starts from the first error and not in a fixed window. So you can fit n errors in n-1 seconds, since error 0 starts at 0 seconds. Ensure that you can fit `error_threshold` errors lumped in `lumping_interval` seconds within `error_threshold_timeout` seconds.")

        raise_or_log_validation_required!(err)
      end

      unless exponential_backoff_error_timeout.nil? || [true, false].include?(exponential_backoff_error_timeout)
        err = "exponential_backoff_error_timeout must be a boolean, got #{exponential_backoff_error_timeout}"
        err += hint_format("Use true to enable exponential backoff for error timeout. Use false to disable.")

        raise_or_log_validation_required!(err)
      end

      exponential_backoff_initial_timeout = @configuration[:exponential_backoff_initial_timeout]
      unless exponential_backoff_initial_timeout.nil? || (exponential_backoff_initial_timeout.is_a?(Numeric) && exponential_backoff_initial_timeout > 0)
        err = "exponential_backoff_initial_timeout must be a positive number, got #{exponential_backoff_initial_timeout}"
        err += hint_format("This is the initial timeout when exponential backoff is enabled. Must be less than error_timeout.")

        raise_or_log_validation_required!(err)
      end

      exponential_backoff_multiplier = @configuration[:exponential_backoff_multiplier]
      unless exponential_backoff_multiplier.nil? || (exponential_backoff_multiplier.is_a?(Numeric) && exponential_backoff_multiplier > 1)
        err = "exponential_backoff_multiplier must be a number greater than 1, got #{exponential_backoff_multiplier}"
        err += hint_format("This is the factor by which the timeout increases on each subsequent opening. Common values are 2 (double) or 1.5.")

        raise_or_log_validation_required!(err)
      end

      unless exponential_backoff_error_timeout
        if exponential_backoff_initial_timeout
          err = "exponential_backoff_initial_timeout can only be specified when exponential_backoff_error_timeout is true"
          err += hint_format("Set exponential_backoff_error_timeout: true to use exponential backoff features.")

          raise_or_log_validation_required!(err)
        end

        if exponential_backoff_multiplier
          err = "exponential_backoff_multiplier can only be specified when exponential_backoff_error_timeout is true"
          err += hint_format("Set exponential_backoff_error_timeout: true to use exponential backoff features.")

          raise_or_log_validation_required!(err)
        end
      end

      if exponential_backoff_error_timeout && exponential_backoff_initial_timeout && exponential_backoff_initial_timeout >= error_timeout
        err = "exponential_backoff_initial_timeout (#{exponential_backoff_initial_timeout}) must be less than error_timeout (#{error_timeout})"
        err += hint_format("The initial timeout should be smaller than the maximum timeout for exponential backoff to be effective.")

        raise_or_log_validation_required!(err)
      end
    end

    def validate_quota!(quota)
      unless quota.is_a?(Numeric) && quota > 0 && quota < 1
        err = "quota must be a decimal between 0 and 1, got #{quota}"

        if quota == 0
          err += hint_format("Are you sure that this is what you want? This is the same as assigning no workers to the resource, disabling the resource!")
        elsif quota == 1
          err += hint_format("Are you sure that this is what you want? This is the same as assigning all workers to the resource, disabling the bulkhead!")
        end

        raise_or_log_validation_required!(err)
      end
    end

    def validate_tickets!(tickets)
      unless tickets.is_a?(::Integer) && tickets > 0 && tickets < Semian::MAX_TICKETS
        err = "ticket count must be a positive integer and less than #{Semian::MAX_TICKETS}, got #{tickets}"

        if tickets == 0
          err += hint_format("Are you sure that this is what you want? This is the same as assigning no workers to the resource, disabling the resource!")
        elsif tickets == Semian::MAX_TICKETS
          err += hint_format("Are you sure that this is what you want? This is the same as assigning all workers to the resource, disabling the bulkhead!")
        end

        raise_or_log_validation_required!(err)
      end
    end

    def validate_resource_name!
      unless @name.is_a?(String) || @name.is_a?(Symbol)
        raise_or_log_validation_required!("name must be a symbol or string, got #{@name}")
      end

      if Semian.resources[@name]
        err = "Resource with name #{@name} is already registered"
        err += hint_format("Are you sure that this is what you want? This will override an existing resource with the same name!")

        raise_or_log_validation_required!(err)
      end
    end

    def force_config_validation?
      if @configuration[:force_config_validation].nil?
        Semian.default_force_config_validation
      else
        @configuration[:force_config_validation]
      end
    end
  end
end

# lib/semian.rb's tail. The bulkhead is the gem's compiled SysV extension:
# only lib/semian/semian.so is loaded, after the Ruby classes it binds to.
if Semian.semaphores_enabled?
  begin
    require "semian/semian"
  rescue LoadError => error
    raise LoadError, "Semian bulkheads need the semian 0.28.3 gem's compiled extension " \
      "(semian/semian), which this tree could not load: #{error.message}. Declare the " \
      "semian gem in the Gemfile, or set SEMIAN_SEMAPHORES_DISABLED as the gem documents."
  end
  if (spec = Gem.loaded_specs["semian"]) && spec.version != Gem::Version.new("0.28.3")
    raise LoadError, "the Semian runtime port is semian 0.28.3; the loaded extension is semian #{spec.version}"
  end
else
  Semian::MAX_TICKETS = 0
end
