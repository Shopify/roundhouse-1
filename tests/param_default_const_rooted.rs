//! A method's parameter default reads its constants in the class's lexical
//! scope, as the body does, so a resolved reference is rooted where an
//! enclosing namespace declares a constant of its first segment's name.
//! Core's `DeliveryPromise::PromiseConfigurationHelperTest#enable_flag(
//! buyer_id: BUYER_TOKEN)` is written `DeliveryPromise::...::BUYER_TOKEN`,
//! which inside `module DeliveryPromise` binds `DeliveryPromise` to the
//! sibling class `DeliveryPromise::DeliveryPromise`: "uninitialized
//! constant DeliveryPromise::DeliveryPromise::PromiseConfigurationHelperTest".
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const SIBLING: &str = "module Dp\n  class Dp\n  end\nend\n";

const HELPER: &str = r#"module Dp
  class Helper
    TOKEN = "x"
    TYPE = "y"

    def enable(buyer_id: TOKEN, kind: Helper::TYPE)
      [buyer_id, kind]
    end
  end
end
"#;

#[test]
fn a_parameter_default_is_rooted_like_the_body() {
    let run = emit_and_run::empty_app()
        .write("app/models/dp/dp.rb", SIBLING)
        .write("app/models/dp/helper.rb", HELPER)
        .run_ruby("require_relative \"app/models/dp/dp\"\nrequire_relative \"app/models/dp/helper\"\np Dp::Helper.new.enable\n");
    run.assert_passes();
    assert_eq!(run.stdout, "[\"x\", \"y\"]\n");
    let source = std::fs::read_to_string(run.emitted.join("app/models/dp/helper.rb")).expect("emitted helper");
    assert!(source.contains("buyer_id: ::Dp::Helper::TOKEN"), "{source}");
}
