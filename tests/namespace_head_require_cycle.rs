//! A load-time read written through a namespace-only head
//! (`Gql::NegotiationError` inside `module Probe::Gql`, where `Gql` is a
//! module no file declares) names the class of the enclosing scope.
//!
//! The require resolver found the head only when the enclosing scope held
//! a *class* of that name, so the read named no file and the two files'
//! load-time cycle went unseen: Core's `NegotiationError` interface
//! required its violation classes above its own header, and each one
//! `implements(Gql::NegotiationError)` as it loaded, before the interface
//! was defined ("uninitialized constant ...::NegotiationError", 6
//! unit-1000 ids). A cycle's requires are written inside the body, right
//! after the header, where the class is defined.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const BASE: &str = "class ProbeObject\n  def self.implements(mod)\n    (@interfaces ||= []) << mod\n  end\n\n  def self.interfaces\n    @interfaces\n  end\nend\n\nmodule ProbeInterface\nend\n";
const IFACE: &str = "module Probe\n  module Gql\n    module NegotiationError\n      include ::ProbeInterface\n\n      TYPES = {\n        \"a\" => AcceptViolation,\n      }.freeze\n    end\n  end\nend\n";
const TYPE: &str = "module Probe\n  module Gql\n    class AcceptViolation < ::ProbeObject\n      implements(Gql::NegotiationError)\n    end\n  end\nend\n";

fn app() -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write("app/models/probe_object.rb", BASE)
        .write("app/models/probe/gql/negotiation_error.rb", IFACE)
        .write("app/models/probe/gql/accept_violation.rb", TYPE)
}

#[test]
fn the_interface_requires_its_implementors_inside_its_body() {
    let run = app().run_ruby("p Probe::Gql::NegotiationError::TYPES.keys; p Probe::Gql::AcceptViolation.interfaces");
    run.assert_passes();
    assert_eq!(run.stdout.lines().collect::<Vec<_>>(), ["[\"a\"]", "[Probe::Gql::NegotiationError]"], "{}", run.stdout);
    let iface = std::fs::read_to_string(run.emitted.join("app/models/probe/gql/negotiation_error.rb")).unwrap();
    let header = iface.find("module NegotiationError").expect("header");
    let require = iface.find("require_relative \"accept_violation\"").expect("the implementor is required");
    assert!(header < require, "the require waits for the header:\n{iface}");
}
