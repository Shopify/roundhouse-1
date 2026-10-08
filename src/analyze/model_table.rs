//! A concrete model whose table the schema does not declare: every query
//! it makes names a relation that does not exist. Silent until now; 47
//! Core models read such tables, most through table-name rules ingest
//! did not apply (nested-in-model, acronyms). Not this: an abstract base,
//! a tableless ActiveModel class (no superclass), an STI subclass (its
//! parent's table), and an app with no schema tables at all.
//!
//! A model routed to another database configuration is checked against
//! the same schema: Core's PostgreSQL test environment points all six
//! configurations at one database, loaded from one dump. Only a
//! configuration that names a separate schema file (a manifest's
//! `schema_source` other than the primary's) is held out, with a warning
//! naming it, so the exclusion is visible.

use crate::database::ModelDatabase;
use crate::det::HashSet;
use crate::diagnostic::{Diagnostic, Severity};
use crate::lower::model_to_library::markers::is_abstract_class;
use crate::App;

pub(super) fn diagnose(app: &App) -> Vec<Diagnostic> {
    if app.schema.tables.is_empty() {
        return Vec::new();
    }
    let concrete: HashSet<&str> =
        app.models.iter().filter(|m| !is_abstract_class(m)).map(|m| m.name.0.as_str()).collect();
    let primary_schema = app
        .databases
        .iter()
        .find(|d| d.primary)
        .and_then(|d| d.schema_source.as_ref())
        .map(|s| s.path.as_str());
    let mut out = Vec::new();
    for model in &app.models {
        let Some(parent) = &model.parent else { continue };
        let parent = parent.0.as_str().trim_start_matches("::");
        let sti = concrete.contains(parent)
            || model.name.0.as_str().rsplit_once("::").is_some_and(|(ns, _)| concrete.contains(format!("{ns}::{parent}").as_str()));
        if sti || is_abstract_class(model) || app.schema.tables.contains_key(&model.table.0) {
            continue;
        }
        let separate = match app.model_databases.get(&model.name) {
            Some(ModelDatabase::Named { writing, .. }) => app
                .databases
                .iter()
                .find(|d| &d.name == writing)
                .and_then(|d| d.schema_source.as_ref())
                .filter(|s| Some(s.path.as_str()) != primary_schema)
                .map(|s| (writing.as_str(), s.path.as_str())),
            _ => None,
        };
        if let Some((config, path)) = separate {
            let mut d = Diagnostic::unsupported(model.span, None, "model_table_other_schema", format!(
                "`{}` reads table `{}` on database `{config}`, whose schema file `{path}` is not the typing schema; its table is not checked",
                model.name.0, model.table.0
            ));
            d.severity = Severity::Warning;
            out.push(d);
            continue;
        }
        // Name the configuration a routed model reaches: without a
        // manifest, which schema that database loads is not known here.
        let on = match app.model_databases.get(&model.name) {
            Some(ModelDatabase::Named { writing, .. }) => format!(" on database `{writing}`"),
            _ => String::new(),
        };
        out.push(Diagnostic::unsupported(model.span, None, "model_table_not_in_schema", format!(
            "`{}` reads table `{}`{on}, which the schema does not declare; every query it makes names a relation that does not exist",
            model.name.0, model.table.0
        )));
    }
    out
}
