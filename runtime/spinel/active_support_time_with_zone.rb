# frozen_string_literal: true

# ActiveSupport::TimeWithZone and the core extensions that make a Time
# zone-aware (Shopify/rails 5813559f4aed, 8.2.0.alpha activesupport/lib/
# active_support/time_with_zone.rb, time_formats.rb, core_ext/time/
# zones.rb, core_ext/date_and_time/zones.rb, core_ext/date/zones.rb,
# core_ext/string/zones.rb, core_ext/object/acts_like.rb and the
# acts_like files, core_ext/time/conversions.rb's Time#to_fs and
# #formatted_offset, date_formats.rb with Date's and DateTime's #to_fs and #inspect,
# String#to_date and #to_datetime, and the parts of core_ext/time/calculations.rb,
# core_ext/date_time/calculations.rb, core_ext/date/calculations.rb,
# core_ext/date/conversions.rb, core_ext/date_and_time/calculations.rb
# and core_ext/string/conversions.rb that TimeWithZone calls), verbatim
# where ported; Rails' doc comments dropped. Required at the end of
# runtime/active_support_time_zone.rb, as Rails' active_support/all has
# both loaded at boot.
#
# Time.zone is execution-local (ActiveSupport::IsolatedExecutionState,
# runtime/active_support_isolated_execution_state.rb) over
# Time.zone_default, which the boot sets from config.time_zone as Rails'
# active_support.initialize_time_zone initializer does.
#
# Differences, all at the edges:
# - Of core_ext/time/calculations only Time.===, .current, .at's
#   coercion, .days_in_month, .days_in_year, #advance, #since, #ago,
#   #in, the beginning_of/end_of day, hour and minute methods,
#   #seconds_since_midnight, #seconds_until_end_of_day, #sec_fraction,
#   the coercing #<=>, #eql? and #-, and #next_/#prev_ day, month and
#   year are ported; #change is runtime/active_support_time_change.rb.
#   Time#+ and #- with a Duration are the overlay's
#   (runtime/active_support_duration.rb). Of DateAndTime::Calculations
#   only #past?, #future?, #today?, #tomorrow?, #yesterday?,
#   #next_day? and #prev_day? are.
# - TimeFormats' lambdas are plain lambdas (the tree has no
#   ActiveSupport::Ractors), and Time::DATE_FORMATS' deprecation is not
#   ported.
# - `Time.current` with no Time.zone and TimeZone#now read the tree's
#   clock (`ActiveSupport.clock`, Time.now moved by `travel`), which
#   `travel_to`'s Time.now stub also moves.
# - The tree's lowered time helpers (runtime/active_support_ext.rb's
#   `ActiveSupport.present`, `.local_time`, `.in_time_zone`, `.use_zone`,
#   `.current_zone` and the beginning_of/end_of helpers, which compiled
#   code calls for `Time.current`, an attribute read, `in_time_zone`
#   and `Time.use_zone`) answer through Time.zone here, as the methods
#   they stand for do in Rails: a TimeWithZone, or a Time where no zone
#   is set.
# - #to_json goes through ActiveSupport::JSON.encode as
#   ToJsonWithActiveSupportEncoder does, but only for TimeWithZone
#   (runtime/active_support_json.rb reroutes no core class's to_json).
# - Duration's calendar parts move a TimeWithZone as TimeWithZone#advance
#   does (its wall clock, then the zone's period), and Duration#variable?
#   is defined (Rails' duration.rb).
#
# The PostgreSQL CRuby tree only (`project::apply_postgres_bootstrap`).

require "date"
require "time"
require "json"
require "yaml"
require_relative "active_support_isolated_execution_state"

# core_ext/object/acts_like.rb and the acts_like files.
class Object
  def acts_like?(duck)
    case duck
    when :time
      respond_to? :acts_like_time?
    when :date
      respond_to? :acts_like_date?
    when :string
      respond_to? :acts_like_string?
    else
      respond_to? :"acts_like_#{duck}?"
    end
  end
end

class Time
  def acts_like_time?
    true
  end
end

class Date
  def acts_like_date?
    true
  end
end

class DateTime
  def acts_like_date?
    true
  end

  def acts_like_time?
    true
  end
end

# time_formats.rb.
module ActiveSupport
  module TimeFormats
    @list = {
      db: "%Y-%m-%d %H:%M:%S",
      inspect: "%Y-%m-%d %H:%M:%S.%9N %z",
      number: "%Y%m%d%H%M%S",
      nsec: "%Y%m%d%H%M%S%9N",
      usec: "%Y%m%d%H%M%S%6N",
      time: "%H:%M",
      short: "%d %b %H:%M",
      long: "%B %d, %Y %H:%M",
      long_ordinal: lambda { |time|
        day_format = ActiveSupport::Inflector.ordinalize(time.day)
        time.strftime("%B #{day_format}, %Y %H:%M")
      },
      rfc822: lambda { |time|
        offset_format = time.formatted_offset(false)
        time.strftime("%a, %d %b %Y %H:%M:%S #{offset_format}")
      },
      rfc2822: lambda { |time| time.rfc2822 },
      iso8601: lambda { |time| time.iso8601 }
    }.freeze

    singleton_class.attr_reader :list # :nodoc:

    DEPRECATED_LIST = @list.dup # :nodoc:

    def self.lookup(format) # :nodoc:
      @list[format] || DEPRECATED_LIST[format]
    end

    def self.register(name, format)
      @list = @list.merge(name => format).freeze
    end
  end
end

# core_ext/time/conversions.rb.
class Time
  def to_fs(format = :default)
    if formatter = ::ActiveSupport::TimeFormats.lookup(format)
      formatter.respond_to?(:call) ? formatter.call(self).to_s : strftime(formatter)
    else
      to_s
    end
  end
  alias_method :to_formatted_s, :to_fs

  def formatted_offset(colon = true, alternate_utc_string = nil)
    utc? && alternate_utc_string || ActiveSupport::TimeZone.seconds_to_utc_offset(utc_offset, colon)
  end

  alias_method :rfc3339, :xmlschema
end

# duration.rb's Duration#variable?, over the overlay's parts (years,
# months, weeks, days are slots 0 to 3).
module ActiveSupport
  class Duration
    def variable?
      (0..3).any? { |i| !part_slots[i].nil? }
    end

    class << self
      alias_method :advance_time_without_zone, :advance_time

      # TimeWithZone#advance by a calendar part: its wall clock moves,
      # then the zone resolves it (`wrap_with_time_zone`).
      def advance_time(time, index, n)
        return advance_time_without_zone(time, index, n) unless time.instance_of?(ActiveSupport::TimeWithZone)
        time.__send__(:wrap_with_time_zone, advance_time_without_zone(time.time, index, n))
      end
    end
  end
end

# time_with_zone.rb.
module ActiveSupport
  class TimeWithZone
    PRECISIONS = Hash.new { |h, n| h[n] = "%FT%T.%#{n}N" }
    PRECISIONS[0] = "%FT%T"

    include Comparable
    attr_reader :time_zone

    def initialize(utc_time, time_zone, local_time = nil, period = nil)
      @time_zone, @time = time_zone, local_time
      if utc_time
        @utc = transfer_time_values_to_utc_constructor(utc_time)
        @period = period
      else
        @utc = nil
        @period = get_period_and_ensure_valid_local_time(period)
      end
      @is_utc = zone == "UTC" || zone == "UCT"
    end

    def time
      @time ||= incorporate_utc_offset(@utc, utc_offset)
    end

    def utc
      @utc ||= incorporate_utc_offset(@time, -utc_offset)
    end
    alias_method :comparable_time, :utc
    alias_method :getgm, :utc
    alias_method :getutc, :utc
    alias_method :gmtime, :utc

    def period
      @period ||= time_zone.period_for_utc(@utc)
    end

    def in_time_zone(new_zone = ::Time.zone)
      return self if time_zone == new_zone
      utc.in_time_zone(new_zone)
    end

    def localtime(utc_offset = nil)
      utc.getlocal(utc_offset)
    end
    alias_method :getlocal, :localtime

    def dst?
      period.dst?
    end
    alias_method :isdst, :dst?

    def utc?
      @is_utc
    end
    alias_method :gmt?, :utc?

    def utc_offset
      period.observed_utc_offset
    end
    alias_method :gmt_offset, :utc_offset
    alias_method :gmtoff, :utc_offset

    def formatted_offset(colon = true, alternate_utc_string = nil)
      utc? && alternate_utc_string || TimeZone.seconds_to_utc_offset(utc_offset, colon)
    end

    def zone
      period.abbreviation
    end

    def inspect
      "#{time.strftime('%F %H:%M:%S.%9N')} #{zone} #{formatted_offset}"
    end

    def xmlschema(fraction_digits = 0)
      precision = fraction_digits || 0
      if @is_utc
        utc.iso8601(precision)
      else
        transfer_time_values_to_utc_constructor(utc).getlocal(utc_offset).iso8601(precision)
      end
    end
    alias_method :iso8601, :xmlschema
    alias_method :rfc3339, :xmlschema

    # ActiveSupport::JSON::Encoding's settings where the tree carries it
    # (runtime/active_support_json.rb); otherwise their defaults.
    def as_json(options = nil)
      encoding = defined?(ActiveSupport::JSON::Encoding) && ActiveSupport::JSON::Encoding.respond_to?(:time_precision) ? ActiveSupport::JSON::Encoding : nil
      if encoding.nil? || encoding.use_standard_json_time_format
        xmlschema(encoding ? encoding.time_precision : 3).force_encoding(Encoding::UTF_8)
      else
        %(#{time.strftime("%Y/%m/%d %H:%M:%S")} #{formatted_offset(false)})
      end
    end

    # core_ext/object/json's ToJsonWithActiveSupportEncoder, for this
    # class only (the tree reroutes no other to_json): the encoder where
    # the tree carries it, else as_json's JSON string.
    def to_json(options = nil)
      return super if defined?(::JSON::State) && options.is_a?(::JSON::State)
      if defined?(ActiveSupport::JSON) && ActiveSupport::JSON.respond_to?(:encode)
        ActiveSupport::JSON.encode(self, options)
      else
        ::JSON.generate(as_json(options))
      end
    end

    def init_with(coder) # :nodoc:
      initialize(coder["utc"], coder["zone"], coder["time"])
    end

    def encode_with(coder) # :nodoc:
      coder.map = { "utc" => utc, "zone" => time_zone, "time" => time }
    end

    def httpdate
      utc.httpdate
    end

    def rfc2822
      to_fs(:rfc822)
    end
    alias_method :rfc822, :rfc2822

    def to_s
      if @is_utc
        transfer_time_values_to_utc_constructor(utc).to_s
      else
        transfer_time_values_to_utc_constructor(utc).getlocal(utc_offset).to_s
      end
    end

    def to_fs(format = :default)
      if format == :db
        utc.to_fs(format)
      elsif formatter = ::ActiveSupport::TimeFormats.lookup(format)
        formatter.respond_to?(:call) ? formatter.call(self).to_s : strftime(formatter)
      else
        to_s
      end
    end
    alias_method :to_formatted_s, :to_fs

    def strftime(format)
      format = format.gsub(/((?:\A|[^%])(?:%%)*)%Z/, "\\1#{zone}")
      getlocal(utc_offset).strftime(format)
    end

    def <=>(other)
      utc <=> other
    end
    alias_method :before?, :<
    alias_method :after?, :>

    def between?(min, max)
      utc.between?(min, max)
    end

    def past?
      utc.past?
    end

    def today?
      time.today?
    end

    def tomorrow?
      time.tomorrow?
    end
    alias :next_day? :tomorrow?

    def yesterday?
      time.yesterday?
    end
    alias :prev_day? :yesterday?

    def future?
      utc.future?
    end

    def eql?(other)
      other.eql?(utc)
    end

    def hash
      utc.hash
    end

    def +(other)
      if duration_of_variable_length?(other)
        method_missing(:+, other)
      else
        result = utc + other
        result.in_time_zone(time_zone)
      end
    end
    alias_method :since, :+
    alias_method :in, :+

    def -(other)
      if other.acts_like?(:time)
        getutc - other.getutc
      elsif duration_of_variable_length?(other)
        method_missing(:-, other)
      else
        result = utc - other
        result.in_time_zone(time_zone)
      end
    end

    def ago(other)
      since(-other)
    end

    def change(options)
      if options[:zone] && options[:offset]
        raise ArgumentError, "Can't change both :offset and :zone at the same time: #{options.inspect}"
      end

      new_time = time.change(options)

      if options[:zone]
        new_zone = ::Time.find_zone(options[:zone])
      elsif options[:offset]
        new_zone = ::Time.find_zone(new_time.utc_offset)
      end

      new_zone ||= time_zone
      periods = new_zone.periods_for_local(new_time)

      self.class.new(nil, new_zone, new_time, periods.include?(period) ? period : nil)
    end

    def advance(options)
      if options.values_at(:years, :weeks, :months, :days).any?
        method_missing(:advance, options)
      else
        utc.advance(options).in_time_zone(time_zone)
      end
    end

    %w(year mon month day mday wday yday hour min sec usec nsec to_date).each do |method_name|
      class_eval <<~RUBY, __FILE__, __LINE__ + 1
        def #{method_name}    # def month
          time.#{method_name} #   time.month
        end                   # end
      RUBY
    end

    def to_a
      [time.sec, time.min, time.hour, time.day, time.mon, time.year, time.wday, time.yday, dst?, zone]
    end

    def to_f
      utc.to_f
    end

    def to_i
      utc.to_i
    end
    alias_method :tv_sec, :to_i

    def to_r
      utc.to_r
    end

    def to_datetime
      @to_datetime ||= utc.to_datetime.new_offset(Rational(utc_offset, 86_400))
    end

    def to_time
      @to_time_with_timezone ||= getlocal(time_zone)
    end

    def acts_like_time?
      true
    end

    def is_a?(klass)
      klass == ::Time || super
    end
    alias_method :kind_of?, :is_a?

    def blank?
      false
    end

    def present? # :nodoc:
      true
    end

    def freeze
      period; utc; time; to_datetime; to_time
      super
    end

    def marshal_dump
      [utc, time_zone.name, time]
    end

    def marshal_load(variables)
      initialize(variables[0].utc, ::Time.find_zone(variables[1]), variables[2].utc)
    end

    def respond_to_missing?(sym, include_priv)
      time.respond_to?(sym, include_priv)
    end

    def method_missing(...)
      wrap_with_time_zone time.__send__(...)
    rescue NoMethodError => e
      raise e, e.message.sub(time.inspect, inspect).sub("Time", "ActiveSupport::TimeWithZone"), e.backtrace
    end

    private
      SECONDS_PER_DAY = 86400

      def incorporate_utc_offset(time, offset)
        if offset.zero?
          time
        elsif time.kind_of?(Date)
          time + Rational(offset, SECONDS_PER_DAY)
        else
          time + offset
        end
      end

      def get_period_and_ensure_valid_local_time(period)
        @time = transfer_time_values_to_utc_constructor(@time) unless @time.utc?
        begin
          period || @time_zone.period_for_local(@time)
        rescue ::TZInfo::PeriodNotFound
          @time += 3600
          retry
        end
      end

      def transfer_time_values_to_utc_constructor(time)
        return time if time.instance_of?(::Time) && time.utc?
        ::Time.utc(time.year, time.month, time.day, time.hour, time.min, time.sec + time.subsec)
      end

      def duration_of_variable_length?(obj)
        ActiveSupport::Duration === obj && obj.variable?
      end

      def wrap_with_time_zone(time)
        if time.acts_like?(:time)
          periods = time_zone.periods_for_local(time)
          self.class.new(nil, time_zone, time, periods.include?(period) ? period : nil)
        elsif time.is_a?(Range)
          wrap_with_time_zone(time.begin)..wrap_with_time_zone(time.end)
        else
          time
        end
      end
  end
end

YAML.load_tags["!ruby/object:ActiveSupport::TimeWithZone"] = "ActiveSupport::TimeWithZone"
YAML.dump_tags[ActiveSupport::TimeWithZone] = "!ruby/object:ActiveSupport::TimeWithZone"

# core_ext/date_and_time/zones.rb.
module DateAndTime
  module Zones
    def in_time_zone(zone = ::Time.zone)
      time_zone = ::Time.find_zone! zone
      time = acts_like?(:time) ? self : nil

      if time_zone
        time_with_zone(time, time_zone)
      else
        time || to_time
      end
    end

    private
      def time_with_zone(time, zone)
        if time
          ActiveSupport::TimeWithZone.new(time.utc? ? time : time.getutc, zone)
        else
          ActiveSupport::TimeWithZone.new(nil, zone, to_time(:utc))
        end
      end
  end

  # core_ext/date_and_time/calculations.rb: the predicates TimeWithZone
  # delegates to.
  module Calculations
    def yesterday?
      prev_day.to_date == ::Date.current
    end
    alias :prev_day? :yesterday?

    def today?
      to_date == ::Date.current
    end

    def tomorrow?
      next_day.to_date == ::Date.current
    end
    alias :next_day? :tomorrow?

    def past?
      self < self.class.current
    end

    def future?
      self > self.class.current
    end
  end
end

# core_ext/time/zones.rb.
class Time
  include DateAndTime::Zones

  class << self
    attr_accessor :zone_default

    def zone
      ::ActiveSupport::IsolatedExecutionState[:time_zone] || zone_default
    end

    def zone=(time_zone)
      ::ActiveSupport::IsolatedExecutionState[:time_zone] = find_zone!(time_zone)
    end

    def use_zone(time_zone)
      new_zone = find_zone!(time_zone)
      begin
        old_zone, ::Time.zone = ::Time.zone, new_zone
        yield
      ensure
        ::Time.zone = old_zone
      end
    end

    def find_zone!(time_zone)
      return time_zone unless time_zone

      ActiveSupport::TimeZone[time_zone] || raise(ArgumentError, "Invalid Timezone: #{time_zone}")
    end

    def find_zone(time_zone)
      find_zone!(time_zone) rescue nil
    end
  end
end

# core_ext/date/zones.rb, and Date#to_time (core_ext/date/conversions.rb),
# Date.current and Date#advance (core_ext/date/calculations.rb).
class Date
  include DateAndTime::Zones
  include DateAndTime::Calculations

  class << self
    def current
      ::Time.zone ? ::Time.zone.today : ::Date.today
    end
  end

  def to_time(form = :local)
    raise ArgumentError, "Expected :local or :utc, got #{form.inspect}." unless [:local, :utc].include?(form)
    ::Time.public_send(form, year, month, day)
  end

  def prev_day(days = 1)
    self - days
  end unless method_defined?(:prev_day)

  def next_day(days = 1)
    self + days
  end unless method_defined?(:next_day)

  def advance(options)
    d = self

    d = d >> options[:years] * 12 if options[:years]
    d = d >> options[:months] if options[:months]
    d = d + options[:weeks] * 7 if options[:weeks]
    d = d + options[:days] if options[:days]

    d
  end
end

# core_ext/date_time/calculations.rb: the UTC and comparison methods a
# DateTime's in_time_zone and comparisons go through.
class DateTime
  class << self
    def current
      ::Time.zone ? ::Time.zone.now.to_datetime : ::Time.now.to_datetime
    end
  end

  def utc
    utc = new_offset(0)

    Time.utc(
      utc.year, utc.month, utc.day,
      utc.hour, utc.min, utc.sec + utc.sec_fraction
    )
  end
  alias_method :getgm, :utc
  alias_method :getutc, :utc
  alias_method :gmtime, :utc

  def utc?
    offset == 0
  end

  def utc_offset
    (offset * 86400).to_i
  end

  def <=>(other)
    if other.respond_to? :to_datetime
      super other.to_datetime rescue nil
    else
      super
    end
  end
end

# date_formats.rb, and Date#to_fs and #inspect (core_ext/date/
# conversions.rb) and DateTime's, with its #formatted_offset (core_ext/
# date_time/conversions.rb).
module ActiveSupport
  module DateFormats
    @list = {
      short: "%d %b",
      long: "%B %d, %Y",
      db: "%Y-%m-%d",
      inspect: "%Y-%m-%d",
      number: "%Y%m%d",
      long_ordinal: lambda { |date|
        day_format = ActiveSupport::Inflector.ordinalize(date.day)
        date.strftime("%B #{day_format}, %Y")
      },
      rfc822: "%d %b %Y",
      rfc2822: "%d %b %Y",
      iso8601: lambda { |date| date.iso8601 }
    }.freeze

    singleton_class.attr_reader :list # :nodoc:

    DEPRECATED_LIST = @list.dup # :nodoc:

    def self.lookup(format) # :nodoc:
      @list[format] || DEPRECATED_LIST[format]
    end

    def self.register(name, format)
      @list = @list.merge(name => format).freeze
    end
  end
end

class Date
  def to_fs(format = :default)
    if formatter = ActiveSupport::DateFormats.lookup(format)
      if formatter.respond_to?(:call)
        formatter.call(self).to_s
      else
        strftime(formatter)
      end
    else
      to_s
    end
  end
  alias_method :to_formatted_s, :to_fs

  def readable_inspect
    strftime("%a, %d %b %Y")
  end
  alias_method :default_inspect, :inspect
  alias_method :inspect, :readable_inspect
end

class DateTime
  def to_fs(format = :default)
    if formatter = ::ActiveSupport::TimeFormats.lookup(format)
      formatter.respond_to?(:call) ? formatter.call(self).to_s : strftime(formatter)
    else
      to_s
    end
  end
  alias_method :to_formatted_s, :to_fs

  def formatted_offset(colon = true, alternate_utc_string = nil)
    utc? && alternate_utc_string || ActiveSupport::TimeZone.seconds_to_utc_offset(utc_offset, colon)
  end

  def readable_inspect
    to_fs(:rfc822)
  end
  alias_method :default_inspect, :inspect
  alias_method :inspect, :readable_inspect
end

# core_ext/string/zones.rb, over String#to_time, #to_date and
# #to_datetime (core_ext/string/conversions.rb). `blank?` is String's
# (core_ext/object/blank.rb: whitespace only), spelled out because the
# tree's Object#blank? reads `empty?`.
class String
  def to_time(form = :local)
    parts = Date._parse(self, false)
    used_keys = %i(year mon mday hour min sec sec_fraction offset)
    return if !parts.keys.intersect?(used_keys)

    now = Time.now
    time = Time.new(
      parts.fetch(:year, now.year),
      parts.fetch(:mon, now.month),
      parts.fetch(:mday, now.day),
      parts.fetch(:hour, 0),
      parts.fetch(:min, 0),
      parts.fetch(:sec, 0) + parts.fetch(:sec_fraction, 0),
      parts.fetch(:offset, form == :utc ? 0 : nil)
    )

  form == :utc ? time.utc : time.to_time
end

def to_date
  ::Date.parse(self, false) unless match?(/\A[[:space:]]*\z/)
end

def to_datetime
  ::DateTime.parse(self, false) unless match?(/\A[[:space:]]*\z/)
end

  def in_time_zone(zone = ::Time.zone)
    if zone
      ::Time.find_zone!(zone).parse(self)
    else
      to_time
    end
  end
end

# core_ext/time/calculations.rb, the parts listed above.
class Time
  include DateAndTime::Calculations

  COMMON_YEAR_DAYS_IN_MONTH = [nil, 31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31].freeze

  class << self
    def ===(other)
      super || (self == Time && other.is_a?(ActiveSupport::TimeWithZone))
    end

    def days_in_month(month, year = current.year)
      if month == 2 && ::Date.gregorian_leap?(year)
        29
      else
        COMMON_YEAR_DAYS_IN_MONTH[month]
      end
    end

    def days_in_year(year = current.year)
      days_in_month(2, year) + 337
    end

    def current
      ::Time.zone ? ::Time.zone.now : (ActiveSupport.respond_to?(:clock) ? ActiveSupport.clock : ::Time.now)
    end

    def at_with_coercion(time_or_number, *args, **kwargs)
      return at_without_coercion(time_or_number, *args, **kwargs) unless args.empty? && kwargs.empty?

      case time_or_number
      when ActiveSupport::TimeWithZone
        at_without_coercion(time_or_number.to_r).getlocal
      when DateTime
        at_without_coercion(time_or_number.to_i + time_or_number.sec_fraction).getlocal
      else
        at_without_coercion(time_or_number)
      end
    end
    alias_method :at_without_coercion, :at
    alias_method :at, :at_with_coercion
  end

  def seconds_since_midnight
    to_i - change(hour: 0).to_i + (usec / 1.0e+6)
  end

  def seconds_until_end_of_day
    end_of_day.to_i - to_i
  end

  def sec_fraction
    subsec
  end

  def advance(options)
    options = options.dup

    unless options[:weeks].nil?
      options[:weeks], partial_weeks = options[:weeks].divmod(1)
      options[:days] = options.fetch(:days, 0) + 7 * partial_weeks
    end

    unless options[:days].nil?
      options[:days], partial_days = options[:days].divmod(1)
      options[:hours] = options.fetch(:hours, 0) + 24 * partial_days
    end

    d = to_date.gregorian.advance(options)
    time_advanced_by_date = change(year: d.year, month: d.month, day: d.day)
    seconds_to_advance = \
      options.fetch(:seconds, 0) +
      options.fetch(:minutes, 0) * 60 +
      options.fetch(:hours, 0) * 3600

    if seconds_to_advance.zero?
      time_advanced_by_date
    else
      time_advanced_by_date.since(seconds_to_advance)
    end
  end

  def ago(seconds)
    since(-seconds)
  end

  def since(seconds)
    self + seconds
  end
  alias :in :since

  def beginning_of_day
    change(hour: 0)
  end
  alias :midnight :beginning_of_day
  alias :at_midnight :beginning_of_day
  alias :at_beginning_of_day :beginning_of_day

  def middle_of_day
    change(hour: 12)
  end
  alias :midday :middle_of_day
  alias :noon :middle_of_day
  alias :at_midday :middle_of_day
  alias :at_noon :middle_of_day
  alias :at_middle_of_day :middle_of_day

  def end_of_day
    change(
      hour: 23,
      min: 59,
      sec: 59,
      usec: Rational(999999999, 1000)
    )
  end
  alias :at_end_of_day :end_of_day

  def beginning_of_hour
    change(min: 0)
  end
  alias :at_beginning_of_hour :beginning_of_hour

  def end_of_hour
    change(
      min: 59,
      sec: 59,
      usec: Rational(999999999, 1000)
    )
  end
  alias :at_end_of_hour :end_of_hour

  def beginning_of_minute
    change(sec: 0)
  end
  alias :at_beginning_of_minute :beginning_of_minute

  def end_of_minute
    change(
      sec: 59,
      usec: Rational(999999999, 1000)
    )
  end
  alias :at_end_of_minute :end_of_minute

  # Over the overlay's #- (which moves by a Duration), as Rails' chain is
  # minus_with_coercion over minus_with_duration.
  def minus_with_coercion(other)
    other = other.comparable_time if other.respond_to?(:comparable_time)
    if other.is_a?(DateTime)
      (to_r - other.to_i - other.sec_fraction).to_f
    else
      minus_without_coercion(other)
    end
  end
  alias_method :minus_without_coercion, :-
  alias_method :-, :minus_with_coercion

  # Over the overlay's #<=> (which answers Rails' result against a bare
  # Duration): a Duration goes there, anything else that is not a Time
  # through #to_datetime, as Rails does.
  def compare_with_coercion(other)
    if other.class == Time
      compare_without_coercion(other)
    elsif other.is_a?(Time)
      if other.respond_to?(:comparable_time)
        compare_without_coercion(other.comparable_time)
      else
        compare_without_coercion(other.to_time)
      end
    elsif ActiveSupport::Duration === other
      compare_without_coercion(other)
    else
      to_datetime <=> other
    end
  end
  alias_method :compare_without_coercion, :<=>
  alias_method :<=>, :compare_with_coercion

  def eql_with_coercion(other)
    other = other.comparable_time if other.respond_to?(:comparable_time)
    eql_without_coercion(other)
  end
  alias_method :eql_without_coercion, :eql?
  alias_method :eql?, :eql_with_coercion

  def prev_day(days = 1)
    advance(days: -days)
  end

  def next_day(days = 1)
    advance(days: days)
  end

  def prev_month(months = 1)
    advance(months: -months)
  end

  def next_month(months = 1)
    advance(months: months)
  end

  def prev_year(years = 1)
    advance(years: -years)
  end

  def next_year(years = 1)
    advance(years: years)
  end
end

# The tree's lowered time helpers (runtime/active_support_ext.rb), through
# Time.zone as the Rails methods they stand for.
module ActiveSupport
  # railties' active_support.initialize_time_zone initializer
  # (`Time.zone_default = Time.find_zone!(config.time_zone)`, raising for
  # an unknown zone) and activerecord's active_record.initialize_timezone
  # (`time_zone_aware_attributes = true`); the boot calls it with the
  # app's config.time_zone.
  def self.initialize_time_zone(name)
    ::Time.zone_default = ::Time.find_zone!(name)
    ActiveRecord::Base.time_zone_aware_attributes = true if defined?(ActiveRecord::Base) && ActiveRecord::Base.respond_to?(:time_zone_aware_attributes=)
  end

  class << self
    # `Time.zone`.
    def current_zone
      ::Time.zone
    end

    # `Time.use_zone(zone) { ... }`.
    def use_zone(zone, &block)
      ::Time.use_zone(zone, &block)
    end

    # A Time read as Rails presents it: `in_time_zone` (Time.current, an
    # attribute read under time_zone_aware_attributes).
    def present(t)
      t.in_time_zone
    end

    def present_db(t)
      return nil if t.nil?
      present(t)
    end

    # `t.in_time_zone(zone)`; no zone is Time.zone.
    def in_time_zone(t, zone)
      zone.nil? ? present(t) : t.in_time_zone(zone)
    end

    alias_method :local_time_without_zone, :local_time

    # `Time.zone.local(...)` where a zone is set.
    def local_time(y, mo, d, h, mi, s, nsec)
      zone = ::Time.zone
      return local_time_without_zone(y, mo, d, h, mi, s, nsec) if zone.nil?
      TimeWithZone.new(nil, zone, ::Time.utc(y, mo, d, h, mi, s + Rational(nsec, 1_000_000_000)))
    end

    # `t.beginning_of_day` and the like, in t's own zone.
    %i[beginning_of_minute end_of_minute beginning_of_hour end_of_hour beginning_of_day end_of_day noon].each do |name|
      alias_method :"#{name}_without_zone", name
      define_method(name) do |t|
        t.acts_like?(:time) && t.respond_to?(name) ? t.public_send(name) : __send__(:"#{name}_without_zone", t)
      end
    end
  end
end
