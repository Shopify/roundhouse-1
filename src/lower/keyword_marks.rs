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
use crate::expr::{ERASED_KEYWORD_SPLAT, Expr};

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
