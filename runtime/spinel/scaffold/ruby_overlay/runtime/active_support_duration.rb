# CRuby-only ActiveSupport::Duration.
#
# Rails adds duration builders to Integer/Numeric (`70.days`, `1.week`) plus
# `Duration#ago` / `#from_now`. Reopening `Integer` in the shared, transpiled
# runtime is off-limits (no built-in subclassing — every strict target would
# have to model it, and `Time` arithmetic doesn't transpile uniformly), so
# the value type lives here and ships only to the CRuby/JRuby trees. The
# Ruby emit path rewrites `<int>.days` → `ActiveSupport::Duration.days(<int>)`
# so no `Integer` method is needed. When lobsters comes up on another target,
# that target gets its own Duration.
#
# Seconds-based. days/hours/minutes/weeks are exact; months/years use Rails'
# average-length constants (a month is 1/12 of a 365.2425-day year), so
# `1.year.ago` is within a fraction of a day of Rails' calendar arithmetic —
# close enough for the comparisons the corpus makes, not a calendar engine.
module ActiveSupport
  class Duration
    SECONDS_PER_MINUTE = 60
    SECONDS_PER_HOUR = 3600
    SECONDS_PER_DAY = 86400
    SECONDS_PER_WEEK = 604800
    SECONDS_PER_MONTH = 2629746
    SECONDS_PER_YEAR = 31556952

    # activesupport 8.2 keeps the parts a duration was built from
    # (`2.days` is `{days: 2}`), which `inspect` names ("2 days") and `+`
    # adds up. Here one slot per PART_NAMES entry, in Rails' PARTS order,
    # nil where the duration has no such part.
    PART_NAMES = ["years", "months", "weeks", "days", "hours", "minutes", "seconds"].freeze

    def initialize(seconds, parts = nil, order = nil)
      @seconds = seconds
      @parts = parts || [nil, nil, nil, nil, nil, nil, nil]
      # The order the parts were added in, as Rails' parts Hash keeps
      # it: `since`/`ago` move a time by each in that order (from Jan 30,
      # `1.month + 1.day` reaches Mar 1 and `1.day + 1.month` Feb 29).
      @order = order || Duration.present_slots(@parts)
      # Rails drops the zero parts, unless the whole value is zero
      # (`1.day - 1.day` inspects as "0 days").
      if seconds != 0
        kept = []
        @parts.each { |v| kept << (v == 0 ? nil : v) }
        @parts = kept
        live = []
        @order.each { |i| live << i unless @parts[i].nil? }
        @order = live
      end
    end

    # The PART_NAMES indices `parts` holds, in PARTS order.
    def self.present_slots(parts)
      order = []
      i = 0
      while i < 7
        order << i unless parts[i].nil?
        i += 1
      end
      order
    end

    # The parts of a duration built from one unit: `index` into PART_NAMES.
    def self.single_part(index, n)
      parts = [nil, nil, nil, nil, nil, nil, nil]
      parts[index] = n
      parts
    end

    def self.seconds(n) = new(n, single_part(6, n))
    def self.second(n) = new(n, single_part(6, n))
    def self.minutes(n) = new(n * SECONDS_PER_MINUTE, single_part(5, n))
    def self.minute(n) = new(n * SECONDS_PER_MINUTE, single_part(5, n))
    def self.hours(n) = new(n * SECONDS_PER_HOUR, single_part(4, n))
    def self.hour(n) = new(n * SECONDS_PER_HOUR, single_part(4, n))
    def self.days(n) = new(n * SECONDS_PER_DAY, single_part(3, n))
    def self.day(n) = new(n * SECONDS_PER_DAY, single_part(3, n))
    def self.weeks(n) = new(n * SECONDS_PER_WEEK, single_part(2, n))
    def self.week(n) = new(n * SECONDS_PER_WEEK, single_part(2, n))
    def self.fortnights(n) = new(n * SECONDS_PER_WEEK * 2, single_part(2, n * 2))
    def self.fortnight(n) = new(n * SECONDS_PER_WEEK * 2, single_part(2, n * 2))
    def self.months(n) = new(n * SECONDS_PER_MONTH, single_part(1, n))
    def self.month(n) = new(n * SECONDS_PER_MONTH, single_part(1, n))
    def self.years(n) = new(n * SECONDS_PER_YEAR, single_part(0, n))
    def self.year(n) = new(n * SECONDS_PER_YEAR, single_part(0, n))

    # activesupport 8.2: `since` (`from_now`, `after`) and `ago`
    # (`until`, `before`) move `time` by each part in the order the parts
    # were added (`sum`). Seconds, minutes and hours add their seconds;
    # days, weeks, months and years move the calendar date and keep the
    # wall clock (`Time#advance`): `1.month.since(Jan 31)` is Feb 29, and
    # `1.day` across a DST change is still the same local time. A
    # duration with no parts moves by its value.
    # `ActiveSupport.now`, not `Time.now`, is the default: the runtime's
    # one clock, so `travel_to` in a test moves what `70.days.ago` means.
    # See the TEST CLOCK block in active_support_time_parsing.rb.
    def since(time = ActiveSupport.now)
      sum(1, time)
    end

    def from_now(time = ActiveSupport.now)
      sum(1, time)
    end

    def after(time = ActiveSupport.now)
      sum(1, time)
    end

    def ago(time = ActiveSupport.now)
      sum(-1, time)
    end

    def until(time = ActiveSupport.now)
      sum(-1, time)
    end

    def before(time = ActiveSupport.now)
      sum(-1, time)
    end

    # Rails' `Duration#sum`. A Date moves as `Date#advance` and
    # `Date#since` (its `to_time`, as activesupport without a `Time.zone`)
    # move it; once a clock part made it a Time, the rest move a Time.
    def sum(sign, time)
      unless time.is_a?(Time) || (time.respond_to?(:jd) && time.respond_to?(:>>))
        raise ArgumentError, "expected a time or date, got #{time.inspect}"
      end
      return Duration.move_seconds(time, sign * @seconds, !time.is_a?(Time)) if @order.empty?
      @order.each do |i|
        n = sign * @parts[i]
        date = !time.is_a?(Time)
        time =
          if i == 6
            Duration.move_seconds(time, n, date)
          elsif i == 5
            Duration.move_seconds(time, n * 60, date)
          elsif i == 4
            Duration.move_seconds(time, n * 3600, date)
          elsif date
            Duration.advance_date(time, i, n)
          else
            Duration.advance_time(time, i, n)
          end
      end
      time
    end

    def self.move_seconds(time, seconds, date)
      date ? time.to_time + seconds : time + seconds
    end

    # `Date#advance` by one calendar part.
    def self.advance_date(date, index, n)
      if index == 0
        date >> n * 12
      elsif index == 1
        date >> n
      elsif index == 2
        date + n * 7
      else
        date + n
      end
    end

    # activesupport 8.2 `Time#advance` by one calendar part (`index`
    # into PART_NAMES, 0 to 3): a fraction of a week is days, a fraction
    # of a day is hours; the date moves as `Date#>>` (a month count is
    # floored, the day clamps to the month's last) and `Date#+`, the
    # time keeps its wall clock (`Time#change`), and the hours add last.
    def self.advance_time(time, index, n)
      months = 0
      days = 0
      hours = 0
      if index == 0
        months = n * 12
      elsif index == 1
        months = n
      elsif index == 2
        weeks, partial_weeks = n.divmod(1)
        whole_days, partial_days = (7 * partial_weeks).divmod(1)
        days = weeks * 7 + whole_days
        hours = 24 * partial_days
      else
        days, partial_days = n.divmod(1)
        hours = 24 * partial_days
      end
      year = time.year
      month = time.month
      day = time.day
      if months != 0
        total = (year * 12 + month - 1 + months).floor
        year = total.div(12)
        month = total % 12 + 1
        last = days_in_month(year, month)
        day = last if day > last
      end
      if days != 0
        civil = jd_to_civil(civil_to_jd(year, month, day) + days.to_i)
        year = civil[0]
        month = civil[1]
        day = civil[2]
      end
      moved = change_date(time, year, month, day)
      seconds = hours * 3600
      seconds == 0 ? moved : moved + seconds
    end

    # activesupport 8.2 `Time#change(year:, month:, day:)`: the same wall
    # clock on another date, in UTC, the local zone (keeping `isdst`, as
    # `Time.local`'s ten-argument form does) or the same fixed offset.
    def self.change_date(time, year, month, day)
      sec = time.sec + time.subsec
      if time.utc?
        Time.utc(year, month, day, time.hour, time.min, sec)
      elsif time.zone
        Time.local(sec, time.min, time.hour, day, month, year, nil, nil, time.isdst, nil)
      else
        Time.new(year, month, day, time.hour, time.min, sec, time.utc_offset)
      end
    end

    def self.days_in_month(year, month)
      return 29 if month == 2 && (year % 4 == 0 && (year % 100 != 0 || year % 400 == 0))
      [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31][month - 1]
    end

    # Proleptic Gregorian day numbers (`Date#gregorian`), so the
    # calendar moves without the date library.
    def self.civil_to_jd(year, month, day)
      a = (14 - month) / 12
      y = year + 4800 - a
      m = month + 12 * a - 3
      day + (153 * m + 2) / 5 + 365 * y + y / 4 - y / 100 + y / 400 - 32045
    end

    def self.jd_to_civil(jd)
      a = jd + 32044
      b = (4 * a + 3) / 146097
      c = a - 146097 * b / 4
      d = (4 * c + 3) / 1461
      e = c - 1461 * d / 4
      m = (5 * e + 2) / 153
      [100 * b + d - 4800 + m / 10, m + 3 - 12 * (m / 10), e - (153 * m + 2) / 5 + 1]
    end

    def to_i = @seconds.to_i
    def to_f = @seconds.to_f
    # activesupport 8.2 duration.rb: `alias :in_seconds :to_i`, and the
    # `in_<unit>` readers divide the value by the unit's float length.
    def in_seconds = to_i
    def in_minutes = @seconds / SECONDS_PER_MINUTE.to_f
    def in_hours = @seconds / SECONDS_PER_HOUR.to_f
    def in_days = @seconds / SECONDS_PER_DAY.to_f
    def in_weeks = @seconds / SECONDS_PER_WEEK.to_f
    def in_months = @seconds / SECONDS_PER_MONTH.to_f
    def in_years = @seconds / SECONDS_PER_YEAR.to_f
    # Rails parity: Duration#to_s is the seconds value's to_s
    # (`30.minutes.to_s == "1800"`) — also what interpolation calls
    # (lobsters embeds durations in cache keys).
    def to_s = @seconds.to_s
    def seconds = @seconds

    def part_slots
      @parts
    end

    def part_order
      @order
    end

    # activesupport 8.2 `Duration#+`: a Duration adds its value and its
    # parts unit by unit; a number adds to the value and to the seconds
    # part. `Time + Duration` is Time's (the CRuby overlay's reopen).
    def +(other)
      if other.is_a?(Duration)
        theirs = other.part_slots
        parts = []
        i = 0
        while i < 7
          mine = @parts[i]
          other_part = theirs[i]
          parts << (mine.nil? ? other_part : (other_part.nil? ? mine : mine + other_part))
          i += 1
        end
        order = @order.dup
        other.part_order.each { |k| order << k unless order.include?(k) }
        Duration.new(@seconds + other.seconds, parts, order)
      else
        parts = @parts.dup
        parts[6] = (parts[6] || 0) + other
        order = @order.dup
        order << 6 unless order.include?(6)
        Duration.new(@seconds + other, parts, order)
      end
    end

    # Rails: `self + (-other)`.
    def -(other)
      self + (-other)
    end

    def -@
      parts = []
      @parts.each { |v| parts << (v.nil? ? nil : -v) }
      Duration.new(-@seconds, parts, @order.dup)
    end

    # activesupport 8.2 `Duration#inspect`: its parts in PARTS order, a
    # unit of 1 singular, joined as `to_sentence(locale: false)` joins
    # ("1 day, 2 hours, and 5 minutes"); "<value> seconds" with none.
    def inspect
      words = []
      i = 0
      while i < 7
        v = @parts[i]
        unless v.nil?
          unit = PART_NAMES[i]
          words << "#{v} #{v == 1 ? unit.chop : unit}"
        end
        i += 1
      end
      return "#{@seconds} seconds" if words.empty?
      return words[0] if words.length == 1
      return "#{words[0]} and #{words[1]}" if words.length == 2
      "#{words[0, words.length - 1].join(", ")}, and #{words[words.length - 1]}"
    end

    # Rails parity: a Duration IS its seconds count for equality, so
    # `1.year == 31556952` is true and `assert_equal 1.year, n` passes
    # against a plain Integer, and `1.day == 24.hours` whatever the parts.
    def ==(other)
      other.is_a?(Duration) ? @seconds == other.seconds : @seconds == other
    end

    def eql?(other)
      other.is_a?(Duration) && other.seconds.eql?(@seconds)
    end

    def hash
      @seconds.hash
    end

    # activesupport 8.2: ordered by value against a Duration or a
    # Numeric, nil against anything else.
    def <=>(other)
      if other.is_a?(Duration)
        @seconds <=> other.seconds
      elsif other.is_a?(Numeric)
        @seconds <=> other
      end
    end

    # Rails' Duration has no Comparable: `<` and the rest reach its value
    # through method_missing, so `1.day < "x"` raises "comparison of
    # Integer with String failed". spinel's net/http reads a campfire
    # webhook's `open_timeout = 7.seconds` as `limit <= 0`. Spelled out
    # so every operator is a plain method on the spinel lane.
    def <(other)
      @seconds < Duration.value_of(other)
    end

    def <=(other)
      @seconds <= Duration.value_of(other)
    end

    def >(other)
      @seconds > Duration.value_of(other)
    end

    def >=(other)
      @seconds >= Duration.value_of(other)
    end

    def self.value_of(other)
      other.is_a?(Duration) ? other.seconds : other
    end

    # Numeric comparison protocol: `Time.current - created_at <=
    # 14.days` has a Float receiver, and Ruby's numeric `<=>` resolves
    # an unknown operand through its `coerce`: the duration pairs up as
    # its seconds value (AS Duration is coercible the same way).
    def coerce(other)
      [other, @seconds]
    end
  end
end

# ActiveSupport patches `Time#<=>` (compare_with_coercion) so a non-Time
# operand routes through `to_datetime <=> other`; for a bare Duration that
# resolves to comparing the astronomical julian day NUMBER against the
# duration's bare seconds VALUE (pinned empirically against activesupport
# 8.1: `Time.at(0) <= 2440587.seconds` → false, `<= 2440589.seconds` →
# true — flip exactly at ajd == seconds). Dimensionally meaningless, but
# lobsters ships `created_at <= 1.hour` (story.rb `send_referrer?`, a
# dormant `.ago`-less bug) and on Rails it evaluates — always false for
# realistic timestamps — instead of raising. Mirror the arithmetic so the
# benchmark sees identical behavior. 210_866_760_000 = 2440587.5 * 86400
# (unix epoch's ajd, in seconds).
class Time
  # `Time.current` — AS's zone-aware now. The bench runs zoneless
  # (config.time_zone default UTC, TZ-naive comparisons throughout), so
  # plain `now` is the same instant.
  def self.current
    now
  end

  alias_method :roundhouse_compare_without_duration, :<=>
  def <=>(other)
    if other.is_a?(ActiveSupport::Duration)
      ajd = (to_r + 210_866_760_000r) / 86_400r
      ajd <=> other.seconds
    else
      roundhouse_compare_without_duration(other)
    end
  end

  # Time ± Duration → Time, activesupport 8.2's plus_with_duration and
  # minus_with_duration: `Duration#since(self)` / `#until(self)`, so the
  # calendar parts keep the wall clock (`Time.utc(2024, 1, 31) + 1.month`
  # is Feb 29). The commentbox preview window does `Time.current -
  # 90.seconds`.
  alias_method :roundhouse_minus_without_duration, :-
  def -(other)
    return other.until(self) if other.is_a?(ActiveSupport::Duration)
    roundhouse_minus_without_duration(other)
  end

  alias_method :roundhouse_plus_without_duration, :+
  def +(other)
    return other.since(self) if other.is_a?(ActiveSupport::Duration)
    roundhouse_plus_without_duration(other)
  end

  # AS's readable comparators (`story.created_at.after?(cutoff)`).
  def after?(other)
    self > other
  end

  def before?(other)
    self < other
  end
end
