//! Circular module accessors must not thrash the harvest table.
//!
//! A `config` / `Configuration.load!` cycle can alternate
//! `Configuration ↔ Configuration|Untyped` every fixpoint round and
//! exhaust `FIXPOINT_CAP`. Stabilizing returns that differ only by
//! top-level `Untyped`/`Var` arms lets the signature converge.

use roundhouse::det::HashMap;
use std::path::PathBuf;

use roundhouse::emit::ruby::emit_library;
use roundhouse::ingest::ingest_app_from_tree;

const CIRCULAR: &str = r#"
module Probe
  def self.config
    @config ||= Configuration.load!
  end

  class Configuration
    def self.load!
      if config_exists?
        Probe.config.apply_upgrades
      else
        new
      end
    end

    def self.config_exists?
      true
    end

    def apply_upgrades
      self
    end
  end
end

class Client
  def boot
    Probe.config.apply_upgrades
  end
end
"#;

#[test]
fn circular_config_load_emits_configuration_not_gradual_churn() {
    let mut tree: HashMap<PathBuf, Vec<u8>> = HashMap::default();
    tree.insert(
        PathBuf::from("db/schema.rb"),
        b"ActiveRecord::Schema.define do\nend\n".to_vec(),
    );
    tree.insert(PathBuf::from("lib/probe.rb"), CIRCULAR.as_bytes().to_vec());
    let mut app = ingest_app_from_tree(tree).expect("ingest");
    roundhouse::session::analyze_and_lower(&mut app);
    let rbs = emit_library(&app)
        .into_iter()
        .filter(|f| f.path.extension().is_some_and(|e| e == "rbs"))
        .map(|f| f.content)
        .collect::<Vec<_>>()
        .join("\n");
    // RBS collapses `T | untyped` to bare `untyped`, so checking for the
    // substring "Configuration | untyped" never fails. Pin the settled
    // signatures themselves — without stabilize, `config` ends as
    // `-> untyped` while the class name still matches a naive search.
    assert!(
        rbs.contains("def self.config: () -> Probe::Configuration"),
        "expected Probe.config to settle on Probe::Configuration, got:\n{rbs}"
    );
    // `new` on a class-side `self` answers the receiving class, which a
    // singleton signature spells `instance` (`Sub.load!` builds a Sub);
    // the other branch is the configuration `Probe.config` holds.
    assert!(
        rbs.contains("def self.load!: () -> (instance | Probe::Configuration)"),
        "expected Configuration.load! to settle on `instance | Probe::Configuration`, got:\n{rbs}"
    );
}
