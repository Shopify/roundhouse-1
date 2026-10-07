# `useragent` 0.16.11's UserAgent::Version (lib/user_agent/version.rb),
# verbatim below this header, for the CRuby PostgreSQL tree.
#
# The shared port (runtime/ruby/user_agent.rb) is typed for every target
# and answers versions as Strings; it has no Version class. Shopify Core's
# secure_headers gem source builds three at class-body load
# (headers/content_security_policy.rb: `VERSION_46 = ::UserAgent::Version.new("46")`,
# VERSION_10, FALLBACK_VERSION) and compares a browser's version against
# them with `>=`. Its include of ::Comparable, `self.new` passthrough,
# segment-wise `<=>` (Integer runs, a trailing alphabetic tag that sorts
# below any number, missing segments as 0, the six-segment cap) and the
# String/nil coercions are the gem's own, so this file is the gem's code
# rather than a typed transcription. tests/user_agent_version.rs runs the
# same comparisons against the installed gem and requires them equal.
#
# The gem is Rails-free, but loading it whole would redefine the port's
# UserAgent (tests/user_agent_feature_port.rs), so only this class is
# carried. `class UserAgent` here reopens the port's class.
#
# Not ported with it (a gap, not a fallback):
# - `UserAgent::Browsers::Base` and the other browser classes. secure_headers'
#   ContentSecurityPolicy#initialize tests `user_agent.is_a?(UserAgent::Browsers::Base)`,
#   which raises NameError here. Core's default configuration opts CSP out,
#   so boot does not build one.
# - The port's `UserAgent.parse(...).version` is a String, where the gem
#   answers a Version. String#<=> compares a Version through its to_str,
#   lexicographically, so `"126.0" >= VERSION_46` is false and
#   `"9.1" >= VERSION_10` true, unlike the gem (tests/user_agent_version.rb
#   records both). secure_headers' per-browser CSP directive choice
#   (supported_directives, nonces_supported?) reaches that comparison.

class UserAgent
  class Version
    include ::Comparable

    def self.new(obj = nil)
      case obj
      when Version
        obj
      when String
        super
      when NilClass
        super("")
      else
        raise ArgumentError, "invalid value for Version: #{obj.inspect}"
      end
    end

    def initialize(str)
      @str = str

      if @str =~ /^\s*$/
        @nil        = true
        @sequences  = []
        @comparable = false
      elsif str =~ /^\d+$/ || str =~ /^\d+\./
        @nil        = false
        @sequences  = str.scan(/\d+|[A-Za-z][0-9A-Za-z-]*$/).map { |s| s =~ /^\d+$/ ? s.to_i : s }
        @comparable = true
      else
        @nil        = false
        @sequences  = [str]
        @comparable = false
      end
    end

    def nil?
      @nil
    end

    def to_a
      @sequences.dup
    end

    def to_str
      @str.dup
    end

    def eql?(other)
      other.is_a?(self.class) && to_s == other.to_s
    end

    def ==(other)
      case other
      when Version
        eql?(other)
      when String
        eql?(self.class.new(other))
      when NilClass
        nil?
      else
        false
      end
    end

    def <=>(other)
      case other
      when Version
        if @comparable
          ([0]*6).zip(to_a, other.to_a).each do |dump, a, b|
            a ||= 0
            b ||= 0

            if a.is_a?(String) && b.is_a?(Integer)
              return -1
            elsif a.is_a?(Integer) && b.is_a?(String)
              return 1
            elsif a == b
              next
            else
              return a <=> b
            end
          end
          0
        elsif to_s == other.to_s
          return 0
        else
          return -1
        end
      when String, NilClass
        self <=> self.class.new(other)
      else
        nil
      end
    end

    def to_s
      to_str
    end

    def inspect
      "#<#{self.class} #{to_s}>"
    end
  end
end
