//! Emitted-program regression: an app module's `self.included(base)`
//! hook that calls the including class's own class methods runs after
//! them, as the source wrote it.
//!
//! secure_headers 5.2 (`headers/content_security_policy_config.rb`, a
//! gem Core loads as source) defines `ATTRS` and `def self.attrs` and
//! THEN includes `DynamicConfig`, whose hook does
//! `base.send(:attr_reader, *base.attrs)`. The Ruby emitter wrote every
//! include first in the class body, so loading the file raised
//! `undefined method 'attrs' for class SecureHeaders::
//! ContentSecurityPolicyConfig` (a boot blocker of Core's
//! query_error_log_subscriber_test). The expected output is what CRuby
//! prints for the same source (see bin/probe-check).

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const HOOK_PROBE: &str = r##"module HookProbe
  module DynamicConfig
    def self.included(base)
      base.send(:attr_writer, :modified)
      base.send(:attr_reader, *base.attrs)
      base.attrs.each do |attr|
        base.send(:define_method, "#{attr}=") do |value|
          instance_variable_set("@#{attr}", value)
        end
      end
    end

    def to_h
      self.class.attrs.each_with_object({}) { |key, hash| hash[key] = send(key) }
    end
  end

  class Config
    ATTRS = [:default_src, :img_src].freeze
    def self.attrs
      ATTRS
    end

    include DynamicConfig

    DEFAULT = { default_src: "self" }.freeze

    def initialize(hash)
      @default_src = nil
      @img_src = nil
      hash.each { |k, v| send("#{k}=", v) }
    end

    def report_only?
      false
    end
  end

  def self.report
    c = Config.new(Config::DEFAULT)
    c.img_src = "data:"
    [c.to_h, c.report_only?]
  end
end
"##;

#[test]
fn an_included_hook_reading_class_methods_runs_after_them() {
    let run = emit_and_run::real_blog()
        .write("app/models/hook_probe.rb", HOOK_PROBE)
        .run_ruby("p HookProbe.report\n");
    run.assert_passes();
    // CRuby on the source above.
    assert_eq!(run.stdout.trim_end(), r#"[{default_src: "self", img_src: "data:"}, false]"#);
    let config = std::fs::read_to_string(run.emitted.join("app/models/hook_probe/config.rb"))
        .expect("emitted config.rb");
    let attrs = config.find("def self.attrs").expect("attrs def");
    let include = config.find("include HookProbe::DynamicConfig").expect("include");
    assert!(attrs < include, "the include precedes the class method its hook calls:\n{config}");
}
