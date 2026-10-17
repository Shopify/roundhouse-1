# frozen_string_literal: true
# Run-time controller filters (actionpack 8.x abstract_controller/callbacks.rb):
# `before_action` / `prepend_before_action` / `after_action` /
# `around_action` / `skip_*_action` called on a controller class AT RUN
# TIME, which is how a Concern's class macro registers them —
# core's `Cors::ClassMethods#allow_insecure_cross_origin_access` ends in
# `prepend_before_action(opts) { handle_cross_origin_access(...) }`.
#
# A filter written literally in a controller body (`before_action
# :authenticate`) is not a registry entry: lowering folds it into the
# synthesized `process_action` (`lower::controller_to_library`). What
# the lowering cannot see is a filter a macro call registers when the
# class body runs. Those land here, in each class's own list, and the
# dispatcher reaches them through the four hooks Base defines:
#
#   run_prepended_filters(action)  runtime `prepend_before_action`s,
#                                  ahead of every lowered filter
#   run_appended_filters(action)   the remaining runtime before filters,
#                                  after the lowered ones
#   run_around_filters(action) {}  runtime arounds, innermost, around
#                                  the action's `case`
#   run_after_filters(action)      runtime afters, before the lowered
#                                  afters (reverse declaration order)
#
# The dispatcher only calls them from a controller whose chain has a
# class-body macro call the lowering did not consume. A lowered filter's
# declaration site is not recorded, so the order inside one chain is:
# runtime prepends, lowered prepends, lowered filters in declaration
# order, runtime filters in declaration order (ancestors first). Rails
# interleaves them by declaration position.
#
# Chain composition follows Rails: a class sees its parent's chain,
# then applies its own declarations in order — a `prepend_*` goes to
# the front, an append to the back, a `skip_*_action NAME` removes
# (or, with `only:`/`except:`, narrows) the entry of that name. A before
# filter halts the chain once `performed?`. Dynamic Ruby (procs,
# instance_exec, send), so ruby lane only; the typed no-op hooks live in
# runtime/ruby/action_controller/base.rb.
module ActionController
  module RuntimeFilters
    # One registered filter. `target` is a Symbol (a controller method)
    # or a Proc (the block, or a lambda passed as the target).
    class Entry
      attr_reader :kind, :target, :prepend
      attr_accessor :only, :except, :if_conds, :unless_conds

      def initialize(kind, target, prepend, only, except, if_conds, unless_conds)
        @kind = kind
        @target = target
        @prepend = prepend
        @only = only
        @except = except
        @if_conds = if_conds
        @unless_conds = unless_conds
      end

      def copy
        Entry.new(@kind, @target, @prepend, @only.dup, @except.dup, @if_conds, @unless_conds)
      end

      # The guards that hold for `controller` handling `action`.
      def applies?(controller, action)
        return false unless @only.empty? || @only.include?(action)
        return false if @except.include?(action)
        return false unless @if_conds.all? { |c| RuntimeFilters.condition_holds?(controller, c) }
        @unless_conds.none? { |c| RuntimeFilters.condition_holds?(controller, c) }
      end
    end

    # A skip declaration, replayed over the parent's chain.
    class Skip
      attr_reader :kind, :target, :only, :except

      def initialize(kind, target, only, except)
        @kind = kind
        @target = target
        @only = only
        @except = except
      end
    end

    def self.condition_holds?(controller, cond)
      case cond
      when Symbol, String
        controller.send(cond)
      when Proc
        cond.arity == 0 ? controller.instance_exec(&cond) : controller.instance_exec(controller, &cond)
      else
        cond.respond_to?(:call) ? cond.call(controller) : cond
      end
    end

    def self.symbols(value)
      Array(value).map { |v| v.to_s }
    end

    # `controller.process_action`'s call into one filter.
    def self.invoke(controller, entry)
      target = entry.target
      if target.is_a?(Proc)
        target.arity == 0 ? controller.instance_exec(&target) : controller.instance_exec(controller, &target)
      else
        controller.send(target)
      end
    end

    module ClassMethods
      %i[before after around].each do |kind|
        define_method(:"#{kind}_action") do |*names, &block|
          _register_runtime_filter(kind, names, block, false)
        end
        define_method(:"prepend_#{kind}_action") do |*names, &block|
          _register_runtime_filter(kind, names, block, true)
        end
        define_method(:"append_#{kind}_action") do |*names, &block|
          _register_runtime_filter(kind, names, block, false)
        end
        define_method(:"skip_#{kind}_action") do |*names|
          options = names.last.is_a?(Hash) ? names.pop : {}
          only = RuntimeFilters.symbols(options[:only])
          except = RuntimeFilters.symbols(options[:except])
          names.each { |name| _runtime_filter_declarations << Skip.new(kind, name.to_sym, only, except) }
          nil
        end
      end

      # This class's own declarations, in order: Entry and Skip.
      def _runtime_filter_declarations
        @_runtime_filter_declarations ||= []
      end

      # The chain a request to this class runs: the parent's chain with
      # this class's declarations applied in order.
      def _runtime_filter_chain
        me = self #: untyped
        parent = me.superclass #: untyped
        chain = parent.respond_to?(:_runtime_filter_chain) ? parent._runtime_filter_chain.map(&:copy) : []
        _runtime_filter_declarations.each do |decl|
          if decl.is_a?(Entry)
            decl.prepend ? chain.unshift(decl.copy) : chain.push(decl.copy)
          else
            chain = _apply_runtime_skip(chain, decl)
          end
        end
        chain
      end

      private

      # Rails' `_insert_callbacks`: the names, then a trailing options
      # Hash (a positional one, as `before_action(opts) { … }` passes
      # it), then the block.
      def _register_runtime_filter(kind, names, block, prepend)
        names = names.dup
        options = names.last.is_a?(Hash) ? names.pop : {}
        # `before_action :a, prepend: true` is `prepend_before_action :a`.
        prepend ||= options[:prepend] ? true : false
        only = RuntimeFilters.symbols(options[:only])
        except = RuntimeFilters.symbols(options[:except])
        if_conds = Array(options[:if])
        unless_conds = Array(options[:unless])
        targets = names.map { |n| n.is_a?(Proc) ? n : n.to_sym }
        targets << block if block
        # `before_action :a, :b` prepends as a unit, so the first name
        # stays first (each one is unshifted in reverse).
        targets = targets.reverse if prepend
        targets.each do |t|
          _runtime_filter_declarations << Entry.new(kind, t, prepend, only, except, if_conds, unless_conds)
        end
        nil
      end

      def _apply_runtime_skip(chain, skip)
        chain.filter_map do |entry|
          next entry unless entry.kind == skip.kind && entry.target == skip.target
          next nil if skip.only.empty? && skip.except.empty?
          entry.except |= skip.only
          unless skip.except.empty?
            entry.only = entry.only.empty? ? skip.except : (entry.only & skip.except)
            next nil if entry.only.empty?
          end
          entry
        end
      end
    end

    module InstanceMethods
      def run_prepended_filters(action_name)
        _run_runtime_before(action_name, true)
      end

      def run_appended_filters(action_name)
        _run_runtime_before(action_name, false)
      end

      # Arounds nest in chain order: the first is outermost. The block
      # is the action; a filter that never yields skips it, as in Rails.
      def run_around_filters(action_name, &action)
        action_s = action_name.to_s
        arounds = _runtime_chain.select { |e| e.kind == :around && e.applies?(self, action_s) }
        arounds.reverse.inject(action) do |inner, entry|
          lambda do
            target = entry.target
            if target.is_a?(Proc)
              instance_exec(self, inner, &target)
            else
              send(target, &inner)
            end
          end
        end.call
        nil
      end

      # After filters run in reverse declaration order.
      def run_after_filters(action_name)
        action_s = action_name.to_s
        afters = _runtime_chain.select { |e| e.kind == :after && e.applies?(self, action_s) }
        afters.reverse_each { |entry| RuntimeFilters.invoke(self, entry) }
        nil
      end

      private

      def _runtime_chain
        klass = self.class #: untyped
        klass.respond_to?(:_runtime_filter_chain) ? klass._runtime_filter_chain : []
      end

      def _run_runtime_before(action_name, prepended)
        action_s = action_name.to_s
        _runtime_chain.each do |entry|
          next unless entry.kind == :before && entry.prepend == prepended
          next unless entry.applies?(self, action_s)
          RuntimeFilters.invoke(self, entry)
          break if performed?
        end
        nil
      end
    end
  end

  class Base
    extend RuntimeFilters::ClassMethods
    # Prepended: Base defines typed no-op hooks of the same names for the
    # targets that carry no registry, and a method on the class itself
    # would outrank an included module.
    prepend RuntimeFilters::InstanceMethods
  end
end
