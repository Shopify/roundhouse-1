# frozen_string_literal: true

# Time#change (Shopify/rails 5813559f4aed, 8.2.0.alpha
# activesupport/lib/active_support/core_ext/time/calculations.rb:148),
# verbatim; Rails' doc comment dropped. The rest of core_ext/time/
# calculations is not ported, and a call to one of its methods stays the
# NameError active_support/all names (runtime/active_support_not_ported.rb).
# Rails' :load_active_support initializer loads it with active_support/all,
# so a class body's `Time.now.utc.change(nsec: 0)` needs no require
# (`project::BOOT_LOADED_CORE_EXTENSIONS`).
#
# The PostgreSQL CRuby tree only (`project::apply_postgres_bootstrap`).

class Time
  def change(options)
    new_year   = options.fetch(:year, year)
    new_month  = options.fetch(:month, month)
    new_day    = options.fetch(:day, day)
    new_hour   = options.fetch(:hour, hour)
    new_min    = options.fetch(:min, options[:hour] ? 0 : min)
    new_sec    = options.fetch(:sec, (options[:hour] || options[:min]) ? 0 : sec)
    new_offset = options.fetch(:offset, nil)

    if new_nsec = options[:nsec]
      raise ArgumentError, "Can't change both :nsec and :usec at the same time: #{options.inspect}" if options[:usec]
      new_usec = Rational(new_nsec, 1000)
    else
      new_usec = options.fetch(:usec, (options[:hour] || options[:min] || options[:sec]) ? 0 : Rational(nsec, 1000))
    end

    raise ArgumentError, "argument out of range" if new_usec >= 1000000

    new_sec += Rational(new_usec, 1000000)

    if new_offset
      ::Time.new(new_year, new_month, new_day, new_hour, new_min, new_sec, new_offset)
    elsif utc?
      ::Time.utc(new_year, new_month, new_day, new_hour, new_min, new_sec)
    elsif zone.respond_to?(:utc_to_local)
      new_time = ::Time.new(new_year, new_month, new_day, new_hour, new_min, new_sec, zone)

      # Some versions of Ruby have a bug where Time.new with a zone object and
      # fractional seconds will end up with a broken utc_offset.
      # This is fixed in Ruby 3.3.1 and 3.2.4
      unless new_time.utc_offset.integer?
        new_time += 0
      end

      # When there are two occurrences of a nominal time due to DST ending,
      # `Time.new` chooses the first chronological occurrence (the one with a
      # larger UTC offset). However, for `change`, we want to choose the
      # occurrence that matches this time's UTC offset.
      #
      # If the new time's UTC offset is larger than this time's UTC offset, the
      # new time might be a first chronological occurrence. So we add the offset
      # difference to fast-forward the new time, and check if the result has the
      # desired UTC offset (i.e. is the second chronological occurrence).
      offset_difference = new_time.utc_offset - utc_offset
      if offset_difference > 0 && (new_time_2 = new_time + offset_difference).utc_offset == utc_offset
        new_time_2
      else
        new_time
      end
    elsif zone
      ::Time.local(new_sec, new_min, new_hour, new_day, new_month, new_year, nil, nil, isdst, nil)
    else
      ::Time.new(new_year, new_month, new_day, new_hour, new_min, new_sec, utc_offset)
    end
  end
end
