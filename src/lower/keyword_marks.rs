//! Marks for keyword arguments a lowering passes positionally.
//!
//! Ingest flattens an optional keyword parameter into a
//! positional-with-default (`Param::from_keyword`) and `**rest` into
//! `rest = {}` (`Param::from_kwrest`). The ruby-family trees write those
//! parameters back as keywords. A call that a lowering built or rewrote
//! against the flattened shape then has to say which of its positional
//! arguments were keywords. The marks are `expr::SOURCE_KEYWORD_ARG`,
//! `expr::KEYWORD_DEFAULT_FILL` and `expr::ERASED_KEYWORD_SPLAT`. Every
//! other target ignores them and keeps the positional call.

use crate::dialect::Param;
use crate::expr::{ERASED_KEYWORD_SPLAT, Expr, KEYWORD_DEFAULT_FILL, SOURCE_KEYWORD_ARG};

/// All three marks. Each describes the argument SLOT, not the value in it.
const SLOT_MARKS: u64 = SOURCE_KEYWORD_ARG | KEYWORD_DEFAULT_FILL | ERASED_KEYWORD_SPLAT;

/// An argument slot's keyword marks, read off the value that fills it so
/// that a rewrite which REBUILDS the value can put them back.
///
/// A rewrite that writes a fresh `Expr` over a call argument (`*e =
/// Expr::new(…)`) keeps the slot but drops the marks, because they live
/// on the value. The ruby family then writes that argument positionally
/// while its neighbours keep their labels. `helper_kwargs::respell` turned
/// `Growth::Dsl.validate(@source, path: @path, schema: Growth::Dsl.growth_schema)`
/// into marked positionals, and `scope_chain::rewrite` rebuilt every send
/// it visited, so the emitted call read `path: @path,
/// Growth::Dsl.growth_schema`, which Ruby rejects at parse time. Only
/// the sends lost their labels; the literals and ivars beside them were
/// never rebuilt.
pub(crate) struct SlotMarks {
    decisions: u64,
    keyword_arg: Option<Box<crate::ident::Symbol>>,
}

impl SlotMarks {
    pub(crate) fn of(e: &Expr) -> Self {
        Self { decisions: e.decisions & SLOT_MARKS, keyword_arg: e.keyword_arg.clone() }
    }

    /// Put the marks back on whatever now fills the slot. An unmarked
    /// slot stays unmarked.
    pub(crate) fn restore(self, e: &mut Expr) {
        if self.decisions == 0 {
            return;
        }
        e.decisions = (e.decisions & !SLOT_MARKS) | self.decisions;
        e.keyword_arg = self.keyword_arg;
    }
}

/// Mark `arg`, a read of the enclosing method's own parameter `p`, for a
/// synthesized call that forwards it to a callee declaring `p` the same way
/// (a class-side wrapper's `new.m(a, b)`). A flattened keyword passes on as
/// `p: p`, and a flattened `**rest` as `**rest`.
pub(crate) fn mark_forwarded_param(arg: &mut Expr, p: &Param) {
    if p.from_keyword {
        arg.mark_source_keyword(p.name.clone());
    } else if p.from_kwrest {
        arg.decisions |= ERASED_KEYWORD_SPLAT;
    }
}
