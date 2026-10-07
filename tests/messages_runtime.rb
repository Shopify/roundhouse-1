# Native ActiveSupport 8.1.4 oracle for the signed and encrypted messages
# port (runtime/spinel/active_support_messages.rb, with the ActiveSupport::JSON
# coder of runtime/spinel/active_support_json.rb).
#
# Modes:
# - `native` / `emitted` print one line per observation. Deterministic
#   APIs print their output (a verifier's message for a fixed secret, a
#   derived key); random ones (an encryptor's IV) print what can be
#   compared: round trips, lengths, the failure each tampering raises.
#   Each mode also writes the messages it made to ARGV[1] and, given
#   ARGV[2], reads the other side's messages and prints what it gets
#   out of them.
# - `compare` runs native and emitted twice (make, then read the other
#   side's messages) and requires every observation equal.
#
# The surface is what Core calls: Shopify::Secrets' build_rotation over
# MessageVerifier / MessageEncryptor / SecureCompareRotator with
# rotations and on_rotation, generate / verify / verified /
# valid_message?, encrypt_and_sign / decrypt_and_verify with purpose
# and expiry, SecurityUtils, KeyGenerator, MessageEncryptor.key_len, a
# MessageVerifier subclass (DevelopmentSupport::ReportifyTokenGenerator),
# and the serializers it passes (:marshal default, JSON, ActiveSupport::JSON,
# a custom module). Secrets are synthetic.
require "json"
require "tmpdir"
require "fileutils"

if ARGV.first == "compare"
  require "open3"
  run = lambda do |mode, out, other|
    stdout, stderr, status = Open3.capture3(RbConfig.ruby, __FILE__, mode, out, *other)
    abort "#{mode}: #{stderr}" unless status.success?
    stdout
  end
  Dir.mktmpdir do |dir|
    native_made = File.join(dir, "native.json")
    emitted_made = File.join(dir, "emitted.json")
    run.call("native", native_made, [])
    run.call("emitted", emitted_made, [])
    outputs = [
      run.call("native", File.join(dir, "native2.json"), [emitted_made]),
      run.call("emitted", File.join(dir, "emitted2.json"), [native_made]),
    ]
    unless outputs.uniq.length == 1
      a, b = outputs.map(&:lines)
      diff = a.zip(b).each_with_index.reject { |(x, y), _| x == y }.first(5)
      abort "messages differ:\n" + diff.map { |(x, y), i| "line #{i + 1}\n  native:  #{x}  emitted: #{y}" }.join("\n")
    end
    puts outputs.first
    puts "Native ActiveSupport and the emitted ActiveSupport messages agree"
  end
  exit
end

MODE, MADE_PATH, OTHER_PATH = ARGV

if MODE == "native"
  gem "activesupport", "8.1.4"
  require "active_support"
  require "active_support/notifications"
  require "active_support/json"
  require "active_support/core_ext/object/json"
  # Rails' own order (railties application.rb requires key_generator
  # first): openssl is loaded before security_utils picks its branch.
  require "active_support/key_generator"
  require "active_support/security_utils"
  require "active_support/message_verifier"
  require "active_support/message_encryptor"
  require "active_support/secure_compare_rotator"
else
  # The tree's load order: rails.rb, then active_support_ext.rb, whose
  # MessageVerifier stand-in project.rs replaces with a require of the
  # port (`MESSAGES_STAND_IN`).
  root = File.expand_path("..", __dir__)
  require File.join(root, "runtime/ruby/rails")
  stand_in = <<~RUBY.gsub(/^/, "  ")
    class MessageVerifier
      class InvalidSignature < StandardError
        def initialize(message = "ActiveSupport::MessageVerifier::InvalidSignature")
          super(message)
        end
      end
    end
  RUBY
  ext = File.read(File.join(root, "runtime/ruby/active_support_ext.rb"))
  abort "active_support_ext.rb: expected one MessageVerifier stand-in" unless ext.scan(stand_in).length == 1
  tree = Dir.mktmpdir
  at_exit { FileUtils.remove_entry(tree) }
  File.write(File.join(tree, "active_support_ext.rb"), ext.sub(stand_in, "  require_relative \"active_support_messages\"\n"))
  %w[active_support_messages active_support_json active_support_notifications].each do |name|
    FileUtils.cp(File.join(root, "runtime/spinel/#{name}.rb"), tree)
  end
  require File.join(tree, "active_support_ext")
end

def show(label, value)
  puts "#{label}: #{value.inspect}"
end

def outcome
  yield.inspect
rescue Exception => e
  "#{e.class}: #{e.message.lines.first.to_s.strip}"
end

FALLBACKS = []
ActiveSupport::Notifications.subscribe("message_serializer_fallback.active_support") do |name, _start, _finish, _id, payload|
  FALLBACKS << [name, payload[:serializer], payload[:fallback], payload[:deserialized]]
end

def fallbacks
  out = FALLBACKS.dup
  FALLBACKS.clear
  out
end

made = {}
SECRET = "s" * 64
OLD_SECRET = "o" * 64
KEY = "k" * 32
OLD_KEY = "q" * 32
FUTURE = Time.utc(2100, 1, 2, 3, 4, 5)
PAST = Time.utc(2001, 1, 2, 3, 4, 5)

# --- SecurityUtils --------------------------------------------------------
su = ActiveSupport::SecurityUtils
show("secure_compare", [su.secure_compare("abc", "abc"), su.secure_compare("abc", "abd"), su.secure_compare("abc", "ab"), su.secure_compare("", "")])
show("fixed_length_secure_compare", [su.fixed_length_secure_compare("abc", "abc"), su.fixed_length_secure_compare("abc", "abd"), outcome { su.fixed_length_secure_compare("abc", "ab") }])
show("security utils methods", [su.respond_to?(:secure_compare), su.private_instance_methods.sort.include?(:secure_compare)])

# --- KeyGenerator ---------------------------------------------------------
kg = ActiveSupport::KeyGenerator.new("secret base", iterations: 1000)
show("generate_key", kg.generate_key("salt").unpack1("H*"))
show("generate_key 32", kg.generate_key("salt", 32).unpack1("H*"))
show("generate_key sha256", ActiveSupport::KeyGenerator.new("secret base", iterations: 1000, hash_digest_class: OpenSSL::Digest::SHA256).generate_key("salt", 32).unpack1("H*"))
show("hash_digest_class", ActiveSupport::KeyGenerator.hash_digest_class.name)
show("hash_digest_class=", [outcome { ActiveSupport::KeyGenerator.hash_digest_class = String }, outcome { ActiveSupport::KeyGenerator.hash_digest_class = OpenSSL::Digest::SHA1 }])
caching = ActiveSupport::CachingKeyGenerator.new(kg)
first = caching.generate_key("salt", 32)
show("caching", [first == kg.generate_key("salt", 32), caching.generate_key("salt", 32).equal?(first), caching.generate_key("salt").bytesize])
show("key inspect", kg.inspect.sub(/0x\h+/, "0x"))
show("key_len", [ActiveSupport::MessageEncryptor.key_len, ActiveSupport::MessageEncryptor.key_len("aes-256-gcm"), ActiveSupport::MessageEncryptor.key_len("aes-128-gcm")])

# --- MessageVerifier --------------------------------------------------------
module JsonSerializer
  def self.dump(object) = JSON.generate(object)
  def self.load(payload) = JSON.parse(payload)
end

verifier = ActiveSupport::MessageVerifier.new(SECRET)
token = verifier.generate("data")
show("generate marshal", token)
show("verify", [verifier.verify(token), verifier.verified(token), verifier.valid_message?(token)])
show("generate values", [verifier.generate(1), verifier.generate([1, "a", :b, nil]), verifier.generate({ "a" => { b: 1 } })])
show("round trip values", [verifier.verify(verifier.generate([1, "a", :b, nil])), verifier.verify(verifier.generate({ "a" => { b: 1 } })), verifier.verify(verifier.generate(FUTURE))])
tampered = token.sub(/.\z/) { |c| c == "0" ? "1" : "0" }
show("tampered", [verifier.verified(tampered), verifier.valid_message?(tampered), outcome { verifier.verify(tampered) }])
show("nil and blank", [verifier.verified(nil), verifier.valid_message?(nil), outcome { verifier.verify(nil) }, outcome { verifier.verify("") }, outcome { verifier.verify("--") }])
show("invalid encoding", [outcome { verifier.verify("\xff--abc".dup.force_encoding("UTF-8")) }, verifier.verified("\xff".dup.force_encoding("UTF-8"))])
show("no separator", outcome { verifier.verify("abc" * 20) })
show("wrong secret", [ActiveSupport::MessageVerifier.new(OLD_SECRET).verified(token), outcome { ActiveSupport::MessageVerifier.new(OLD_SECRET).verify(token) }])
show("nil secret", outcome { ActiveSupport::MessageVerifier.new(nil) })
bad_base64 = "!!!!--" + OpenSSL::HMAC.hexdigest("SHA1", SECRET, "!!!!")
show("bad base64", [outcome { verifier.verify(bad_base64) }, verifier.verified(bad_base64)])
show("sha256", ActiveSupport::MessageVerifier.new(SECRET, digest: "SHA256").generate("data"))
show("sha256 symbol digest", ActiveSupport::MessageVerifier.new(SECRET, digest: :SHA512).generate("data").length)
url = ActiveSupport::MessageVerifier.new(SECRET, url_safe: true)
show("url_safe", [url.generate("?>?>~~~"), url.verify(url.generate("?>?>~~~")), url.verify(verifier.generate("?>?>~~~")), verifier.verify(url.generate("?>?>~~~"))])

# Metadata: the legacy envelope (use_message_serializer_for_metadata unset).
show("use_message_serializer_for_metadata", ActiveSupport::Messages::Metadata.use_message_serializer_for_metadata)
purposed = verifier.generate("data", purpose: :login)
show("purpose", [purposed, verifier.verify(purposed, purpose: :login), verifier.verify(purposed, purpose: "login"), verifier.verified(purposed), verifier.verified(purposed, purpose: :other)])
show("purpose raises", [outcome { verifier.verify(purposed) }, outcome { verifier.verify(purposed, purpose: :other) }, outcome { verifier.verify(token, purpose: :login) }])
expiring = verifier.generate("data", expires_at: FUTURE, purpose: "p")
show("expires_at future", [expiring, verifier.verify(expiring, purpose: "p")])
expired = verifier.generate("data", expires_at: PAST)
show("expires_at past", [expired, verifier.verified(expired), outcome { verifier.verify(expired) }])
show("expires_in", [verifier.verify(verifier.generate("data", expires_in: 3600)), verifier.verified(verifier.generate("data", expires_in: -1))])
show("no metadata values", verifier.generate("data", purpose: nil, expires_at: nil) == token)

ActiveSupport::Messages::Metadata.use_message_serializer_for_metadata = true
show("message serializer metadata", [verifier.generate("data", purpose: :login), verifier.generate("data", expires_at: FUTURE)])
show("message serializer metadata reads", [verifier.verify(verifier.generate("data", purpose: :login), purpose: :login), verifier.verify(purposed, purpose: :login), verifier.verified(verifier.generate("data", expires_at: PAST))])
show("force legacy", ActiveSupport::MessageVerifier.new(SECRET, force_legacy_metadata_serializer: true).generate("data", purpose: :login) == purposed)
custom_meta = ActiveSupport::MessageVerifier.new(SECRET, serializer: JsonSerializer)
show("custom serializer keeps legacy envelope", custom_meta.generate({ "a" => 1 }, purpose: :x))
ActiveSupport::Messages::Metadata.use_message_serializer_for_metadata = nil

# Serializers.
json = ActiveSupport::MessageVerifier.new(SECRET, serializer: JSON)
as_json = ActiveSupport::MessageVerifier.new(SECRET, serializer: ActiveSupport::JSON)
custom = ActiveSupport::MessageVerifier.new(SECRET, serializer: JsonSerializer)
sym_json = ActiveSupport::MessageVerifier.new(SECRET, serializer: :json)
allow_marshal = ActiveSupport::MessageVerifier.new(SECRET, serializer: :json_allow_marshal)
marshal = ActiveSupport::MessageVerifier.new(SECRET, serializer: :marshal)
value = { "id" => 1, "name" => "<a&b>", "list" => [1, 2.5, nil, true], "nested" => { "sym" => :s } }
show("serializer JSON", [json.generate(value), json.verify(json.generate(value))])
show("serializer ActiveSupport::JSON", [as_json.generate(value), as_json.verify(as_json.generate(value)), as_json.generate("x\u2028y"), as_json.verify(as_json.generate(FUTURE))])
show("serializer custom", [custom.generate(value), custom.verify(custom.generate(value), purpose: nil)])
show("serializer :json", [sym_json.generate(value), sym_json.verify(sym_json.generate(value)), sym_json.verify(sym_json.generate("str"))])
show("serializer :json_allow_marshal", [allow_marshal.generate(value), allow_marshal.verify(allow_marshal.generate(value))])
show("fallback json reads marshal", [outcome { sym_json.verify(marshal.generate("m")) }, outcome { sym_json.verified(marshal.generate("m")) }, fallbacks])
show("fallback allow marshal reads marshal", [allow_marshal.verify(marshal.generate("m")), fallbacks])
show("fallback marshal reads json", [marshal.verify(sym_json.generate({ "j" => 1 })), fallbacks])
show("unparseable payload", [outcome { json.verify(verifier.generate("data")) }, outcome { as_json.verified(verifier.generate("data")) }])
show("default_serializer", [ActiveSupport::Messages::Codec.default_serializer, ActiveSupport::MessageVerifier.default_serializer, ActiveSupport::MessageEncryptor.default_serializer])
ActiveSupport::MessageVerifier.default_serializer = :json
show("subclass default_serializer", [ActiveSupport::Messages::Codec.default_serializer, ActiveSupport::MessageVerifier.default_serializer, ActiveSupport::MessageEncryptor.default_serializer, ActiveSupport::MessageVerifier.new(SECRET).generate("x")])
ActiveSupport::MessageVerifier.default_serializer = :marshal
# :message_pack is not compared: it needs the msgpack gem, which the
# tree does not carry (the port raises LoadError, as natively without it).
show("unknown serializer", outcome { ActiveSupport::MessageVerifier.new(SECRET, serializer: :yaml) })

# Rotation, the way Shopify::Secrets.build_rotation builds it.
ROTATIONS = []
def build_rotation(klass, secrets, options = {})
  if options[:on_rotation] == false
    options.delete(:on_rotation)
  else
    options[:on_rotation] ||= -> { ROTATIONS << klass.name }
  end
  secrets = Array(secrets).dup
  klass.new(secrets.shift, **options).tap do |crypt|
    secrets.each { |secret| crypt.rotate(secret) }
  end
end

old = ActiveSupport::MessageVerifier.new(OLD_SECRET, serializer: JsonSerializer)
rotating = build_rotation(ActiveSupport::MessageVerifier, [SECRET, OLD_SECRET], serializer: JsonSerializer)
show("rotation new", [rotating.verify(rotating.generate({ "a" => 1 })), ROTATIONS.dup])
show("rotation old", [rotating.verify(old.generate({ "a" => 1 })), rotating.verified(old.generate("b")), ROTATIONS.dup])
ROTATIONS.clear
show("rotation neither", [rotating.verified(ActiveSupport::MessageVerifier.new("x" * 64).generate("c")), outcome { rotating.verify(ActiveSupport::MessageVerifier.new("x" * 64).generate("c")) }, ROTATIONS.dup])
show("rotation purpose", [rotating.verify(old.generate("p", purpose: :a), purpose: :a), rotating.verified(old.generate("p", purpose: :a), purpose: :b), ROTATIONS.dup])
ROTATIONS.clear
quiet = build_rotation(ActiveSupport::MessageVerifier, [SECRET, OLD_SECRET], on_rotation: false)
show("rotation without callback", [quiet.verify(ActiveSupport::MessageVerifier.new(OLD_SECRET).generate("z")), ROTATIONS.dup])
called = []
explicit = ActiveSupport::MessageVerifier.new(SECRET).on_rotation { called << :block }
explicit.rotate(OLD_SECRET, digest: "SHA1")
show("on_rotation block", [explicit.verify(ActiveSupport::MessageVerifier.new(OLD_SECRET).generate("z")), called, explicit.verify(ActiveSupport::MessageVerifier.new(OLD_SECRET).generate("z"), on_rotation: -> { called << :override }), called])
digest_rotation = ActiveSupport::MessageVerifier.new(SECRET, digest: "SHA256")
digest_rotation.rotate(digest: "SHA1")
show("rotate options only", [digest_rotation.verify(verifier.generate("d")), digest_rotation.generate("d") == ActiveSupport::MessageVerifier.new(SECRET, digest: "SHA256").generate("d")])
duped = rotating.dup
duped.rotate("y" * 64)
show("dup keeps rotations apart", [duped.verified(ActiveSupport::MessageVerifier.new("y" * 64, serializer: JsonSerializer).generate("w")), rotating.verified(ActiveSupport::MessageVerifier.new("y" * 64, serializer: JsonSerializer).generate("w"))])
show("fall_back_to", ActiveSupport::MessageVerifier.new(SECRET).fall_back_to(old).verified(old.generate({ "f" => 1 })))
show("verifier inspect", verifier.inspect.sub(/0x\h+/, "0x"))
show("verifier ancestors", ActiveSupport::MessageVerifier.ancestors.take_while { |m| m != Object }.map(&:name))
show("verifier constants", [ActiveSupport::MessageVerifier::SEPARATOR, ActiveSupport::MessageVerifier::InvalidSignature.superclass.name, ActiveSupport::MessageVerifier::InvalidSignature.new.message])

# A subclass that does not call super (DevelopmentSupport::ReportifyTokenGenerator).
class TokenGenerator < ActiveSupport::MessageVerifier
  def initialize(verifier)
    @verifier = verifier
  end

  def generate(token)
    token[0] = "42"
    @verifier.generate(token)
  end
end
show("subclass", [TokenGenerator.new(as_json).generate(["x", "y"]), as_json.verify(TokenGenerator.new(as_json).generate(["x", "y"]))])

made["verifier"] = { "plain" => token, "purpose" => purposed, "json" => as_json.generate(value), "rotated" => old.generate({ "r" => 1 }) }

# --- MessageEncryptor --------------------------------------------------------
show("use_authenticated_message_encryption", [ActiveSupport::MessageEncryptor.use_authenticated_message_encryption, ActiveSupport::MessageEncryptor.default_cipher])
cbc = ActiveSupport::MessageEncryptor.new(KEY)
message = cbc.encrypt_and_sign("data")
show("cbc", [cbc.decrypt_and_verify(message), message.count("-"), message.split("--").map(&:length)[1..], message.length == cbc.encrypt_and_sign("data").length, message == cbc.encrypt_and_sign("data")])
show("cbc values", [cbc.decrypt_and_verify(cbc.encrypt_and_sign([1, :a, { "b" => nil }])), cbc.decrypt_and_verify(cbc.encrypt_and_sign(FUTURE))])
show("cbc tampered", [outcome { cbc.decrypt_and_verify(message.sub(/.\z/) { |c| c == "0" ? "1" : "0" }) }, outcome { cbc.decrypt_and_verify(message.reverse) }, outcome { cbc.decrypt_and_verify(nil) }, outcome { cbc.decrypt_and_verify("") }])
show("cbc wrong key", outcome { ActiveSupport::MessageEncryptor.new(OLD_KEY).decrypt_and_verify(message) })
inner = ActiveSupport::MessageVerifier.new(KEY, serializer: ActiveSupport::MessageEncryptor::NullSerializer)
forged = inner.generate("not-base64--parts")
show("cbc bad parts", [outcome { cbc.decrypt_and_verify(forged) }, outcome { cbc.decrypt_and_verify(inner.generate("YWJj--YWJjZGVmZ2hpamtsbW5vcA==")) }])
wrong_key_inner = ActiveSupport::MessageEncryptor.new(OLD_KEY, KEY)
show("cbc signed but wrong cipher key", outcome { ActiveSupport::MessageEncryptor.new(KEY, KEY).decrypt_and_verify(wrong_key_inner.encrypt_and_sign("x")) })
sign_secret = ActiveSupport::MessageEncryptor.new(KEY, "sign" * 16)
show("sign secret", [sign_secret.decrypt_and_verify(sign_secret.encrypt_and_sign("x")), outcome { cbc.decrypt_and_verify(sign_secret.encrypt_and_sign("x")) }])
gcm = ActiveSupport::MessageEncryptor.new(KEY, cipher: "aes-256-gcm")
gcm_message = gcm.encrypt_and_sign("data")
show("gcm", [gcm.decrypt_and_verify(gcm_message), gcm_message.split("--").map(&:length)[1..], gcm_message == gcm.encrypt_and_sign("data")])
show("gcm tampered", [outcome { gcm.decrypt_and_verify(gcm_message.sub(/\A./) { |c| c == "A" ? "B" : "A" }) }, outcome { gcm.decrypt_and_verify(gcm_message.sub(/--[^-]+\z/, "--AAAA")) }, outcome { gcm.decrypt_and_verify(gcm_message.sub(/--[^-]+\z/, "")) }])
show("gcm wrong key", outcome { ActiveSupport::MessageEncryptor.new(OLD_KEY, cipher: "aes-256-gcm").decrypt_and_verify(gcm_message) })
show("cipher mismatch", [outcome { cbc.decrypt_and_verify(gcm_message) }, outcome { gcm.decrypt_and_verify(message) }])
show("bad cipher", outcome { ActiveSupport::MessageEncryptor.new(KEY, cipher: "rot13") })
show("short key", outcome { ActiveSupport::MessageEncryptor.new("short").encrypt_and_sign("x") })
url_enc = ActiveSupport::MessageEncryptor.new(KEY, url_safe: true)
url_message = url_enc.encrypt_and_sign("data?>~")
show("url_safe encryptor", [url_enc.decrypt_and_verify(url_message), url_message.match?(/\A[A-Za-z0-9_\-]+\z/), outcome { cbc.decrypt_and_verify(url_message) }])
url_gcm = ActiveSupport::MessageEncryptor.new(KEY, cipher: "aes-256-gcm", url_safe: true)
show("url_safe gcm", url_gcm.decrypt_and_verify(url_gcm.encrypt_and_sign([1, 2])))
json_enc = ActiveSupport::MessageEncryptor.new(KEY, serializer: JSON)
show("encryptor JSON", [json_enc.decrypt_and_verify(json_enc.encrypt_and_sign(value)), outcome { json_enc.decrypt_and_verify(cbc.encrypt_and_sign("m")) }])
show("encryptor purpose", [cbc.decrypt_and_verify(cbc.encrypt_and_sign("p", purpose: :login), purpose: :login), cbc.decrypt_and_verify(cbc.encrypt_and_sign("p", purpose: :login)), cbc.decrypt_and_verify(cbc.encrypt_and_sign("p", purpose: :login), purpose: :other)])
show("encryptor expiry", [cbc.decrypt_and_verify(cbc.encrypt_and_sign("e", expires_at: PAST)), cbc.decrypt_and_verify(cbc.encrypt_and_sign("e", expires_in: 60)), gcm.decrypt_and_verify(gcm.encrypt_and_sign("e", expires_at: FUTURE))])
ROTATIONS.clear
old_enc = ActiveSupport::MessageEncryptor.new(OLD_KEY, serializer: JSON)
rot_enc = build_rotation(ActiveSupport::MessageEncryptor, [KEY, OLD_KEY], serializer: JSON)
show("encryptor rotation", [rot_enc.decrypt_and_verify(rot_enc.encrypt_and_sign({ "a" => 1 })), ROTATIONS.dup, rot_enc.decrypt_and_verify(old_enc.encrypt_and_sign({ "a" => 2 })), ROTATIONS.dup, outcome { rot_enc.decrypt_and_verify(ActiveSupport::MessageEncryptor.new("z" * 32).encrypt_and_sign("x")) }])
ROTATIONS.clear
gcm_rot = build_rotation(ActiveSupport::MessageEncryptor, [KEY, OLD_KEY], cipher: "aes-256-gcm", serializer: JSON)
show("gcm rotation", [gcm_rot.decrypt_and_verify(ActiveSupport::MessageEncryptor.new(OLD_KEY, cipher: "aes-256-gcm", serializer: JSON).encrypt_and_sign([3])), ROTATIONS.dup])
cbc_rot = ActiveSupport::MessageEncryptor.new(KEY, cipher: "aes-256-gcm")
cbc_rot.rotate(KEY, cipher: "aes-256-cbc")
show("rotate cipher", cbc_rot.decrypt_and_verify(cbc.encrypt_and_sign("c")))
derived = ActiveSupport::KeyGenerator.new("base", iterations: 1000).generate_key("salt", ActiveSupport::MessageEncryptor.key_len)
derived_enc = ActiveSupport::MessageEncryptor.new(derived)
show("derived key", derived_enc.decrypt_and_verify(derived_enc.encrypt_and_sign("k")))
show("encryptor inspect", cbc.inspect.sub(/0x\h+/, "0x"))
show("encryptor ancestors", ActiveSupport::MessageEncryptor.ancestors.take_while { |m| m != Object }.map(&:name))
show("encryptor constants", [ActiveSupport::MessageEncryptor::OpenSSLCipherError.equal?(OpenSSL::Cipher::CipherError), ActiveSupport::MessageEncryptor::SEPARATOR, ActiveSupport::MessageEncryptor::AUTH_TAG_LENGTH, ActiveSupport::MessageEncryptor::InvalidMessage.superclass.name])
ActiveSupport::MessageEncryptor.use_authenticated_message_encryption = true
show("authenticated default", [ActiveSupport::MessageEncryptor.default_cipher, ActiveSupport::MessageEncryptor.key_len, ActiveSupport::MessageEncryptor.new(KEY).encrypt_and_sign("a").count("-")])
ActiveSupport::MessageEncryptor.use_authenticated_message_encryption = false

made["encryptor"] = {
  "cbc" => message,
  "gcm" => gcm_message,
  "url" => url_message,
  "purpose" => cbc.encrypt_and_sign({ "x" => [1, nil] }, purpose: "transfer"),
  "json" => json_enc.encrypt_and_sign(value),
  "old" => old_enc.encrypt_and_sign({ "old" => true }),
  "sign" => sign_secret.encrypt_and_sign("signed"),
}

# --- SecureCompareRotator ----------------------------------------------------
ROTATIONS.clear
plain = build_rotation(ActiveSupport::SecureCompareRotator, ["new-value", "old-value"])
show("secure_compare!", [plain.secure_compare!("new-value"), ROTATIONS.dup, plain.secure_compare!("old-value"), ROTATIONS.dup, outcome { plain.secure_compare!("other") }, outcome { plain.secure_compare!("new-valu") }])
seen = []
show("secure_compare! override", [plain.secure_compare!("old-value", on_rotation: -> { seen << :x }), seen])
lone = ActiveSupport::SecureCompareRotator.new("v")
show("rotate return", [lone.rotate("w"), lone.rotate("u"), lone.secure_compare!("u")])
show("rotator ancestors", [ActiveSupport::SecureCompareRotator.ancestors.take_while { |m| m != Object }.map(&:name), ActiveSupport::SecureCompareRotator::InvalidMatch.superclass.name, lone.respond_to?(:secure_compare), lone.send(:secure_compare, "a", "a")])

# --- ActiveSupport::JSON --------------------------------------------------------
Point = Struct.new(:x, :y)
class Plain
  def initialize
    @a = 1
    @b = [Time.utc(2020, 5, 6, 7, 8, 9, 123456)]
  end
end
show("json encode", [
  ActiveSupport::JSON.encode({ a: 1, "b" => [nil, true, 1.5, "<script>&</script>"], 3 => :sym }),
  ActiveSupport::JSON.encode("\u2028\u2029"),
  ActiveSupport::JSON.encode(Time.utc(2020, 5, 6, 7, 8, 9, 123456)),
  ActiveSupport::JSON.encode(Float::NAN),
  ActiveSupport::JSON.encode(Point.new(1, 2)),
  ActiveSupport::JSON.encode(Plain.new),
  ActiveSupport::JSON.encode(1..3),
  ActiveSupport::JSON.encode(String),
  ActiveSupport::JSON.encode({ "a" => "<" }, escape: false),
  ActiveSupport::JSON.encode({ "a" => "<", "b" => 2 }, only: "a"),
  ActiveSupport::JSON.dump([1]),
])
show("json decode", [ActiveSupport::JSON.decode('{"a":[1,2.5,null,"x"]}'), ActiveSupport::JSON.load("[1]"), ActiveSupport::JSON.decode('"2020-01-01"'), outcome { ActiveSupport::JSON.decode("{") }.split(":").first, ActiveSupport::JSON.parse_error.name])
show("json switches", [ActiveSupport.escape_html_entities_in_json, ActiveSupport.use_standard_json_time_format, ActiveSupport.time_precision, ActiveSupport.parse_json_times])

# --- The other side's messages --------------------------------------------
if OTHER_PATH
  other = JSON.parse(File.read(OTHER_PATH))
  v = other["verifier"]
  show("other verifier", [verifier.verify(v["plain"]), verifier.verify(v["purpose"], purpose: :login), as_json.verify(v["json"]), rotating.verify(v["rotated"]), ROTATIONS.last])
  e = other["encryptor"]
  show("other encryptor", [cbc.decrypt_and_verify(e["cbc"]), gcm.decrypt_and_verify(e["gcm"]), url_enc.decrypt_and_verify(e["url"]), cbc.decrypt_and_verify(e["purpose"], purpose: "transfer"), cbc.decrypt_and_verify(e["purpose"]), json_enc.decrypt_and_verify(e["json"]), rot_enc.decrypt_and_verify(e["old"]), sign_secret.decrypt_and_verify(e["sign"]), outcome { cbc.decrypt_and_verify(e["sign"]) }])
end

File.write(MADE_PATH, JSON.generate(made)) if MADE_PATH
