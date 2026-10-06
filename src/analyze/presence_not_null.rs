//! A NOT NULL column's in-memory slot is the column's type, not `T | nil`
//! (`ty_of_column_slot`), and the constructor fills an absent or nil input
//! with the type's zero (`attrs[:shop_id] || 0`). For a numeric column
//! that zero is *present*, so `validates :shop_id, presence: true` can never
//! fail on `new(shop_id: nil)`, where Rails reports "can't be blank" before
//! the database is reached. Strings and booleans zero to a blank value and
//! still fail the check, so they are not refused. Refuse the numeric case
//! until an unsaved record can hold nil in a NOT NULL slot.

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
            if matches!(ty_of_column(&col.col_type), Ty::Int | Ty::Float) {
                out.push(Diagnostic::unsupported(span, None, "presence_on_not_null_numeric",
                    format!("{}.{} is NOT NULL, so an unsaved record holds 0 where Rails holds nil; `validates :{}, presence: true` cannot fail as Rails' does",
                        model.name.0, col.name, col.name)));
            }
        }
    }
    out
}
