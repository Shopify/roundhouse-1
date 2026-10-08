//! A concrete model whose table the schema does not declare: every query
//! it makes names a relation that does not exist. Silent until now; 47
//! Core models read such tables, most through table-name rules ingest
//! did not apply (nested-in-model, acronyms). Not this: an abstract base,
//! a tableless ActiveModel class (no superclass), an STI subclass (its
//! parent's table), a model routed to another database (whose schema
//! `app.schema` is not; the routing is refused on its own), and an app
//! with no schema tables at all.

use crate::database::ModelDatabase;
use crate::det::HashSet;
use crate::diagnostic::Diagnostic;
use crate::lower::model_to_library::markers::is_abstract_class;
use crate::App;

pub(super) fn diagnose(app: &App) -> Vec<Diagnostic> {
    if app.schema.tables.is_empty() {
        return Vec::new();
    }
    let concrete: HashSet<&str> =
        app.models.iter().filter(|m| !is_abstract_class(m)).map(|m| m.name.0.as_str()).collect();
    let mut out = Vec::new();
    for model in &app.models {
        let Some(parent) = &model.parent else { continue };
        let parent = parent.0.as_str().trim_start_matches("::");
        let sti = concrete.contains(parent)
            || model.name.0.as_str().rsplit_once("::").is_some_and(|(ns, _)| concrete.contains(format!("{ns}::{parent}").as_str()));
        if sti
            || is_abstract_class(model)
            || app.schema.tables.contains_key(&model.table.0)
            || app.model_databases.get(&model.name).is_some_and(|d| !matches!(d, ModelDatabase::Primary))
        {
            continue;
        }
        out.push(Diagnostic::unsupported(model.span, None, "model_table", format!(
            "`{}` reads table `{}`, which the schema does not declare; every query it makes names a relation that does not exist",
            model.name.0, model.table.0
        )));
    }
    out
}
