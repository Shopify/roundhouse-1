//! A class-body call's require edges name only what its load reads.
//!
//! The constants a replayed class-body call names are load-time needs,
//! so its file requires their files at its top. Two kinds of call made
//! edges the source does not have: one the emit DROPS (it is not in the
//! emitted body at all), and a closure a replayed call only stores
//! (`register :zeta, -> { Zeta::NAME }`), whose body runs on call. Each
//! such edge can close a require cycle: `Alpha` requiring `zeta` at its
//! top while `Zeta::DEFAULT = Alpha.build` reads `Alpha` at load left
//! `Alpha` undefined there (`require_relative`'s mid-load short-circuit).
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const REGISTRY: &str = r#"module Registry
  #: (Symbol name, Proc factory) -> void
  def register(name, factory)
    factories[name] = factory
  end

  #: () -> Hash[Symbol, Proc]
  def factories
    @factories ||= {} #: Hash[Symbol, Proc]?
  end
end
"#;

const ALPHA: &str = r#"class Alpha
  extend Registry

  register :zeta, -> { Zeta::NAME }

  #: () -> Alpha
  def self.build
    new
  end

  #: () -> String
  def self.zeta_name
    factories.fetch(:zeta).call
  end
end
"#;

const ZETA: &str = r#"class Zeta
  NAME = "zeta"
  DEFAULT = Alpha.build
end
"#;

#[test]
fn a_stored_closure_s_constants_are_not_load_time_requires() {
    let app = || {
        emit_and_run::empty_app()
            .write("app/models/registry.rb", REGISTRY)
            .write("app/models/alpha.rb", ALPHA)
            .write("app/models/zeta.rb", ZETA)
    };
    let (emitted, _) = app().emit(BuildTarget::Ruby);
    let alpha = std::fs::read_to_string(emitted.join("app/models/alpha.rb")).expect("alpha.rb");
    assert!(!alpha.contains("require_relative \"zeta\""), "{alpha}");
    assert!(alpha.contains("register :zeta"), "{alpha}");
    let run = app().run_ruby("p Alpha.zeta_name, Zeta::DEFAULT.class\n");
    run.assert_passes();
    assert_eq!(run.stdout, "\"zeta\"\nAlpha\n");
}
