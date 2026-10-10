# frozen_string_literal: true

# ActiveSupport::TimeZone (Shopify/rails 5813559f4aed, 8.2.0.alpha
# activesupport/lib/active_support/values/time_zone.rb) over the tzinfo
# gem, as Rails has it natively; Rails' doc comments dropped.
#
# A zone answers an ActiveSupport::TimeWithZone (`local`, `at`, `iso8601`,
# `parse`, `rfc3339`, `strptime`, `now`), as natively: TimeWithZone and
# the core_ext time/zones methods are runtime/active_support_time_with_zone.rb,
# required at the end of this file. The differences: `now` and `today`
# read the tree's clock (`ActiveSupport.clock`, Time.now moved by
# `travel`, which `travel_to`'s stub of Time.now also moves) where Rails
# reads Time.now and `tzinfo.now`; the Ractor sharing
# (`make_shareable!`) is not ported.
#
# The PostgreSQL CRuby tree only (`project::apply_postgres_bootstrap`).

require "tzinfo"
require "concurrent/map"

module ActiveSupport
  class TimeZone
    MAPPING = { # rubocop:disable Style/MutableConstant
      "International Date Line West" => "Etc/GMT+12",
      "Midway Island"                => "Pacific/Midway",
      "American Samoa"               => "Pacific/Pago_Pago",
      "Hawaii"                       => "Pacific/Honolulu",
      "Alaska"                       => "America/Juneau",
      "Pacific Time (US & Canada)"   => "America/Los_Angeles",
      "Pacific Time (Canada)"        => "America/Vancouver",
      "Tijuana"                      => "America/Tijuana",
      "Mountain Time (US & Canada)"  => "America/Denver",
      "Alberta"                      => "America/Edmonton",
      "Arizona"                      => "America/Phoenix",
      "Chihuahua"                    => "America/Chihuahua",
      "Mazatlan"                     => "America/Mazatlan",
      "Central Time (US & Canada)"   => "America/Chicago",
      "Saskatchewan"                 => "America/Regina",
      "Guadalajara"                  => "America/Mexico_City",
      "Mexico City"                  => "America/Mexico_City",
      "Monterrey"                    => "America/Monterrey",
      "Central America"              => "America/Guatemala",
      "Eastern Time (US & Canada)"   => "America/New_York",
      "Indiana (East)"               => "America/Indiana/Indianapolis",
      "Bogota"                       => "America/Bogota",
      "Lima"                         => "America/Lima",
      "Quito"                        => "America/Lima",
      "Atlantic Time (Canada)"       => "America/Halifax",
      "Caracas"                      => "America/Caracas",
      "La Paz"                       => "America/La_Paz",
      "Santiago"                     => "America/Santiago",
      "Asuncion"                     => "America/Asuncion",
      "Newfoundland"                 => "America/St_Johns",
      "Brasilia"                     => "America/Sao_Paulo",
      "Buenos Aires"                 => "America/Argentina/Buenos_Aires",
      "Montevideo"                   => "America/Montevideo",
      "Georgetown"                   => "America/Guyana",
      "Puerto Rico"                  => "America/Puerto_Rico",
      "Greenland"                    => "America/Nuuk",
      "Mid-Atlantic"                 => "Atlantic/South_Georgia",
      "Azores"                       => "Atlantic/Azores",
      "Cape Verde Is."               => "Atlantic/Cape_Verde",
      "Dublin"                       => "Europe/Dublin",
      "Edinburgh"                    => "Europe/London",
      "Lisbon"                       => "Europe/Lisbon",
      "London"                       => "Europe/London",
      "Casablanca"                   => "Africa/Casablanca",
      "Monrovia"                     => "Africa/Monrovia",
      "UTC"                          => "Etc/UTC",
      "Belgrade"                     => "Europe/Belgrade",
      "Bratislava"                   => "Europe/Bratislava",
      "Budapest"                     => "Europe/Budapest",
      "Ljubljana"                    => "Europe/Ljubljana",
      "Prague"                       => "Europe/Prague",
      "Sarajevo"                     => "Europe/Sarajevo",
      "Skopje"                       => "Europe/Skopje",
      "Warsaw"                       => "Europe/Warsaw",
      "Zagreb"                       => "Europe/Zagreb",
      "Brussels"                     => "Europe/Brussels",
      "Copenhagen"                   => "Europe/Copenhagen",
      "Madrid"                       => "Europe/Madrid",
      "Paris"                        => "Europe/Paris",
      "Amsterdam"                    => "Europe/Amsterdam",
      "Berlin"                       => "Europe/Berlin",
      "Bern"                         => "Europe/Zurich",
      "Zurich"                       => "Europe/Zurich",
      "Rome"                         => "Europe/Rome",
      "Stockholm"                    => "Europe/Stockholm",
      "Vienna"                       => "Europe/Vienna",
      "West Central Africa"          => "Africa/Algiers",
      "Bucharest"                    => "Europe/Bucharest",
      "Cairo"                        => "Africa/Cairo",
      "Helsinki"                     => "Europe/Helsinki",
      "Kyiv"                         => "Europe/Kyiv",
      "Riga"                         => "Europe/Riga",
      "Sofia"                        => "Europe/Sofia",
      "Tallinn"                      => "Europe/Tallinn",
      "Vilnius"                      => "Europe/Vilnius",
      "Athens"                       => "Europe/Athens",
      "Istanbul"                     => "Europe/Istanbul",
      "Minsk"                        => "Europe/Minsk",
      "Jerusalem"                    => "Asia/Jerusalem",
      "Harare"                       => "Africa/Harare",
      "Pretoria"                     => "Africa/Johannesburg",
      "Kaliningrad"                  => "Europe/Kaliningrad",
      "Moscow"                       => "Europe/Moscow",
      "St. Petersburg"               => "Europe/Moscow",
      "Volgograd"                    => "Europe/Volgograd",
      "Samara"                       => "Europe/Samara",
      "Kuwait"                       => "Asia/Kuwait",
      "Riyadh"                       => "Asia/Riyadh",
      "Nairobi"                      => "Africa/Nairobi",
      "Baghdad"                      => "Asia/Baghdad",
      "Tehran"                       => "Asia/Tehran",
      "Abu Dhabi"                    => "Asia/Muscat",
      "Muscat"                       => "Asia/Muscat",
      "Baku"                         => "Asia/Baku",
      "Tbilisi"                      => "Asia/Tbilisi",
      "Yerevan"                      => "Asia/Yerevan",
      "Kabul"                        => "Asia/Kabul",
      "Ekaterinburg"                 => "Asia/Yekaterinburg",
      "Islamabad"                    => "Asia/Karachi",
      "Karachi"                      => "Asia/Karachi",
      "Tashkent"                     => "Asia/Tashkent",
      "Chennai"                      => "Asia/Kolkata",
      "Kolkata"                      => "Asia/Kolkata",
      "Mumbai"                       => "Asia/Kolkata",
      "New Delhi"                    => "Asia/Kolkata",
      "Kathmandu"                    => "Asia/Kathmandu",
      "Dhaka"                        => "Asia/Dhaka",
      "Sri Jayawardenepura"          => "Asia/Colombo",
      "Almaty"                       => "Asia/Almaty",
      "Astana"                       => "Asia/Almaty",
      "Novosibirsk"                  => "Asia/Novosibirsk",
      "Rangoon"                      => "Asia/Yangon",
      "Bangkok"                      => "Asia/Bangkok",
      "Hanoi"                        => "Asia/Bangkok",
      "Jakarta"                      => "Asia/Jakarta",
      "Krasnoyarsk"                  => "Asia/Krasnoyarsk",
      "Beijing"                      => "Asia/Shanghai",
      "Chongqing"                    => "Asia/Chongqing",
      "Hong Kong"                    => "Asia/Hong_Kong",
      "Urumqi"                       => "Asia/Urumqi",
      "Kuala Lumpur"                 => "Asia/Kuala_Lumpur",
      "Singapore"                    => "Asia/Singapore",
      "Taipei"                       => "Asia/Taipei",
      "Perth"                        => "Australia/Perth",
      "Irkutsk"                      => "Asia/Irkutsk",
      "Ulaanbaatar"                  => "Asia/Ulaanbaatar",
      "Seoul"                        => "Asia/Seoul",
      "Osaka"                        => "Asia/Tokyo",
      "Sapporo"                      => "Asia/Tokyo",
      "Tokyo"                        => "Asia/Tokyo",
      "Yakutsk"                      => "Asia/Yakutsk",
      "Darwin"                       => "Australia/Darwin",
      "Adelaide"                     => "Australia/Adelaide",
      "Canberra"                     => "Australia/Canberra",
      "Melbourne"                    => "Australia/Melbourne",
      "Sydney"                       => "Australia/Sydney",
      "Brisbane"                     => "Australia/Brisbane",
      "Hobart"                       => "Australia/Hobart",
      "Vladivostok"                  => "Asia/Vladivostok",
      "Guam"                         => "Pacific/Guam",
      "Port Moresby"                 => "Pacific/Port_Moresby",
      "Magadan"                      => "Asia/Magadan",
      "Srednekolymsk"                => "Asia/Srednekolymsk",
      "Solomon Is."                  => "Pacific/Guadalcanal",
      "New Caledonia"                => "Pacific/Noumea",
      "Fiji"                         => "Pacific/Fiji",
      "Kamchatka"                    => "Asia/Kamchatka",
      "Marshall Is."                 => "Pacific/Majuro",
      "Auckland"                     => "Pacific/Auckland",
      "Wellington"                   => "Pacific/Auckland",
      "Nuku'alofa"                   => "Pacific/Tongatapu",
      "Tokelau Is."                  => "Pacific/Fakaofo",
      "Chatham Is."                  => "Pacific/Chatham",
      "Samoa"                        => "Pacific/Apia"
    }

    TWO_DIGITS = Array.new(60) { |n| format("%02d", n).freeze }.freeze # :nodoc:
    private_constant :TWO_DIGITS


    @lazy_zones_map = Concurrent::Map.new
    @country_zones  = Concurrent::Map.new

    class << self
      def seconds_to_utc_offset(seconds, colon = true)
        sign = seconds < 0 ? "-" : "+"
        hours = seconds.abs / 3600
        minutes = (seconds.abs % 3600) / 60
        hours = TWO_DIGITS[hours] || format("%02d", hours)
        minutes = TWO_DIGITS[minutes]
        colon ? "#{sign}#{hours}:#{minutes}" : "#{sign}#{hours}#{minutes}"
      end

      def find_tzinfo(name)
        TZInfo::Timezone.get(MAPPING[name] || name)
      end

      alias_method :create, :new # :nodoc:

      def new(name)
        self[name]
      end

      def all
        @zones ||= zones_map.values.sort
      end

      def [](arg)
        case arg
        when self
          arg
        when String
          begin
            @lazy_zones_map[arg] ||= create(arg)
          rescue TZInfo::InvalidTimezoneIdentifier
            nil
          end
        when TZInfo::Timezone
          @lazy_zones_map[arg.name] ||= create(arg.name, nil, arg)
        when Numeric, ActiveSupport::Duration
          arg *= 3600 if arg.abs <= 13
          all.find { |z| z.utc_offset == arg.to_i }
        else
          raise ArgumentError, "invalid argument to TimeZone[]: #{arg.inspect}"
        end
      end

      def us_zones
        country_zones(:us)
      end

      def country_zones(country_code)
        code = country_code.to_s.upcase
        @country_zones[code] ||= load_country_zones(code)
      end

      def clear # :nodoc:
        @lazy_zones_map = Concurrent::Map.new
        @country_zones  = Concurrent::Map.new
        @zones = nil
        @zones_map = nil
      end

      private
        def load_country_zones(code)
          country = TZInfo::Country.get(code)
          country.zone_identifiers.flat_map do |tz_id|
            if MAPPING.value?(tz_id)
              MAPPING.inject([]) do |memo, (key, value)|
                memo << self[key] if value == tz_id
                memo
              end
            else
              create(tz_id, nil, TZInfo::Timezone.get(tz_id))
            end
          end.sort!
        end

        def zones_map
          @zones_map ||= MAPPING.each_with_object({}) do |(name, _), zones|
            timezone = self[name]
            zones[name] = timezone if timezone
          end
        end
    end

    include Comparable
    attr_reader :name
    attr_reader :tzinfo

    def initialize(name, utc_offset = nil, tzinfo = nil)
      @name = name
      @utc_offset = utc_offset
      @tzinfo = tzinfo || TimeZone.find_tzinfo(name)
    end

    def standard_name
      MAPPING[name] || name
    end

    def utc_offset
      @utc_offset || tzinfo&.current_period&.base_utc_offset
    end

    def formatted_offset(colon = true, alternate_utc_string = nil)
      utc_offset == 0 && alternate_utc_string || self.class.seconds_to_utc_offset(utc_offset, colon)
    end

    def <=>(zone)
      return unless zone.respond_to? :utc_offset
      result = (utc_offset <=> zone.utc_offset)
      result = (name <=> zone.name) if result == 0
      result
    end

    def =~(re)
      re === name || re === MAPPING[name]
    end

    def match?(re)
      (re == name) || (re == MAPPING[name]) ||
        ((Regexp === re) && (re.match?(name) || re.match?(MAPPING[name])))
    end

    def to_s
      "(GMT#{formatted_offset}) #{name}"
    end

    def local(*args)
      time = Time.utc(*args)
      ActiveSupport::TimeWithZone.new(nil, self, time)
    end

    def at(*args)
      Time.at(*args).utc.in_time_zone(self)
    end

    def iso8601(str)
      raise ArgumentError, "invalid date" if str.nil?

      parts = Date._iso8601(str)

      year = parts.fetch(:year)

      if parts.key?(:yday)
        ordinal_date = Date.ordinal(year, parts.fetch(:yday))
        month = ordinal_date.month
        day = ordinal_date.day
      else
        month = parts.fetch(:mon)
        day = parts.fetch(:mday)
      end

      time = Time.new(
        year,
        month,
        day,
        parts.fetch(:hour, 0),
        parts.fetch(:min, 0),
        parts.fetch(:sec, 0) + parts.fetch(:sec_fraction, 0),
        parts.fetch(:offset, 0)
      )

      if parts[:offset]
        TimeWithZone.new(time.utc, self)
      else
        TimeWithZone.new(nil, self, time)
      end

    rescue Date::Error, KeyError
      raise ArgumentError, "invalid date"
    end

    def parse(str, now = now())
      parts_to_time(Date._parse(str, false), now)
    end

    def rfc3339(str)
      parts = Date._rfc3339(str)

      raise ArgumentError, "invalid date" if parts.empty?

      time = Time.new(
        parts.fetch(:year),
        parts.fetch(:mon),
        parts.fetch(:mday),
        parts.fetch(:hour),
        parts.fetch(:min),
        parts.fetch(:sec) + parts.fetch(:sec_fraction, 0),
        parts.fetch(:offset)
      )

      TimeWithZone.new(time.utc, self)
    end

    def strptime(str, format, now = now())
      parts_to_time(DateTime._strptime(str, format), now)
    end

    def now
      time_now.utc.in_time_zone(self)
    end

    def today
      now.to_date
    end

    def tomorrow
      today + 1
    end

    def yesterday
      today - 1
    end

    # ActiveSupport.utc_to_local_returns_utc_offset_times is true under
    # `load_defaults` 6.1 and later (Core: 8.1) where the tree does not
    # carry the accessor.
    def utc_to_local(time)
      offset_times = ActiveSupport.respond_to?(:utc_to_local_returns_utc_offset_times) ? ActiveSupport.utc_to_local_returns_utc_offset_times : true
      tzinfo.utc_to_local(time).yield_self do |t|
        offset_times ?
          t : Time.utc(t.year, t.month, t.day, t.hour, t.min, t.sec, t.sec_fraction * 1_000_000)
      end
    end

    def local_to_utc(time, dst = true)
      tzinfo.local_to_utc(time, dst)
    end

    def period_for_utc(time) # :nodoc:
      tzinfo.period_for_utc(time)
    end

    def period_for_local(time, dst = true) # :nodoc:
      tzinfo.period_for_local(time, dst) { |periods| periods.last }
    end

    def periods_for_local(time) # :nodoc:
      tzinfo.periods_for_local(time)
    end

    def abbr(time) # :nodoc:
      tzinfo.abbr(time)
    end

    def dst?(time) # :nodoc:
      tzinfo.dst?(time)
    end

    def init_with(coder) # :nodoc:
      initialize(coder["name"])
    end

    def encode_with(coder) # :nodoc:
      coder.tag = "!ruby/object:#{self.class}"
      coder.map = { "name" => tzinfo.name }
    end

    private
      def parts_to_time(parts, now)
        raise ArgumentError, "invalid date" if parts.nil?
        if parts.empty?
          raise ArgumentError, "invalid date" if ActiveSupport.respond_to?(:raise_on_invalid_time_zone_parse) && ActiveSupport.raise_on_invalid_time_zone_parse
          return
        end

        if parts[:seconds]
          time = Time.at(parts[:seconds] + parts.fetch(:sec_fraction, 0))
        else
          time = Time.new(
            parts.fetch(:year, now.year),
            parts.fetch(:mon, now.month),
            parts.fetch(:mday, parts[:year] || parts[:mon] ? 1 : now.day),
            parts.fetch(:hour, 0),
            parts.fetch(:min, 0),
            parts.fetch(:sec, 0) + parts.fetch(:sec_fraction, 0),
            parts.fetch(:offset, 0)
          )
        end

        if parts[:offset] || parts[:seconds]
          TimeWithZone.new(time.utc, self)
        else
          TimeWithZone.new(nil, self, time)
        end
      end

      def time_now
        ActiveSupport.respond_to?(:clock) ? ActiveSupport.clock : Time.now
      end
  end
end

require_relative "active_support_time_with_zone"
