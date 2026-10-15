//! A module whose `included(base)` hook reads `base.ancestors` (Core's
//! `Apps::ApiService` raises unless `Apps::Extension` or an
//! `ExtensionImplementation` is among them), included by a class that
//! includes the required module through a local aliasing `self` above
//! it (Core's Retail::CartPromotion). Ruby runs the hook at the `include`,
//! so the earlier include has to come first; the include bucket wrote
//! `include Checked` at the top of the body and the hook raised.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

#[test]
fn an_include_whose_hook_reads_ancestors_follows_the_statement_above_it() {
    let run = emit_and_run::empty_app()
        .write("app/models/marker.rb", "module Marker\nend\n")
        .write(
            "app/models/checked.rb",
            r#"module Checked
  class << self
    def included(base)
      raise "needs Marker" unless base.ancestors.any? { |a| a == Marker }
    end
  end
end
"#,
        )
        .write(
            "app/models/promotion.rb",
            r#"class Promotion
  include ActiveModel::Validations
  unsafe_self = self #: as untyped
  unsafe_self.include(Marker)
  include Checked

  X = 1
end
"#,
        )
        .run_ruby("p [Promotion.include?(Checked), Promotion.include?(Marker)]\n");
    assert!(run.success, "{}\n{}", run.stdout, run.stderr);
    assert_eq!(run.stdout, "[true, true]\n", "{}", run.stderr);
}
