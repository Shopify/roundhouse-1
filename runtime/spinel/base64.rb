# Minimal Base64 shim — provides only the surface called from
# framework Ruby. The Ruby target's `require_relative "runtime/base64"`
# resolves here uniformly; under CRuby this module is also reachable
# via the stdlib's `require "base64"` but the framework Ruby's
# `Base64.strict_encode64` call site doesn't depend on which path
# loaded the constant — both implementations produce the same RFC
# 4648 strict (no-line-wrap) encoding.
#
# Grown on demand, as the header always said it would be:
# `strict_encode64` for turbo-stream signed names, `strict_decode64` for
# the Action Cable glue, and the `urlsafe_*` pair for campfire's QR-code
# helper (`Base64.urlsafe_encode64(url)` / the controller's decode).
# Since then the whole stdlib surface, ported with its semantics
# (Core passes `padding: false` and relies on strict decode raising):
# `encode64`, lax `decode64`, strict `strict_decode64`.
module Base64
  ALPHABET = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"

  # RFC 4648 §4 base64 encoding with no line wrapping. Iterates the
  # input bytes in 3-byte chunks producing 4 output characters per
  # chunk; trailing 1- or 2-byte remainders pad with "=" / "==".
  def self.strict_encode64(s)
    bytes = s.bytes
    n = bytes.length
    out = +""
    i = 0
    while i + 3 <= n
      b0 = bytes[i]
      b1 = bytes[i + 1]
      b2 = bytes[i + 2]
      out << ALPHABET[(b0 >> 2) & 0x3F].to_s
      out << ALPHABET[((b0 << 4) | (b1 >> 4)) & 0x3F].to_s
      out << ALPHABET[((b1 << 2) | (b2 >> 6)) & 0x3F].to_s
      out << ALPHABET[b2 & 0x3F].to_s
      i = i + 3
    end
    rem = n - i
    if rem == 1
      b0 = bytes[i]
      out << ALPHABET[(b0 >> 2) & 0x3F].to_s
      out << ALPHABET[(b0 << 4) & 0x3F].to_s
      out << "=="
    elsif rem == 2
      b0 = bytes[i]
      b1 = bytes[i + 1]
      out << ALPHABET[(b0 >> 2) & 0x3F].to_s
      out << ALPHABET[((b0 << 4) | (b1 >> 4)) & 0x3F].to_s
      out << ALPHABET[(b1 << 2) & 0x3F].to_s
      out << "="
    end
    out
  end

  # RFC 4648 §4 decode — inverse of strict_encode64, with Ruby's
  # `unpack1("m0")` strictness: the length must be a multiple of four,
  # every character must be in the alphabet, "=" may only close the
  # last quantum, and the bits padding discards must be zero. Anything
  # else raises `ArgumentError, "invalid base64"`, as the stdlib does —
  # JWT and cookie verifiers rely on that to reject tampered input.
  def self.strict_decode64(s)
    n = s.bytesize
    if n % 4 != 0
      raise ArgumentError, "invalid base64"
    end
    pad = 0
    if n > 0 && s.getbyte(n - 1) == 61   # "="
      pad = 1
      if s.getbyte(n - 2) == 61
        pad = 2
      end
    end
    # By byte, so the check allocates nothing per character.
    i = 0
    while i < n - pad
      if byte_value(s.getbyte(i)) < 0
        raise ArgumentError, "invalid base64"
      end
      i = i + 1
    end
    if pad == 2 && (byte_value(s.getbyte(n - 3)) & 0x0F) != 0
      raise ArgumentError, "invalid base64"
    end
    if pad == 1 && (byte_value(s.getbyte(n - 2)) & 0x03) != 0
      raise ArgumentError, "invalid base64"
    end
    decode64(s)
  end

  # Ruby's lax `decode64` (`unpack1("m")`): skips any byte outside the
  # alphabet (newlines included) and stops at the first "=". Accumulates
  # 6 bits per input char into a bit buffer, emitting one byte per 8
  # accumulated bits. lobsters' FetchEmailBlocklistJob decodes a GitHub
  # contents-API payload, which wraps at 60 columns.
  def self.decode64(s)
    out = +""
    acc = 0
    nbits = 0
    i = 0
    n = s.bytesize
    while i < n
      b = s.getbyte(i)
      i = i + 1
      if b == 61   # "="
        break
      end
      val = byte_value(b)
      if val < 0
        next   # stray byte
      end
      # Masked: only the low `nbits + 6` bits (at most 13) are ever
      # read, and an unmasked accumulator grows without bound — a
      # Bignum in Ruby, and on a fixed-width Integer a silent wrap that
      # spinel now raises on (`integer overflow in <<`, e3883499).
      acc = ((acc << 6) | val) & 0xFFFF
      nbits = nbits + 6
      if nbits >= 8
        nbits = nbits - 8
        byte = (acc >> nbits) & 0xFF
        out << byte.chr
      end
    end
    out
  end

  # Ruby's `encode64` (`pack("m")`): the strict encoding wrapped at 60
  # characters (45 input bytes) per line, every line — the last one
  # included — ending in "\n". The empty string encodes to "".
  def self.encode64(s)
    out = +""
    i = 0
    n = s.bytesize
    while i < n
      out << strict_encode64(s.byteslice(i, 45).to_s)
      out << "\n"
      i = i + 45
    end
    out
  end

  # The 6-bit value of one base64 alphabet byte, or -1 for anything
  # outside the alphabet (padding / separators). Arithmetic on the byte,
  # not a scan of ALPHABET: the scan made decoding 64 comparisons per
  # character, and since the session cookie is signed every request
  # decodes it — the lobsters ruby lane spent a third of its time here.
  def self.byte_value(b)
    return b - 65 if b >= 65 && b <= 90   # A-Z
    return b - 71 if b >= 97 && b <= 122  # a-z
    return b + 4 if b >= 48 && b <= 57    # 0-9
    return 62 if b == 43                  # +
    return 63 if b == 47                  # /
    -1
  end

  # RFC 4648 §5 — the URL-safe alphabet is the standard one with `+` and
  # `/` swapped for `-` and `_`. Implemented as a translation over the
  # standard encoder's output rather than by parameterizing it: the
  # encoder is the verified path for turbo-stream signed names and is
  # left untouched. `String#tr` would say this in one line, but the
  # explicit loop uses only the shapes the rest of this file already
  # does, which is what keeps it compiling on every ruby-family target.
  #
  # The stdlib signature, `urlsafe_encode64(bin, padding: true)`:
  # campfire calls it with no keyword, GlobalID#to_param and Core's JWT
  # code with `padding: false`. Trailing "=" are what padding means in
  # base64, and nothing else in the alphabet produces one, so dropping
  # them is the whole difference.
  def self.urlsafe_encode64(s, padding: true)
    std = strict_encode64(s)
    n = std.length
    unless padding
      while n > 0 && std[n - 1] == "="
        n = n - 1
      end
    end
    out = +""
    i = 0
    while i < n
      c = std[i]
      if c == "+"
        out << "-"
      elsif c == "/"
        out << "_"
      else
        out << c.to_s
      end
      i = i + 1
    end
    out
  end

  # Kept for the runtime callers written before `urlsafe_encode64`
  # took the keyword (web_push_crypto, rails.rb's GlobalID).
  def self.urlsafe_encode64_nopad(s)
    urlsafe_encode64(s, padding: false)
  end

  # The inverse, as the stdlib does it: map the URL-safe pair back,
  # restore the padding an unpadded input left off (RFC 4648 lets an
  # encoder omit it, and `padding: false` does), then decode strictly.
  def self.urlsafe_decode64(s)
    std = +""
    i = 0
    n = s.length
    while i < n
      c = s[i]
      if c == "-"
        std << "+"
      elsif c == "_"
        std << "/"
      else
        std << c.to_s
      end
      i = i + 1
    end
    if !(n > 0 && s[n - 1] == "=")
      while std.length % 4 != 0
        std << "="
      end
    end
    strict_decode64(std)
  end
end
