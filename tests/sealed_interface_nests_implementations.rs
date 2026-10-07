//! A Sorbet sealed interface that nests its own implementations is a
//! module those classes include, not a namespace.
//!
//! Core's `StorefrontCarts::CartCompletion::CompletionAttempt::State` is
//! `extend T::Helpers; interface!; sealed!` followed by `class Failed;
//! include State; …; end` and three more such classes (Core has 15
//! modules of this shape). The marker-module rule admitted only the
//! marker calls, so the nested classes made `State` a namespace; each
//! `include State` reported "includes unresolved State" and `is_a?(State)`
//! read a constant no class defined.
//!
//! The control runs the same source on CRuby without the three Sorbet
//! statements: sorbet-runtime is not part of the test toolchain, and the
//! statements are declarations (the classes `sealed!` allows are all in
//! this file), which the emitted tree drops as it drops `sig`.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const SORBET_MARKERS: &str = "      extend T::Helpers\n      interface!\n      sealed!\n";

const ATTEMPT: &str = r#"module Carts
  class CompletionAttempt
    module State
      extend T::Helpers
      interface!
      sealed!

      class Failed
        include State

        attr_reader :errors

        def initialize(errors:)
          @errors = errors
        end
      end

      class Success
        include State

        attr_reader :receipt

        def initialize(receipt:)
          @receipt = receipt
        end
      end
    end

    def initialize(state)
      @state = state
    end

    def failed?
      @state.is_a?(State::Failed)
    end

    def state?
      @state.is_a?(State)
    end
  end
end
"#;

const SCRIPT: &str = r#"failed = Carts::CompletionAttempt::State::Failed.new(errors: [:declined])
ok = Carts::CompletionAttempt::State::Success.new(receipt: 7)
p [failed.errors, ok.receipt]
p [Carts::CompletionAttempt.new(failed).failed?, Carts::CompletionAttempt.new(ok).failed?]
p [Carts::CompletionAttempt.new(ok).state?, Carts::CompletionAttempt.new(:other).state?]
p Carts::CompletionAttempt::State.class
p Carts::CompletionAttempt::State::Success.include?(Carts::CompletionAttempt::State)
"#;

const EXPECTED: &str = "[[:declined], 7]\n[true, false]\n[true, false]\nModule\ntrue\n";

#[test]
fn a_sealed_interface_holding_its_implementations_is_their_mixin() {
    assert!(ATTEMPT.contains(SORBET_MARKERS));
    let native = emit_and_run::ruby()
        .arg("-e")
        .arg(format!("{}\n{SCRIPT}", ATTEMPT.replace(SORBET_MARKERS, "")))
        .output()
        .expect("native Ruby control");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    assert_eq!(String::from_utf8_lossy(&native.stdout), EXPECTED);
    let run = emit_and_run::empty_app()
        .write("app/models/carts/completion_attempt.rb", ATTEMPT)
        .run_ruby(SCRIPT);
    run.assert_passes();
    assert_eq!(run.stdout, EXPECTED);
}

/// Ingest surfaces `State` as a module of its own beside its nested
/// classes. Without a Sorbet marker, a module of nested declarations
/// stays a namespace: the rule does not turn every wrapper into a mixin.
#[test]
fn only_a_marked_module_of_declarations_is_surfaced() {
    let names = |src: &str| -> Vec<(String, bool)> {
        roundhouse::ingest::ingest_library_classes(src.as_bytes(), "app/models/x.rb")
            .expect("ingest")
            .into_iter()
            .map(|c| (c.name.0.as_str().to_string(), c.is_module))
            .collect()
    };
    let attempt = names(ATTEMPT);
    for (name, module) in [
        ("Carts::CompletionAttempt", false),
        ("Carts::CompletionAttempt::State", true),
        ("Carts::CompletionAttempt::State::Failed", false),
        ("Carts::CompletionAttempt::State::Success", false),
    ] {
        assert!(attempt.contains(&(name.to_string(), module)), "{name} in {attempt:?}");
    }
    let plain = names("module Wrap\n  extend T::Sig\n\n  class Inner\n    def x = 1\n  end\nend\n");
    assert_eq!(plain, [("Wrap::Inner".to_string(), false)]);
}
