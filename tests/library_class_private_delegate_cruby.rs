//! A library class whose `delegate` carries `private: true`
//! (`delegate :schema_key, to: :class, private: true`).
//!
//! `ingest::delegate` does not expand a `private:` option (the forwarder
//! would come out public), so the call used to reach the emit as an
//! unmodelled class-body call and was dropped: Core's
//! `Markets::ShopScope::Cached#market_definitions` called `schema_key`
//! and raised `undefined method 'schema_key'`. The Ruby family replays the
//! call as written; `Module#delegate` (runtime/module_delegate.rb) defines
//! the private forwarder as the body loads.
//!
//! CRuby running the authored source is the oracle.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const SOURCE: &str = r##"class Scope
  def self.schema_key(*models)
    "key:#{models.map(&:name).join(",")}"
  end

  delegate :schema_key, to: :class, private: true

  def definitions
    "defs:#{schema_key(Scope, String)}"
  end
end
"##;

const SCRIPT: &str = r#"scope = Scope.new
p scope.definitions
p Scope.private_method_defined?(:schema_key)
p Scope.public_method_defined?(:schema_key)
"#;

const EXPECTED: &str = "\"defs:key:Scope,String\"\ntrue\nfalse\n";

#[test]
fn a_private_delegate_to_the_class_forwards_privately_as_in_cruby() {
    let native = emit_and_run::ruby()
        .arg("-e")
        .arg(format!("require \"active_support/core_ext/module/delegation\"\n{SOURCE}\n{SCRIPT}"))
        .output()
        .expect("CRuby control");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    assert_eq!(String::from_utf8_lossy(&native.stdout), EXPECTED, "native control");

    let run = emit_and_run::real_blog().write("app/services/scope.rb", SOURCE).run_ruby(SCRIPT);
    run.assert_passes();
    assert_eq!(run.stdout, EXPECTED, "{}", run.stderr);
}
