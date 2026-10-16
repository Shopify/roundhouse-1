//! A module constant aliasing a class of the class that prepends the
//! module.
//!
//! Core's `Semian::TaxService` (components/taxes/app/utils/semian/
//! tax_service.rb) is prepended by `TaxServiceClient` and holds
//! `ResourceBusyError = ::TaxServiceClient::ResourceBusyError`. A scoped
//! read looks through the owner's ancestors, prepends first, so the
//! reference resolved to the constant being defined and was emitted as
//! `(raise "roundhouse: constant not supported (all targets)")`: the
//! module failed to load and every test reaching TaxServiceClient died.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const CLIENT: &str = "class TaxServiceClient\n  prepend Semian::TaxService\n\n  class Error < StandardError\n  end\nend\n";
const BUSY: &str = "class TaxServiceClient\n  class ResourceBusyError < TaxServiceClient::Error\n  end\nend\n";
const ADAPTER: &str = "module Semian\n  module TaxService\n    ResourceBusyError = ::TaxServiceClient::ResourceBusyError\n\n    \
def self.busy\n      ResourceBusyError\n    end\n  end\nend\n";

#[test]
fn the_alias_of_a_prepending_class_constant_resolves_and_loads() {
    let run = emit_and_run::empty_app()
        .write("app/services/tax_service_client.rb", CLIENT)
        .write("app/utils/tax_service_client/resource_busy_error.rb", BUSY)
        .write("app/utils/semian/tax_service.rb", ADAPTER)
        .run_ruby(
            "puts [Semian::TaxService.busy.name, Semian::TaxService::ResourceBusyError.equal?(TaxServiceClient::ResourceBusyError), \
Semian::TaxService.busy.superclass.name].inspect\n",
        );
    run.assert_passes();
    assert_eq!(run.stdout.trim(), r#"["TaxServiceClient::ResourceBusyError", true, "TaxServiceClient::Error"]"#);
    assert!(!run.stderr.contains("not supported"), "{}", run.stderr);
}
