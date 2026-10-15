//! A class body that names constants in a list statement of their own,
//! only so each one loads where the body runs: Core's
//! `GraphApi::Admin::DomainType` lists `::Domain` and its subclasses
//! ("so that Domain.descendants call can bring these") and then
//! validates its values against `::Domain.descendants`, which lists only
//! the subclasses that have loaded. The list has no effect but those
//! reads, and the emitted body dropped it, so four of the nine subclasses
//! were never loaded and the enum's values were "unexpected".
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

fn app() -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("app/models/domain.rb", "class Domain\nend\n")
        .write("app/models/open_srs_domain.rb", "class OpenSrsDomain < Domain\nend\n")
        .write("app/models/third_party_domain.rb", "class ThirdPartyDomain < Domain\nend\n")
        .write(
            "app/models/domain_type.rb",
            "class DomainType\n  [\n    ::Domain,\n    ::OpenSrsDomain,\n    ::ThirdPartyDomain,\n  ]\n\n  SEEN = ::Domain.subclasses.map(&:name).sort\nend\n",
        )
}

#[test]
fn a_list_statement_of_constants_loads_them_where_the_body_runs() {
    let run = app().run_ruby("p DomainType::SEEN\n");
    assert!(run.success, "{}\n{}", run.stdout, run.stderr);
    assert_eq!(run.stdout, "[\"OpenSrsDomain\", \"ThirdPartyDomain\"]\n", "{}", run.stderr);
}
