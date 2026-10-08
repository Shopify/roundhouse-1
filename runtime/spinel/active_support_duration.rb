# ActiveSupport::Duration for the spinel (AOT) tree.
#
# The value class HALF of the CRuby overlay's
# `ruby_overlay/runtime/active_support_duration.rb` — same constants,
# same builders, same seconds-based arithmetic. What it deliberately
# omits is that file's `class Time` reopen (Duration-aware `<=>`/`-`/`+`
# via `alias_method` and Rational math): reopening a built-in's
# operators is CRuby-only forever, and spinel's subset rejects the
# Rational literals it leans on. Sites that need Time±Duration under
# AOT get grounded at emit instead (`.ago`/`.from_now` live here and
# cover the corpus's dominant shape).
#
# On the CRuby/JRuby trees the overlay file replaces this one wholesale
# (same emitted path, dedupe-last-wins), so the two never coexist.
#
# Seconds-based. days/hours/minutes/weeks are exact; months/years use
# Rails' average-length constants (a month is 1/12 of a 365.2425-day
# year) — close enough for the comparisons the corpus makes, not a
# calendar engine.
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

    def self.seconds(n)
      new(n, single_part(6, n))
    end

    def self.second(n)
      new(n, single_part(6, n))
    end

    def self.minutes(n)
      new(n * SECONDS_PER_MINUTE, single_part(5, n))
    end

    def self.minute(n)
      new(n * SECONDS_PER_MINUTE, single_part(5, n))
    end

    def self.hours(n)
      new(n * SECONDS_PER_HOUR, single_part(4, n))
    end

    def self.hour(n)
      new(n * SECONDS_PER_HOUR, single_part(4, n))
    end

    def self.days(n)
      new(n * SECONDS_PER_DAY, single_part(3, n))
    end

    def self.day(n)
      new(n * SECONDS_PER_DAY, single_part(3, n))
    end

    def self.weeks(n)
      new(n * SECONDS_PER_WEEK, single_part(2, n))
    end

    def self.week(n)
      new(n * SECONDS_PER_WEEK, single_part(2, n))
    end

    def self.fortnights(n)
      new(n * SECONDS_PER_WEEK * 2, single_part(2, n * 2))
    end

    def self.fortnight(n)
      new(n * SECONDS_PER_WEEK * 2, single_part(2, n * 2))
    end

    def self.months(n)
      new(n * SECONDS_PER_MONTH, single_part(1, n))
    end

    def self.month(n)
      new(n * SECONDS_PER_MONTH, single_part(1, n))
    end

    def self.years(n)
      new(n * SECONDS_PER_YEAR, single_part(0, n))
    end

    def self.year(n)
      new(n * SECONDS_PER_YEAR, single_part(0, n))
    end

    # A Time minus seconds is a Time, so `ago`/`from_now` (and the
    # Rails aliases `until`/`since`) stay Time-typed for callers like
    # `created_at > 70.days.ago`.
    # `ActiveSupport.now`, not `Time.now` — the runtime's one clock, so
    # `travel_to` in a test moves what `70.days.ago` means. See the
    # TEST CLOCK block in active_support_time_parsing.rb.
    def ago
      ActiveSupport.now.utc - @seconds
    end

    def until
      ActiveSupport.now.utc - @seconds
    end

    def from_now
      ActiveSupport.now.utc + @seconds
    end

    def since
      ActiveSupport.now.utc + @seconds
    end

    def to_i
      @seconds.to_i
    end

    def to_f
      @seconds.to_f
    end

    # activesupport 8.2 duration.rb: `alias :in_seconds :to_i`, and the
    # `in_<unit>` readers divide the value by the unit's float length.
    def in_seconds
      to_i
    end

    def in_minutes
      @seconds / SECONDS_PER_MINUTE.to_f
    end

    def in_hours
      @seconds / SECONDS_PER_HOUR.to_f
    end

    def in_days
      @seconds / SECONDS_PER_DAY.to_f
    end

    def in_weeks
      @seconds / SECONDS_PER_WEEK.to_f
    end

    def in_months
      @seconds / SECONDS_PER_MONTH.to_f
    end

    def in_years
      @seconds / SECONDS_PER_YEAR.to_f
    end

    # Rails parity: Duration#to_s is the seconds VALUE's to_s
    # (`30.minutes.to_s == "1800"`). Also what string interpolation
    # calls — lobsters' cache keys embed durations
    # (`"aggregates_#{interval}_#{cache_time}"`), and matching Rails
    # here keeps those keys byte-identical.
    def to_s
      @seconds.to_s
    end

    def seconds
      @seconds
    end

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

    # Rails parity, the overlay's rule: a Duration IS its seconds count
    # for equality, so `assert_equal 1.year, response.cache_control
    # [:max_age].to_i` (campfire's QR code test) holds against an
    # Integer. Ruby's `!=` is the negation of this.
    def ==(other)
      other.is_a?(Duration) ? @seconds == other.seconds : @seconds == other
    end

    # Ordering, the same rule: Rails' Duration compares as its seconds
    # against a Numeric or another Duration. spinel's net/http reads a
    # campfire webhook's `open_timeout = 7.seconds` as `limit <= 0`
    # before it connects, which raised "comparison of Duration with 0
    # failed" the moment #4492 let that path be reached. Spelled out
    # rather than through `Comparable` so every operator is a plain
    # method on this lane.
    def <=>(other)
      @seconds <=> (other.is_a?(Duration) ? other.seconds : other)
    end

    def <(other)
      (self <=> other) < 0
    end

    def <=(other)
      (self <=> other) <= 0
    end

    def >(other)
      (self <=> other) > 0
    end

    def >=(other)
      (self <=> other) >= 0
    end
  end
end
