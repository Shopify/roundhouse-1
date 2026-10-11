//! A Concern's `included do cattr_accessor :x; self.x = {...} end` on a
//! plain (non-model) includer: `Module#cattr_accessor`
//! (active_support/core_ext/module/attribute_accessors) is on every class,
//! and the `self.x =` seed after it writes through the accessor the
//! declaration just made, so the block runs on the includer as written.
//! The block was dropped by name, and the includer's class-body macro
//! (`ClassMethods#after_enable`, which reads the seeded table) then raised
//! NoMethodError as the file loaded.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const CALLBACKS: &str = r#"module Callbacks
  extend ActiveSupport::Concern

  included do
    cattr_accessor :callbacks
    self.callbacks = { after_enable: {}, after_disable: {} }
  end

  def fire(action, feature)
    (callbacks[action][feature] || []).map { |callback| send(callback) }
  end

  module ClassMethods
    def after_enable(feature, callback)
      (callbacks[:after_enable][feature] ||= []) << callback
    end
  end
end
"#;

const FEATURES: &str = r#"class Features
  include Callbacks

  after_enable :pos, :configure_pos

  def configure_pos
    :configured
  end
end
"#;

const SCRIPT: &str = r#"p Features.new.fire(:after_enable, :pos)
p Features.callbacks
p Features.new.callbacks.equal?(Features.callbacks)
"#;

#[test]
fn a_concern_s_cattr_accessor_and_seed_run_on_a_plain_includer() {
    let native = emit_and_run::ruby()
        .arg("-e")
        .arg(format!(
            "require \"active_support\"\nrequire \"active_support/concern\"\nrequire \"active_support/core_ext/module/attribute_accessors\"\n{CALLBACKS}\n{FEATURES}\n{SCRIPT}"
        ))
        .output()
        .expect("CRuby control");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    let expected = "[:configured]\n{after_enable: {pos: [:configure_pos]}, after_disable: {}}\ntrue\n";
    assert_eq!(String::from_utf8_lossy(&native.stdout), expected);
    let run = emit_and_run::empty_app()
        .write("app/models/concerns/callbacks.rb", CALLBACKS)
        .write("app/models/features.rb", FEATURES)
        .run_ruby(SCRIPT);
    run.assert_passes();
    assert_eq!(run.stdout, expected);
}
