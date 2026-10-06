//! A NOT NULL column's in-memory slot is the column's type, not `T | nil`
//! (`ty_of_column_slot`), and the constructor fills an absent or nil input
//! with the schema default or the type's zero (`attrs[:shop_id] || 0`).
//! Rails holds nil there until save, so `validates :col, presence: true`
//! on `new(col: nil)` reports "can't be blank" where the emitted program
//! sees a present value. The one slot that still answers as Rails does is
//! a string with no default: its zero is the blank `""`. Every other NOT
//! NULL slot is refused (a boolean's `false` is blank to Rails as well,
//! which the generic presence check does not model), until an unsaved
//! record can hold nil there.

use crate::diagnostic::Diagnostic;
use crate::dialect::ValidationRule;
use crate::lower::model_to_library::{ty_of_column, ty_of_column_slot};
use crate::ty::Ty;
use crate::App;

pub(super) fn diagnose(app: &App) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for model in &app.models {
        let Some(table) = app.schema.tables.get(&model.table.0) else { continue };
        for (span, validation) in model.spanned_validations() {
            if !validation.rules.iter().any(|r| matches!(r, ValidationRule::Presence)) {
                continue;
            }
            let Some(col) = table.columns.iter().find(|c| c.name == validation.attribute) else { continue };
            if matches!(ty_of_column_slot(col), Ty::Union { .. }) || col.primary_key {
                continue;
            }
            let ty = ty_of_column(&col.col_type);
            if ty == Ty::Str && col.default.is_none() {
                continue;
            }
            let held = match &col.default {
                Some(default) => format!("its default {default:?}"),
                None if ty == Ty::Bool => "false (blank to Rails' presence, present to the emitted nil-or-empty check)".to_string(),
                None => "the type's zero".to_string(),
            };
            out.push(Diagnostic::unsupported(span, None, "presence_on_not_null_column",
                format!("{}.{} is NOT NULL, so an unsaved record given nil holds {held} where Rails holds nil; `validates :{}, presence: true` cannot fail as Rails' does",
                    model.name.0, col.name, col.name)));
        }
    }
    out
}
