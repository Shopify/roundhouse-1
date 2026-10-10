//! `thread_mattr_accessor` with Rails' options.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

/// Core's ShopVersioning declares `thread_mattr_accessor
/// :version_bumps_enabled, instance_accessor: false, default: true`.
/// The options call used to be left as an unknown class-body call and
/// dropped from the module, so the reader raised NoMethodError. Native
/// ActiveSupport answers the default until a write (a nil write reads
/// nil) and each thread starts from the default.
#[test]
fn a_thread_mattr_accessor_with_a_default_and_no_instance_accessor() {
    let run = emit_and_run::empty_app()
        .write("config/environments/development.rb", "Rails.application.configure do\n  config.eager_load = false\nend\n")
        .write(
            "app/models/concerns/versioning.rb",
            "module Versioning\n  extend T::Helpers\n\n  requires_ancestor { Object }\n\n  thread_mattr_accessor :bumps_enabled, instance_accessor: false, default: true\n\n  class << self\n    def without_bumps\n      old = bumps_enabled\n      self.bumps_enabled = false\n      yield\n    ensure\n      self.bumps_enabled = old\n    end\n  end\nend\n",
        )
        .run_ruby("p Versioning.bumps_enabled\nVersioning.without_bumps { p Versioning.bumps_enabled }\np Versioning.bumps_enabled\nVersioning.bumps_enabled = nil\np Versioning.bumps_enabled\np Thread.new { Versioning.bumps_enabled }.value\n");
    let v = std::fs::read_to_string(run.emitted.join("app/models/versioning.rb")).unwrap_or_default();
    assert!(run.success, "{v}\n{}\n{}", run.stdout, run.stderr);
    assert_eq!(run.stdout, "true\nfalse\ntrue\nnil\ntrue\n", "{v}\n{}", run.stderr);
}
