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

    def initialize(seconds, parts = nil)
      @seconds = seconds
      @parts = parts || [nil, nil, nil, nil, nil, nil, nil]
      # Rails drops the zero parts, unless the whole value is zero
      # (`1.day - 1.day` inspects as "0 days").
      if seconds != 0
        kept = []
        @parts.each { |v| kept << (v == 0 ? nil : v) }
        @parts = kept
      end
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

    # A Time minus seconds is a Time in Ruby, so `ago`/`from_now` (and the
    # Rails aliases `until`/`since`) stay Time-typed for callers like
    # `created_at > 70.days.ago`.
    # `ActiveSupport.now`, not `Time.now` — the runtime's one clock, so
    # `travel_to` in a test moves what `70.days.ago` means. See the
    # TEST CLOCK block in active_support_time_parsing.rb.
    def ago = ActiveSupport.now - @seconds
    def until = ActiveSupport.now - @seconds
    def from_now = ActiveSupport.now + @seconds
    def since = ActiveSupport.now + @seconds

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
        Duration.new(@seconds + other.seconds, parts)
      else
        parts = @parts.dup
        parts[6] = (parts[6] || 0) + other
        Duration.new(@seconds + other, parts)
      end
    end

    # Rails: `self + (-other)`.
    def -(other)
      self + (-other)
    end

    def -@
      parts = []
      @parts.each { |v| parts << (v.nil? ? nil : -v) }
      Duration.new(-@seconds, parts)
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
    # against a plain Integer. `coerce` below covers `<`/`<=`; equality
    # does not route through it (Ruby's `==` has no coercion protocol),
    # which is why this is its own method and not a consequence of that.
    def ==(other)
      other.is_a?(Duration) ? @seconds == other.seconds : @seconds == other
    end

    # Numeric comparison protocol: `Time.current - created_at <=
    # 14.days` has a Float receiver, and Ruby's numeric `<=>` resolves
    # an unknown operand through its `coerce` — pair the duration up as
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

  # Time ± Duration → Time (AS arithmetic; the commentbox preview
  # window does `Time.current - 90.seconds`).
  alias_method :roundhouse_minus_without_duration, :-
  def -(other)
    other = other.seconds if other.is_a?(ActiveSupport::Duration)
    roundhouse_minus_without_duration(other)
  end

  alias_method :roundhouse_plus_without_duration, :+
  def +(other)
    other = other.seconds if other.is_a?(ActiveSupport::Duration)
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
