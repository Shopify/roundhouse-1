//! A library class whose `delegate` splats its names from a constant's
//! list (`delegate(*Commands::TABLE.keys, to: :upstream)`).
//!
//! `ingest::delegate` expands literal names only, so the call used to
//! reach the emit as an unmodelled class-body call and was dropped
//! (`warning[lower_residue]: delegate in the body of ... is not modelled`):
//! Core's `Podding::Redis::PoddedClient` forwards every
//! `::Redis::Namespace::COMMANDS` key to the pod's client, and with the
//! call dropped `evalsha` (sabertooth's rate limiter) was undefined on it.
//! The Ruby family replays the call as written; `Module#delegate`
//! (runtime/module_delegate.rb) defines the forwarders as the body loads.
//!
//! CRuby running the authored source is the oracle.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const COMMANDS: &str = r##"module Commands
  TABLE = { "get" => [:key], "set" => [:key] }.merge("evalsha" => [:scripts]).freeze
end
"##;

const SOURCE: &str = r##"class Upstream
  def get(key) = "get #{key}"
  def set(key, value) = "set #{key} #{value}"
  def evalsha(digest, *args, keys: [], argv: []) = "evalsha #{digest} #{args.inspect} #{keys.inspect} #{argv.inspect}"
  def with = yield(self)
end

class PodClient
  def initialize(upstream)
    @upstream = upstream
  end

  self.delegate(
    *::Commands::TABLE.keys, #: as untyped
    to: :upstream
  )

  def with(&block)
    upstream.with(&block)
  end

  private

  def upstream
    @upstream
  end
end
"##;

const SCRIPT: &str = r#"client = PodClient.new(Upstream.new)
p client.get("k")
p client.set("k", 1)
p client.evalsha("d1", 1, keys: ["a"], argv: [2])
p PodClient.public_method_defined?(:evalsha)
p client.with { |u| u.class }
"#;

const EXPECTED: &str = "\"get k\"\n\"set k 1\"\n\"evalsha d1 [1] [\\\"a\\\"] [2]\"\ntrue\nUpstream\n";

#[test]
fn a_delegate_splatting_a_constants_keys_forwards_every_name_as_in_cruby() {
    let native = emit_and_run::ruby()
        .arg("-e")
        .arg(format!("require \"active_support/core_ext/module/delegation\"\n{COMMANDS}\n{SOURCE}\n{SCRIPT}"))
        .output()
        .expect("CRuby control");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    assert_eq!(String::from_utf8_lossy(&native.stdout), EXPECTED, "native control");

    let run = emit_and_run::real_blog()
        .write("app/services/commands.rb", COMMANDS)
        .write("app/services/pod_client.rb", SOURCE)
        .run_ruby(SCRIPT);
    run.assert_passes();
    assert_eq!(run.stdout, EXPECTED, "{}", run.stderr);
}
