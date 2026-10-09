//! A class that includes one of the runtime's ActiveModel ports, as
//! activeresource's ActiveResource::Base does (`include
//! ActiveModel::Conversion`, base.rb:1864), answers that port's methods:
//! its `exists?`, `element_path`, `element_url` and `reload` call a bare
//! `to_param`, which Conversion defines. On the PostgreSQL CRuby tree,
//! which carries the port (runtime/spinel/active_model_naming.rb), that is
//! a plain send Ruby dispatches, not an "Object extension" refusal that
//! raises where the call runs (`analyze::body::runtime_base::module_answers`).
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const TICKET: &str = r#"class Ticket
  include ActiveModel::Conversion

  def id
    5
  end

  def persisted?
    true
  end

  def path
    "/tickets/#{to_param}"
  end

  def key_of(other)
    other.to_key
  end
end
"#;

const UNKNOWN: &str = r#"class Stub
  include ActiveModel::Conversion

  def path
    "/stubs/#{some_unknown_conversion_api}"
  end
end
"#;

fn overlay(files: &[(&str, &str)]) -> emit_and_run::Overlay {
    files.iter().fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text))
}

#[test]
fn an_includer_of_a_runtime_active_model_port_dispatches_its_methods_on_postgresql() {
    let files = [
        ("roundhouse.yml", "database_adapter: postgresql\n"),
        ("config/database.yml", "test:\n  adapter: postgresql\n  database: rh_e2e_port_include\n"),
        ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
        ("db/structure.sql", "CREATE TABLE widgets (\n    id bigserial NOT NULL,\n    CONSTRAINT widgets_pkey PRIMARY KEY (id)\n);\n"),
        ("app/models/ticket.rb", TICKET),
        ("app/models/stub.rb", UNKNOWN),
    ];
    let (emitted, errors) = overlay(&files).emit(roundhouse::project::BuildTarget::Ruby);
    let ticket = std::fs::read_to_string(emitted.join("app/models/ticket.rb")).expect("emitted ticket");
    assert!(ticket.contains("#{to_param}"), "no plain to_param in:\n{ticket}");
    assert!(ticket.contains("other.to_key"), "no plain to_key in:\n{ticket}");
    assert!(!ticket.contains("not supported"), "{ticket}");
    assert!(
        !errors.iter().any(|e| e.contains("to_param") || e.contains("to_key")),
        "{errors:#?}"
    );
}

/// Without the PostgreSQL tree the port is not this tree's: `to_param`
/// on the class stays the named gap.
#[test]
fn an_includer_of_a_runtime_active_model_port_stays_a_named_gap_without_postgresql() {
    let files = [
        ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
        ("db/schema.rb", "ActiveRecord::Schema.define(version: 1) do\nend\n"),
        ("app/models/ticket.rb", TICKET),
    ];
    let (_emitted, errors) = overlay(&files).emit(roundhouse::project::BuildTarget::Ruby);
    assert!(
        errors.iter().any(|e| e.contains("to_param lacks a shared runtime/lowering implementation")),
        "{errors:#?}"
    );
}
