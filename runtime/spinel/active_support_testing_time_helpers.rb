# frozen_string_literal: true

# ActiveSupport::Testing::TimeHelpers and its SimpleStubs, ported from
# activesupport 8.1.4 (lib/active_support/testing/time_helpers.rb) for the
# CRuby and JRuby trees. Carried where the app or its tests name the
# module or call a helper the harness lacks (`freeze_time`); the harness
# TestBase then includes it, as ActiveSupport::TestCase does, in place of
# its offset stand-in.
#
# Each method follows the 8.1.4 source: `travel_to` stubs `Time.now`,
# `Time.new` (no arguments), `Date.today` and `DateTime.now` on their
# singletons, so time is frozen at the target until `travel_back`.
# The runtime's own clock (`ActiveSupport.now`) reads `Time.now`, so it
# moves with them. tests/time_helpers_runtime.rb runs the same
# observations against native ActiveSupport and this file and requires
# them equal.
#
# Differences, all at the edges:
# - The core extensions the native file requires are not installed on
#   core classes; the calls are written out with the same results:
#   `silence_redefinition_of_method` (a warning silencer) becomes
#   removing the stub before restoring the original;
#   `Date#midnight.to_time` is midnight in `Time.zone` where the tree
#   defines one, and in the runtime's zone otherwise; `Time#change(usec: 0)` drops
#   the sub-second part (the instant is all `getlocal` keeps).
# - A String target is parsed by `Time.zone.parse` where the tree
#   defines `Time.zone`, and otherwise by the runtime's
#   `ActiveSupport.zone_parse`, which is that parser for the app's zone.
# - `after_teardown` calls `super` only where an ancestor defines it:
#   the CRuby harness runner calls the hook itself and TestBase has no
#   Minitest lifecycle above it.

require "date"

module ActiveSupport
  module Testing
    # Manages stubs for TimeHelpers
    class SimpleStubs # :nodoc:
      Stub = Struct.new(:object, :method_name, :original_method)

      def initialize
        @stubs = Hash.new { |h, k| h[k] = {} }
      end

      # Stubs object.method_name with the given block
      # If the method is already stubbed, remove that stub
      # so that removing this stub will restore the original implementation.
      #   Time.current # => Sat, 09 Nov 2013 15:34:49 EST -05:00
      #   target = Time.zone.local(2004, 11, 24, 1, 4, 44)
      #   simple_stubs.stub_object(Time, :now) { at(target.to_i) }
      #   Time.current # => Wed, 24 Nov 2004 01:04:44 EST -05:00
      def stub_object(object, method_name, &block)
        if stub = stubbing(object, method_name)
          unstub_object(stub)
        end

        new_name = "__simple_stub__#{method_name}__#{object_id}"

        @stubs[object.object_id][method_name] = Stub.new(object, method_name, new_name)

        object.singleton_class.alias_method new_name, method_name
        object.define_singleton_method(method_name, &block)
      end

      # Remove all object-method stubs held by this instance
      def unstub_all!
        @stubs.each_value do |object_stubs|
          object_stubs.each_value do |stub|
            unstub_object(stub)
          end
        end
        @stubs.clear
      end

      # Returns the Stub for object#method_name
      # (nil if it is not stubbed)
      def stubbing(object, method_name)
        @stubs[object.object_id][method_name]
      end

      # Returns true if any stubs are set, false if there are none
      def stubbed?
        !@stubs.empty?
      end

      private
        # Restores the original object.method described by the Stub
        def unstub_object(stub)
          singleton_class = stub.object.singleton_class
          singleton_class.send(:remove_method, stub.method_name) if singleton_class.method_defined?(stub.method_name, false) || singleton_class.private_method_defined?(stub.method_name, false)
          singleton_class.alias_method stub.method_name, stub.original_method
          singleton_class.undef_method stub.original_method
        end
    end

    # Contains helpers that help you test passage of time.
    module TimeHelpers
      def after_teardown
        travel_back
        super if defined?(super)
      end

      # Changes current time to the time in the future or in the past by a given time difference by
      # stubbing +Time.now+, +Date.today+, and +DateTime.now+. The stubs are automatically removed
      # at the end of the test.
      def travel(duration, with_usec: false, &block)
        travel_to Time.now + duration, with_usec: with_usec, &block
      end

      # Changes current time to the given time by stubbing +Time.now+, +Time.new+,
      # +Date.today+, and +DateTime.now+ to return the time or date passed into this method.
      # The stubs are automatically removed at the end of the test.
      def travel_to(date_or_time, with_usec: false)
        if block_given? && in_block
          travel_to_nested_block_call = <<~MSG

      Calling `travel_to` with a block, when we have previously already made a call to `travel_to`, can lead to confusing time stubbing.

      Instead of:

         travel_to 2.days.from_now do
           # 2 days from today
           travel_to 3.days.from_now do
             # 5 days from today
           end
         end

      preferred way to achieve above is:

         travel 2.days do
           # 2 days from today
         end

         travel 5.days do
           # 5 days from today
         end

          MSG
          raise travel_to_nested_block_call
        end

        if date_or_time.is_a?(Date) && !date_or_time.is_a?(DateTime)
          # `date_or_time.midnight.to_time` (`Date#in_time_zone`): midnight
          # in Time.zone where the tree defines one; otherwise in the
          # runtime's zone (the process zone, or the one `use_zone` set).
          date = date_or_time
          now = if Time.respond_to?(:zone) && Time.zone
            Time.zone.local(date.year, date.month, date.day).to_time
          elsif ActiveSupport.respond_to?(:local_time)
            ActiveSupport.local_time(date.year, date.month, date.day, 0, 0, 0, 0)
          else
            Time.local(date.year, date.month, date.day)
          end
        elsif date_or_time.is_a?(String)
          # `Time.zone.parse(date_or_time)`.
          now = Time.respond_to?(:zone) ? Time.zone.parse(date_or_time) : ActiveSupport.zone_parse(date_or_time)
        else
          now = date_or_time
          now = now.to_time unless now.is_a?(Time)
        end

        now = Time.at(now.to_i) unless with_usec
        now = now.getlocal

        stubs = simple_stubs
        stubbed_time = Time.now if stubs.stubbing(Time, :now)
        stubs.stub_object(Time, :now) { at(now) }

        stubs.stub_object(Time, :new) do |*args, **options|
          if args.empty? && options.empty?
            at(now)
          else
            stub = stubs.stubbing(Time, :new)
            Time.send(stub.original_method, *args, **options)
          end
        end

        stubs.stub_object(Date, :today) { jd(now.to_date.jd) }
        stubs.stub_object(DateTime, :now) { jd(now.to_date.jd, now.hour, now.min, now.sec, Rational(now.utc_offset, 86400)) }

        if block_given?
          begin
            self.in_block = true
            yield
          ensure
            if stubbed_time
              travel_to stubbed_time
            else
              travel_back
            end
            self.in_block = false
          end
        end
      end

      # Returns the current time back to its original state, by removing the stubs added by
      # +travel+, +travel_to+, and +freeze_time+.
      def travel_back
        # `Time.current`.
        stubbed_time = (Time.respond_to?(:current) ? Time.current : Time.now) if block_given? && simple_stubs.stubbed?

        simple_stubs.unstub_all!
        yield if block_given?
      ensure
        travel_to stubbed_time if stubbed_time
      end
      alias_method :unfreeze_time, :travel_back

      # Calls +travel_to+ with +date_or_time+, which defaults to +Time.now+.
      # Forwards optional <tt>with_usec</tt> argument.
      def freeze_time(date_or_time = Time.now, with_usec: false, &block)
        travel_to date_or_time, with_usec: with_usec, &block
      end

      private
        # The simple stubs are only one per thread, so the time helpers are
        # not thread safe.
        def simple_stubs
          @simple_stubs ||= SimpleStubs.new
        end

        attr_accessor :in_block
    end
  end
end
