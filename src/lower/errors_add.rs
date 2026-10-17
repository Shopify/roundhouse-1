//! `errors.add` grounding: `errors.add(:field, "msg")` →
//! `errors << "Field msg"` and `error_details << ActiveRecord::ErrorDetail.new("field", "msg", "msg", "Field msg", false, true)`,
//! so the accumulator keeps what Rails keeps for each message; `errors.clear`
//! empties both.
//!
//! The shared runtime's error accumulator is a plain `Array[String]` —
//! the validates lowering bakes humanized full messages at lower time
//! ("Short can't be blank") — so hand-written `add` calls ground into
//! the same shape. `:base` contributes the bare message (Rails
//! semantics); a dynamic message interpolates after the humanized
//! field; a missing message defaults to Rails' "is invalid".
//!
//! Any receiver spelling grounds: `errors` as a zero-arg send on
//! `self` (a model adding to its own errors during validation) or on
//! another expression (`record.errors.add(...)` from a controller —
//! lobsters' duplicate-comment guard). The rewrite keeps the receiver,
//! so both land on the same accumulator. A local or parameter field
//! (an EachValidator's `record.errors.add(attribute, "msg")`) grounds
//! to `attribute.to_s == "base" ? "msg" : "#{ActiveSupport.humanize(
//! attribute.to_s)} msg"`, the same text evaluated at run time. A
//! Symbol message is an error type: the count-free entries of Rails'
//! default en.yml (`:blank` → "can't be blank", `:taken`, ...) bake to
//! their text. Any other field expression, error type or option set
//! still joins the residue ledger — the accumulator is an
//! `Array[String]`, which has no `add`, so on strict targets each such
//! site is a named per-target gap rather than a silent compile error
//! (`lower_residue` warning, pass `errors_add`). Adjacent
//! string-literal concats in the message (`"a " << "b"`, lobsters'
//! line-wrap idiom) fold to one literal first — a runtime `<<` on a
//! frozen literal is a hazard the bake sidesteps.
//!
//! `errors.full_message(:field, msg)` bakes the same text without touching
//! the accumulator: Rails answers the humanized field before the message
//! (the bare message for `:base`), which is a pure function of the two
//! arguments. A record that is not a plain read is still built before the
//! text, as `User.new && "Email is invalid"`. Only that shape is typed
//! (`full_message_bakes`); any other spelling keeps analyze's Errors gap.
//!
//! Purely shape-directed; runs on the post-analyze hook
//! (`apply_post_analyze_lowerings`) with its siblings so every target
//! consumes the grounded form. Scope is `for_each_hook_body` (views
//! excluded like every hook pass — the construct has no view presence).

use crate::app::App;
use crate::diagnostic::Diagnostic;
use super::active_model_model::Reading;
use crate::expr::{Expr, ExprNode, InterpPart, Literal};
use crate::ident::Symbol;
use crate::ty::Ty;

/// Rewrite self-receiver `errors.add` sends across every hook body.
/// Returns the residue ledger: recognizable `errors.add` sites left
/// dynamic, with the reason.
pub fn apply_errors_add_lowering(app: &mut App) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    let localized = app.locale_files.first().map(|f| (f.clone(), app.locale_files.len()));
    let port = super::active_model_model::ValidationsPort::of(app);
    let data = LabelData::of(app);
    let own_errors = OwnErrors::of(app);
    super::for_each_owned_hook_body(app, &mut |owner, body| {
        let owner = owner.cloned();
        let locals = port.locals(body);
        let site = Site {
            localized: localized.as_ref(),
            port: &port,
            owner: owner.as_ref(),
            locals: &locals,
            labels: data.at(owner.as_ref()),
            own_errors: &own_errors,
        };
        rewrite_errors_add(body, &site, &mut diags)
    });
    diags.extend(port.take_notes());
    diags
}

/// Each model's stamped i18n, and the app's for an `errors` whose model
/// is not known, held apart from the `App` the walk borrows mutably.
pub(crate) struct LabelData {
    models: std::collections::HashMap<crate::ident::ClassId, crate::i18n::ModelI18n>,
    unknown: crate::i18n::ModelI18n,
}

impl LabelData {
    pub(crate) fn of(app: &App) -> Self {
        LabelData {
            models: app.models.iter().map(|m| (m.name.clone(), m.i18n.clone())).collect(),
            unknown: crate::i18n::ModelI18n {
                scope: "activerecord".into(),
                keys: Vec::new(),
                catalog: app.i18n.subset_for("activerecord", &[]),
            },
        }
    }

    pub(crate) fn at<'a>(&'a self, owner: Option<&'a crate::ident::ClassId>) -> Labels<'a> {
        Labels { data: self, owner }
    }
}

/// What a baked message needs to name its attribute as Rails would.
pub(crate) struct Labels<'a> {
    data: &'a LabelData,
    owner: Option<&'a crate::ident::ClassId>,
}

impl Labels<'_> {
    /// The text a full message for `field` starts with, before its message;
    /// `None` when `errors.format` does not end in the message.
    pub(crate) fn message_prefix(&self, errors_reader: &Expr, field: &str) -> Option<String> {
        let i18n = self.model(errors_reader);
        let format = i18n.errors_format().replace("%{attribute}", &i18n.human_attribute_name(field));
        format.strip_suffix("%{message}").filter(|p| !p.contains("%{message}")).map(str::to_string)
    }

    /// The model whose `errors` this is: the reader's receiver's type, or the owner for `errors` on self.
    fn model(&self, errors_reader: &Expr) -> &crate::i18n::ModelI18n {
        self.known_model(errors_reader).unwrap_or(&self.data.unknown)
    }

    /// [`Labels::model`], `None` where the reader's record is not one of the app's models.
    fn known_model(&self, errors_reader: &Expr) -> Option<&crate::i18n::ModelI18n> {
        let ExprNode::Send { recv, .. } = &*errors_reader.node else { return None };
        let id = match recv.as_ref().map(|r| (&*r.node, r.ty.as_ref())) {
            None | Some((ExprNode::SelfRef, _)) => self.owner,
            Some((_, Some(crate::ty::Ty::Class { id, .. }))) => Some(id),
            _ => None,
        };
        id.and_then(|id| self.data.models.get(id))
    }
}

fn residue(expr: &Expr, reason: &str) -> Diagnostic {
    crate::lower::residue_diagnostic(
        "errors_add",
        "errors.add",
        expr.span,
        reason,
        format!(
            "`errors.add` left as dynamic dispatch ({reason}) — the error \
             accumulator is an Array[String] with no `add`; ground by hand \
             or extend the errors_add lowering"
        ),
    )
}

/// Fold `"a" << "b"` / `"a" + "b"` chains of string literals into one
/// literal (left-recursively, so multi-line wraps fold whole). Any
/// non-literal operand leaves the expression untouched.
fn fold_str_concat(e: Expr) -> Expr {
    let ExprNode::Send { recv: Some(l), method, args, block: None, .. } = &*e.node else {
        return e;
    };
    if !(method.as_str() == "<<" || method.as_str() == "+") || args.len() != 1 {
        return e;
    }
    let left = fold_str_concat(l.clone());
    let (ExprNode::Lit { value: Literal::Str { value: lv } },
         ExprNode::Lit { value: Literal::Str { value: rv } }) = (&*left.node, &*args[0].node)
    else {
        return e;
    };
    Expr::new(e.span, ExprNode::Lit { value: Literal::Str { value: format!("{lv}{rv}") } })
}

/// Where a body sits: the app's locale files, which classes keep
/// ActiveModel::Errors (the port) with the body's owner, and the labels
/// the owner's i18n names attributes by.
struct Site<'a> {
    localized: Option<&'a (String, usize)>,
    port: &'a super::active_model_model::ValidationsPort,
    owner: Option<&'a crate::ident::ClassId>,
    locals: &'a super::active_model_model::PortLocals,
    labels: Labels<'a>,
    own_errors: &'a OwnErrors,
}

/// The library classes whose `errors` is their own method (asset_cloud's
/// `AssetCloud::Validations`: `@errors ||= []`), with every class that
/// includes or inherits one. Their `errors` is not the framework's
/// accumulator, and nothing defines an `error_details` beside it, so the
/// lowerings that record details stand off them.
pub(crate) struct OwnErrors(std::collections::HashSet<crate::ident::ClassId>);

impl OwnErrors {
    pub(crate) fn of(app: &App) -> Self {
        let classes: std::collections::HashMap<&crate::ident::ClassId, &crate::dialect::LibraryClass> =
            app.library_classes.iter().map(|c| (&c.name, c)).collect();
        let defines = |c: &crate::dialect::LibraryClass| {
            // A source-written `errors` only: the accumulator the model
            // lowering synthesizes for a tableless model (synthetic name
            // span) IS the framework's, and its `add` grounds.
            c.methods.iter().any(|m| {
                m.receiver == crate::dialect::MethodReceiver::Instance
                    && m.name.as_str() == "errors"
                    && m.params.is_empty()
                    && !m.name_span.is_synthetic()
            })
        };
        let own = |start: &crate::dialect::LibraryClass| {
            let mut seen: Vec<&crate::ident::ClassId> = Vec::new();
            let mut queue = vec![start];
            while let Some(c) = queue.pop() {
                if seen.contains(&&c.name) {
                    continue;
                }
                seen.push(&c.name);
                if defines(c) {
                    return true;
                }
                let parent = c.parent.iter().filter(|_| !c.is_module);
                for name in c.includes.iter().chain(parent) {
                    let name = crate::ident::ClassId(Symbol::from(name.0.as_str().trim_start_matches("::")));
                    queue.extend(classes.get(&name).copied());
                }
            }
            false
        };
        OwnErrors(app.library_classes.iter().filter(|c| own(c)).map(|c| c.name.clone()).collect())
    }

    /// Whether the `errors` read `reader` (`errors`, `self.errors`,
    /// `record.errors`) is read off such a class: the owner for an
    /// implicit or `self` receiver, the receiver's class where it is typed.
    pub(crate) fn reads(&self, reader: &Expr, owner: Option<&crate::ident::ClassId>) -> bool {
        let ExprNode::Send { recv, .. } = &*reader.node else { return false };
        let named = |ty: &Ty| matches!(ty, Ty::Class { id, .. } if self.0.contains(id));
        match recv.as_ref() {
            None => owner.is_some_and(|o| self.0.contains(o)),
            Some(r) if matches!(&*r.node, ExprNode::SelfRef) => owner.is_some_and(|o| self.0.contains(o)),
            Some(r) => match r.ty.as_ref() {
                Some(Ty::Union { variants }) => variants.iter().any(named),
                Some(ty) => named(ty),
                None => false,
            },
        }
    }
}

fn rewrite_errors_add(expr: &mut Expr, site: &Site<'_>, diags: &mut Vec<Diagnostic>) {
    let localized = site.localized;
    let labels = &site.labels;
    expr.node
        .for_each_child_mut(&mut |c| rewrite_errors_add(c, site, diags));
    if let ExprNode::Send { recv, method, args, block, .. } = &*expr.node {
        if method.as_str() == "full_message" && full_message_bakes(recv.as_ref(), args, block.as_ref()) {
            let span = expr.span;
            let node = std::mem::replace(&mut *expr.node, ExprNode::Seq { exprs: vec![] });
            let ExprNode::Send { recv, args, .. } = node else { unreachable!() };
            let i18n = labels.model(recv.as_ref().expect("checked errors reader"));
            let mut args = args.into_iter();
            let field_expr = args.next().expect("checked two args");
            let ExprNode::Lit { value: Literal::Sym { value: field } } = &*field_expr.node else {
                unreachable!()
            };
            let mut message = baked_message(i18n, span, field.as_str(), args.next().map(fold_str_concat));
            message.ty = Some(crate::ty::Ty::Str);
            // A collection held in a local (`errors = record.errors`) has no record to build.
            let owner = match *recv.expect("checked errors reader").node {
                ExprNode::Send { recv: owner, .. } => owner,
                _ => None,
            };
            *expr = match owner {
                // Not a Seq: an emitter hoists one out of `||` and `?:`, which would build the record where Rails does not.
                Some(owner) if !reads_without_effect(&owner) => {
                    let mut built = Expr::new(
                        span,
                        ExprNode::BoolOp { op: crate::expr::BoolOpKind::And, surface: crate::expr::BoolOpSurface::Symbol, left: owner, right: message },
                    );
                    built.ty = Some(crate::ty::Ty::Str);
                    built
                }
                _ => message,
            };
            return;
        }
    }
    if rewrite_import(expr, labels, diags) {
        return;
    }
    if let ExprNode::Send { recv: Some(r), method, args, block: None, .. } = &*expr.node {
        if method.as_str() == "clear" && args.is_empty() {
            if let ExprNode::Send { recv: owner, method: em, args: ea, .. } = &*r.node {
                if em.as_str() == "errors" && ea.is_empty() {
                    if site.own_errors.reads(r, site.owner) {
                        return;
                    }
                    let span = expr.span;
                    let mut owner = owner.clone();
                    let ty = expr.ty.clone();
                    // A record that is not a plain read is bound once, as both clears read it.
                    let hoisted = (!owner_reads_once(owner.as_ref())).then(|| {
                        let (assign, read) = hoist(span, ERROR_RECORD, owner.take().expect("not a plain read"));
                        owner = Some(read.clone());
                        *errors_owner(expr) = Some(read);
                        assign
                    });
                    let clear_errors = std::mem::replace(expr, Expr::new(span, ExprNode::Seq { exprs: vec![] }));
                    let mut clear_details = Expr::new(
                        span,
                        ExprNode::Send { recv: Some(error_details(owner, span)), method: Symbol::from("clear"), args: Vec::new(), block: None, parenthesized: false },
                    );
                    clear_details.ty = Some(error_details_ty());
                    // Details first, so the value is the `errors` array the caller cleared
                    *expr = Expr::new(span, ExprNode::Seq { exprs: vec![clear_details, clear_errors] });
                    expr.ty = ty.clone();
                    if let Some(assign) = hoisted {
                        *expr = Expr::new(span, ExprNode::Seq { exprs: vec![assign, expr.clone()] });
                        expr.ty = ty;
                    }
                    return;
                }
            }
        }
    }
    // Any `add` on an `errors` reader is this pass's construct; decide
    // ground-vs-residue below.
    let recognized = matches!(
        &*expr.node,
        ExprNode::Send { recv: Some(r), method, block: None, .. }
            if method.as_str() == "add"
                && matches!(&*r.node, ExprNode::Send { method: em, args: ea, .. }
                    if em.as_str() == "errors" && ea.is_empty())
    );
    if !recognized {
        return;
    }
    // The rewrite targets the shared runtime's `Array[String]`
    // accumulator. An `errors` reader the analyzer typed as a class
    // (an app's own error collection, e.g. CustomerFoundations::Errors
    // with `add(attribute, code)`) is not that accumulator: its `add`
    // is an ordinary method call and stays as written. The framework's
    // `ActiveModel::Errors` is the accumulator and grounds.
    let ExprNode::Send { recv: Some(r), .. } = &*expr.node else { unreachable!() };
    if r.ty.as_ref().is_some_and(names_own_class) || site.own_errors.reads(r, site.owner) {
        return;
    }
    // A class over the ActiveModel::Validations port (and a module only
    // those include) holds the real ActiveModel::Errors: `add` is its
    // own, with Rails' arguments and messages.
    match site.port.classify(r, site.owner, site.locals) {
        Reading::Port => return,
        Reading::Refused => {
            site.port.refuse(expr, "errors.add");
            return;
        }
        Reading::Untyped => site.port.note_untyped("errors_add", "errors.add", expr.span),
        Reading::Accumulator => {}
    }
    // Every tree keeps an `error_details` array on the record, which the
    // push appends an `ActiveRecord::ErrorDetail` to, so the record is
    // read twice; the CRuby and JRuby trees also record each push through
    // the runtime's `ActiveModel::ErrorAttributes` (an `IrHint` the
    // emitter realizes), which reads it once. A record that is not a plain
    // read keeps the second only, and the ledger says so.
    let owner_once = match &*expr.node {
        ExprNode::Send { recv: Some(r), .. } => match &*r.node {
            ExprNode::Send { recv: owner, .. } => owner_reads_once(owner.as_ref()),
            _ => false,
        },
        _ => false,
    };
    // The field: a Symbol literal is baked at lower time; a local or
    // parameter (`errors.add(attribute, …)` in an EachValidator's
    // `validate_each`) is humanized at run time. Anything else stays
    // residue: the field would be evaluated twice.
    let field_kind = match &*expr.node {
        ExprNode::Send { args, .. } if !args.is_empty() && (args.len() <= 2 || typed_message_option(args)) => match &*args[0].node {
            ExprNode::Lit { value: Literal::Sym { .. } } => Some(true),
            ExprNode::Var { .. } => Some(false),
            _ => None,
        },
        _ => None,
    };
    let Some(static_field) = field_kind else {
        let reason = match &*expr.node {
            ExprNode::Send { args, .. }
                if args.first().is_some_and(
                    |a| !matches!(&*a.node, ExprNode::Lit { value: Literal::Sym { .. } })) =>
            {
                "dynamic field"
            }
            _ => "unrecognized arg shape",
        };
        diags.push(residue(expr, reason));
        return;
    };
    let span = expr.span;
    let node = std::mem::replace(&mut *expr.node, ExprNode::Seq { exprs: vec![] });
    let ExprNode::Send { mut recv, args, .. } = node else { unreachable!() };
    let mut args = args.into_iter();
    let field_expr = args.next().expect("checked non-empty");
    let mut msg = args.next().map(fold_str_concat);
    // `errors.add(:field, :type, message: "…")`: the `message:` option
    // wins over the type's own text, as with the options spelling below.
    if let Some(options) = args.next() {
        msg = Some(fold_str_concat(options));
    }
    // Rails' options spelling: `errors.add(:field, message: "…")`.
    // The kwargs hash IS the message carrier — unwrap a sole
    // `message:` entry; any other option set stays residue (put the
    // Send back — it was already moved out).
    let sole_message = matches!(
        msg.as_ref().map(|m| &*m.node),
        Some(ExprNode::Hash { entries, .. })
            if entries.len() == 1
                && matches!(&*entries[0].0.node,
                    ExprNode::Lit { value: Literal::Sym { value } }
                        if value.as_str() == "message")
    );
    if sole_message {
        let Some(ExprNode::Hash { entries, .. }) = msg.take().map(|m| *m.node) else {
            unreachable!()
        };
        msg = Some(fold_str_concat(entries.into_iter().next().unwrap().1));
    }
    // A Symbol message is an error TYPE, which Rails translates
    // (`errors.add(attr, :blank)` reads "can't be blank"). On a Symbol
    // field the model's i18n (its locale data over Rails' defaults) gives
    // the text. A local field's model and attribute are only known at run
    // time, so there the count-free entries of activemodel's and
    // activerecord's default en.yml bake to their text, unless the app
    // has locale files that could say otherwise; any other type stays
    // residue.
    let symbol_type = match msg.as_ref().map(|m| &*m.node) {
        Some(ExprNode::Lit { value: Literal::Sym { value } }) => Some(value.clone()),
        _ => None,
    };
    let translated = if static_field || localized.is_some() {
        None
    } else {
        symbol_type.as_ref().and_then(|t| default_error_message(t.as_str()))
    };
    // On a Symbol field a type the catalog words only with a count
    // (`:too_long`) has no message to bake: Rails raises for the missing
    // `%{count}`.
    // With the app's locale data, a type's text is the record's model's,
    // which a record of no known model leaves to run time.
    let uncounted = static_field
        && symbol_type.as_ref().is_some_and(|t| {
            let ExprNode::Lit { value: Literal::Sym { value: field } } = &*field_expr.node else { return false };
            let reader = recv.as_ref().expect("checked errors reader");
            let known = labels.known_model(reader);
            (localized.is_some() && known.is_none())
                || labels.model(reader).try_error(field.as_str(), t.as_str(), &Default::default()).is_none()
        });
    let unrecognized = matches!(msg.as_ref().map(|m| &*m.node), Some(ExprNode::Hash { .. }))
        || uncounted
        || (!static_field && symbol_type.is_some() && translated.is_none());
    if unrecognized {
        let reason = match (&symbol_type, localized) {
            (Some(t), Some((first, n))) => format!(
                "error type :{} resolves through the app's locale data ({first}{})",
                t.as_str(),
                if *n > 1 { format!(" and {} more", n - 1) } else { String::new() }
            ),
            (Some(t), None) => format!("error type :{} has no count-free default message", t.as_str()),
            (None, _) => "unrecognized arg shape".to_string(),
        };
        diags.push(residue(&Expr::new(span, ExprNode::Seq { exprs: vec![] }), &reason));
        *expr.node = ExprNode::Send {
            recv,
            method: Symbol::from("add"),
            args: vec![field_expr, msg.unwrap()],
            block: None,
            parenthesized: true,
        };
        return;
    }
    // Rails' default error type, with no options: `errors.add(:lines)`
    // and `errors.add(:lines, :invalid)` are the same Error as the one a
    // has_many adds for an invalid record (`emit::ruby::
    // collection_autosave`), which `errors.uniq!` keeps once.
    let invalid_type = static_field
        && !sole_message
        && (msg.is_none() || symbol_type.as_ref().is_some_and(|t| t.as_str() == "invalid"));
    // The Error's type and options (`errors.details`): Rails' default
    // `:invalid` without a message or with `message:` alone (whose option
    // `details` drops), the Symbol type, or a String message, which is
    // its own type. A message of unknown type is not claimed.
    let detail = match (&symbol_type, &msg) {
        _ if sole_message => crate::expr::ErrorDetail::Type { name: Symbol::from("invalid"), options: Some(Vec::new()) },
        (Some(t), _) => crate::expr::ErrorDetail::Type { name: t.clone(), options: Some(Vec::new()) },
        (None, None) => crate::expr::ErrorDetail::Type { name: Symbol::from("invalid"), options: Some(Vec::new()) },
        (None, Some(m))
            if matches!(&*m.node, ExprNode::Lit { value: Literal::Str { .. } } | ExprNode::StringInterp { .. })
                || m.ty == Some(Ty::Str) =>
        {
            crate::expr::ErrorDetail::Message
        }
        (None, Some(_)) => crate::expr::ErrorDetail::Unknown,
    };
    let (symbolic, optionless) = detail_flags(&detail);
    if let Some(text) = translated {
        msg = Some(str_lit(span, text));
    }
    // The push reads the record and the message twice (the message and its
    // detail); one that is not a plain read is bound once first.
    let mut hoists: Vec<Expr> = Vec::new();
    if let Some(m) = msg.take() {
        let carrier = matches!(&*m.node, ExprNode::Hash { .. } | ExprNode::Lit { value: Literal::Sym { .. } });
        msg = Some(if carrier || reads_without_effect_deep(&m) {
            m
        } else {
            let (assign, read) = hoist(span, ERROR_MESSAGE, m);
            hoists.push(assign);
            read
        });
    }
    if !owner_once {
        let reader = recv.as_mut().expect("checked errors reader");
        let ExprNode::Send { recv: owner, .. } = &mut *reader.node else { unreachable!("an errors reader") };
        let (assign, read) = hoist(span, ERROR_RECORD, owner.take().expect("not a plain read"));
        hoists.push(assign);
        *owner = Some(read);
    }
    // What the push records on the trees that keep `error_details`: the
    // error type and the bare message. `None` where the message is not a
    // plain read, which the record would then keep twice.
    let mut details_args: Option<(Expr, Expr)> = None;
    let mut message: Expr = if static_field {
        let ExprNode::Lit { value: Literal::Sym { value: field } } = &*field_expr.node else {
            unreachable!()
        };
        let reader = recv.as_ref().expect("checked errors reader");
        let i18n = labels.model(reader);
        // Rails looks a Symbol message (or none, `:invalid`) up as an error type.
        let kind = match msg.as_ref().map(|m| &*m.node) {
            None => Some("invalid".to_string()),
            Some(ExprNode::Lit { value: Literal::Sym { value } }) => Some(value.as_str().to_string()),
            _ => None,
        };
        let ExprNode::Send { recv: reader_owner, .. } = &*reader.node else { unreachable!() };
        match kind {
            Some(kind) => {
                details_args = Some((
                    str_lit(span, &kind),
                    parts_expr(span, i18n.error(field.as_str(), &kind, &Default::default()), reader_owner.as_ref(), field.as_str()),
                ));
                parts_expr(span, i18n.full_error(field.as_str(), &kind, &Default::default()), reader_owner.as_ref(), field.as_str())
            }
            None => {
                let bare = msg.clone().unwrap_or_else(|| unreachable!("a missing message is `:invalid`"));
                // Rails' type for a String message is the message itself
                if reads_without_effect_deep(&bare) {
                    details_args = Some((bare.clone(), bare));
                }
                baked_message(i18n, span, field.as_str(), msg)
            }
        }
    } else {
        // `attribute.to_s == "base" ? msg : "#{ActiveSupport.humanize(
        // attribute.to_s)} #{msg}"`: Rails' full message is the bare
        // message on :base and "<human attribute name> <message>"
        // otherwise. The field is a local, so reading it twice is free.
        let msg = msg.unwrap_or_else(|| str_lit(span, "is invalid"));
        // The record's detail: a Symbol message is the type, any other
        // plain-read message its own.
        details_args = match &symbol_type {
            Some(t) => Some((str_lit(span, t.as_str()), msg.clone())),
            None if msg.hint.is_none() && reads_without_effect_deep(&msg) => Some((msg.clone(), msg.clone())),
            None => None,
        };
        let field_s = typed(
            span,
            ExprNode::Send { recv: Some(field_expr.clone()), method: Symbol::from("to_s"), args: vec![], block: None, parenthesized: false },
            Ty::Str,
        );
        let is_base = typed(
            span,
            ExprNode::Send { recv: Some(field_s.clone()), method: Symbol::from("=="), args: vec![str_lit(span, "base")], block: None, parenthesized: false },
            Ty::Bool,
        );
        let human = typed(
            span,
            ExprNode::Send {
                recv: Some(typed(span, ExprNode::Const { path: vec![Symbol::from("ActiveSupport")] }, Ty::Class { id: crate::ident::ClassId(Symbol::from("ActiveSupport")), args: vec![].into() })),
                method: Symbol::from("humanize"),
                args: vec![field_s],
                block: None,
                parenthesized: true,
            },
            Ty::Str,
        );
        let tail = match &*msg.node {
            ExprNode::Lit { value: Literal::Str { value } } => InterpPart::Text { value: format!(" {value}") },
            _ => InterpPart::Expr { expr: msg.clone() },
        };
        let mut parts = vec![InterpPart::Expr { expr: human }];
        if matches!(tail, InterpPart::Expr { .. }) {
            parts.push(InterpPart::Text { value: " ".into() });
        }
        parts.push(tail);
        let full = typed(span, ExprNode::StringInterp { parts }, Ty::Str);
        typed(span, ExprNode::If { cond: is_base, then_branch: msg, else_branch: full }, Ty::Str)
    };
    // `<<` returns the accumulator it appends to (Array#<< / the errors
    // collection), so the synthesized send's result type is the
    // receiver's. Stamp it: this pass runs *after* analyze, and the
    // post-lowering `diagnose` walk reads stamped types without re-
    // dispatching — leaving `ty = None` here makes it false-positive a
    // `send_dispatch_failed` on a receiver it can see the type of.
    let recv_ty = recv.as_ref().and_then(|r| r.ty.clone());
    // The attribute the message is for and where its short message
    // starts, which the accumulator drops: the CRuby and JRuby trees
    // record them for `errors.attribute_names`, `to_hash`, `as_json` and
    // `messages`. A local's message is the one built just above, the bare
    // message on `base` and the run-time humanized prefix otherwise.
    let attribute = match &*field_expr.node {
        ExprNode::Lit { value: Literal::Sym { value } } => Some(crate::expr::IrHint::ErrorAttribute(
            super::model_to_library::validations::error_tag(value, &message, detail),
        )),
        ExprNode::Var { name, .. } => Some(crate::expr::IrHint::ErrorAttributeLocal(crate::expr::ErrorTag {
            attribute: name.clone(),
            short: crate::expr::ErrorShort::AfterHumanized,
            detail,
        })),
        _ => None,
    };
    if invalid_type {
        message.hint = Some(crate::expr::IrHint::InvalidErrorType);
    }
    let owner = match recv.as_ref().map(|r| &*r.node) {
        Some(ExprNode::Send { recv: owner, .. }) => owner.clone(),
        _ => unreachable!("checked errors reader"),
    };
    let mut push_message = Expr::new(
        span,
        ExprNode::Send { recv, method: Symbol::from("<<"), args: vec![message], block: None, parenthesized: false },
    );
    push_message.hint = attribute;
    push_message.ty = recv_ty;
    // Every tree also keeps the push's `ErrorDetail` on the record.
    let (kind, bare) = details_args.expect("the record and message are plain reads");
    *expr = {
        {
            let field = match &*field_expr.node {
                ExprNode::Lit { value: Literal::Sym { value } } => str_lit(span, value.as_str()),
                _ => typed(
                    span,
                    ExprNode::Send { recv: Some(field_expr.clone()), method: Symbol::from("to_s"), args: vec![], block: None, parenthesized: false },
                    Ty::Str,
                ),
            };
            attributed_push_field(push_message, owner, field, kind, symbolic, optionless, bare, span)
        }
    };
    if !hoists.is_empty() {
        let ty = expr.ty.clone();
        hoists.push(expr.clone());
        *expr = Expr::new(span, ExprNode::Seq { exprs: hoists });
        expr.ty = ty;
    }
}

/// What an `ErrorDetail` record keeps of the push's type: whether it is a
/// Symbol (a String message is its own type) and whether the error has no
/// options beside the callback and `message:` ones.
pub(crate) fn detail_flags(detail: &crate::expr::ErrorDetail) -> (bool, bool) {
    match detail {
        crate::expr::ErrorDetail::Type { options, .. } => (true, options.as_ref().is_some_and(|o| o.is_empty())),
        crate::expr::ErrorDetail::Message => (false, true),
        crate::expr::ErrorDetail::Unknown => (false, false),
    }
}

/// The locals a push binds a record and a message that are not plain reads to.
const ERROR_RECORD: &str = "error_record";
const ERROR_MESSAGE: &str = "error_message";

/// `<name> = <value>` and a read of it, both typed as the value.
fn hoist(span: crate::span::Span, name: &str, value: Expr) -> (Expr, Expr) {
    let ty = value.ty.clone();
    let id = crate::ident::VarId(0);
    let mut assign = Expr::new(
        span,
        ExprNode::Assign { target: crate::expr::LValue::Var { id, name: Symbol::from(name) }, value },
    );
    assign.ty = ty.clone();
    let mut read = Expr::new(span, ExprNode::Var { id, name: Symbol::from(name) });
    read.ty = ty;
    (assign, read)
}

/// The record of an `<owner>.errors.<method>` send.
fn errors_owner(expr: &mut Expr) -> &mut Option<Expr> {
    let ExprNode::Send { recv: Some(reader), .. } = &mut *expr.node else { unreachable!("an errors send") };
    let ExprNode::Send { recv: owner, .. } = &mut *reader.node else { unreachable!("an errors reader") };
    owner
}

fn is_errors_ty(e: &Expr) -> bool {
    matches!(&e.ty, Some(crate::ty::Ty::Class { id, .. }) if id.0.as_str() == "ActiveModel::Errors")
}

fn is_error_ty(e: &Expr) -> bool {
    matches!(&e.ty, Some(crate::ty::Ty::Class { id, .. }) if id.0.as_str() == "ActiveModel::Error")
}

/// The attribute `errors.import` files the error under: `None` for the error's own, else the `attribute:` override.
fn import_attribute(args: &[Expr]) -> Option<Option<&Expr>> {
    match args {
        [error] if is_error_ty(error) => Some(None),
        [error, options] if is_error_ty(error) => match &*options.node {
            ExprNode::Hash { entries, .. } => match entries.as_slice() {
                [(key, value)]
                    if matches!(&*key.node, ExprNode::Lit { value: Literal::Sym { value } } if value.as_str() == "attribute")
                        && matches!(value.ty, Some(crate::ty::Ty::Sym)) =>
                {
                    Some(Some(value))
                }
                _ => None,
            },
            _ => None,
        },
        _ => None,
    }
}

/// The record whose errors `errors.merge!` imports: an `errors` read, or a record.
fn merged_owner(args: &[Expr], is_model: &dyn Fn(&crate::ident::ClassId) -> bool) -> Option<Option<Expr>> {
    let [other] = args else { return None };
    match &*other.node {
        ExprNode::Send { recv, method, args, block: None, .. } if method.as_str() == "errors" && args.is_empty() && is_errors_ty(other) => {
            Some(recv.clone())
        }
        _ if matches!(&other.ty, Some(crate::ty::Ty::Class { id, .. }) if is_model(id)) => Some(Some(other.clone())),
        _ => None,
    }
}

/// Shared with analyze, which types `import` and `merge!` only when this holds.
pub(crate) fn import_grounds(method: &str, args: &[Expr], is_model: &dyn Fn(&crate::ident::ClassId) -> bool) -> bool {
    match method {
        "import" => import_attribute(args).is_some(),
        "merge!" => merged_owner(args, is_model).is_some(),
        _ => false,
    }
}

/// `errors.format` around the attribute's name and the message, when it names each once, in that order.
fn format_pieces(format: &str) -> Option<(String, String, String)> {
    let (before, rest) = format.split_once("%{attribute}")?;
    let (between, after) = rest.split_once("%{message}")?;
    (!before.contains("%{") && !between.contains("%{") && !after.contains("%{"))
        .then(|| (before.to_string(), between.to_string(), after.to_string()))
}

/// `errors.import(error, attribute: :field)` → `ActiveSupport.errors_import(…)` and
/// `errors.merge!(other)` → `ActiveSupport.errors_merge(…)`, with the importing model's
/// translated attribute names: Rails names an imported error's attribute as the importer does.
fn rewrite_import(expr: &mut Expr, labels: &Labels, diags: &mut Vec<Diagnostic>) -> bool {
    let ExprNode::Send { recv: Some(reader), method, args, block: None, .. } = &*expr.node else { return false };
    let method = method.as_str();
    if !matches!(method, "import" | "merge!") || !is_errors_ty(reader) {
        return false;
    }
    let ExprNode::Send { recv: owner, method: em, args: ea, .. } = &*reader.node else { return false };
    if em.as_str() != "errors" || !ea.is_empty() {
        return false;
    }
    let i18n = labels.model(reader);
    let pieces = format_pieces(i18n.errors_format());
    let is_model = |id: &crate::ident::ClassId| labels.data.models.contains_key(id);
    let reason = if !import_grounds(method, args, &is_model) {
        Some(match method {
            "import" => "an error and an `attribute:` Symbol are grounded, no other option",
            _ => "only another record or its `errors` is grounded",
        })
    } else if !owner_reads_once(owner.as_ref()) {
        Some("the record is not a plain read, and the import reads it twice")
    } else if pieces.is_none() {
        Some("`errors.format` does not name the attribute and then the message, once each")
    } else {
        None
    };
    if let Some(reason) = reason {
        // Left to the runtime's port (`ERRORS_PORT_CALL`), which answers any
        // form on the trees that carry it; the others refuse it by name.
        if expr.decisions & crate::expr::ERRORS_PORT_CALL != 0 {
            return false;
        }
        let mut d = crate::lower::residue_diagnostic(
            "errors_add",
            if method == "import" { "errors.import" } else { "errors.merge!" },
            expr.span,
            reason,
            format!("`errors.{method}` is not grounded: {reason}"),
        );
        d.severity = crate::diagnostic::Severity::Error;
        diags.push(d);
        return true;
    }
    let span = expr.span;
    let (before, between, after) = pieces.expect("checked above");
    let names: Vec<(Expr, Expr)> =
        i18n.translated_attribute_names().into_iter().map(|(attr, name)| (str_lit(span, &attr), str_lit(span, &name))).collect();
    let mut names = Expr::new(span, ExprNode::Hash { entries: names, kwargs: false });
    names.ty = Some(crate::ty::Ty::Hash { key: std::sync::Arc::new(crate::ty::Ty::Str), value: std::sync::Arc::new(crate::ty::Ty::Str) });
    let mut errors = reader.clone();
    errors.ty = Some(crate::ty::Ty::Array { elem: std::sync::Arc::new(crate::ty::Ty::Str) });
    let mut call_args = vec![errors, error_details(owner.clone(), span)];
    let helper = if method == "import" {
        let attribute = import_attribute(args).expect("checked above").map(|a| match &*a.node {
            ExprNode::Lit { value: Literal::Sym { value } } => str_lit(span, value.as_str()),
            _ => {
                let mut s = Expr::new(span, ExprNode::Send { recv: Some(a.clone()), method: Symbol::from("to_s"), args: vec![], block: None, parenthesized: false });
                s.ty = Some(crate::ty::Ty::Str);
                s
            }
        });
        call_args.push(args[0].clone());
        call_args.push(attribute.unwrap_or_else(|| {
            let mut nil = Expr::new(span, ExprNode::Lit { value: Literal::Nil });
            nil.ty = Some(crate::ty::Ty::Nil);
            nil
        }));
        "errors_import"
    } else {
        call_args.push(error_details(merged_owner(args, &is_model).expect("checked above"), span));
        "errors_merge"
    };
    call_args.extend([names, str_lit(span, &before), str_lit(span, &between), str_lit(span, &after)]);
    let mut support = Expr::new(span, ExprNode::Const { path: vec![Symbol::from("ActiveSupport")] });
    support.ty = Some(crate::ty::Ty::Class { id: crate::ident::ClassId(Symbol::from("ActiveSupport")), args: vec![].into() });
    let mut call = Expr::new(
        span,
        ExprNode::Send { recv: Some(support), method: Symbol::from(helper), args: call_args, block: None, parenthesized: true },
    );
    call.ty = Some(crate::lower::errors_index::error_array_ty());
    let orig = expr.clone();
    *expr = crate::lower::errors_index::port_form(orig, call);
    true
}

/// `<errors> << msg` followed by `<owner>.error_details << ActiveRecord::ErrorDetail.new("field", type, message, msg, symbolic, optionless)`,
/// so the accumulator keeps what Rails keeps for each message.
pub(crate) fn attributed_push(
    push_message: Expr,
    owner: Option<Expr>,
    field: &str,
    kind: Expr,
    symbolic: bool,
    optionless: bool,
    message: Expr,
    span: crate::span::Span,
) -> Expr {
    attributed_push_field(push_message, owner, str_lit(span, field), kind, symbolic, optionless, message, span)
}

/// [`attributed_push`] for a field read at run time (an attribute in a local).
fn attributed_push_field(
    push_message: Expr,
    owner: Option<Expr>,
    field: Expr,
    kind: Expr,
    symbolic: bool,
    optionless: bool,
    message: Expr,
    span: crate::span::Span,
) -> Expr {
    let ExprNode::Send { args, .. } = &*push_message.node else { unreachable!("an `errors <<` push") };
    let full_message = typed_str(args[0].clone());
    let error_ty = crate::ty::Ty::Class { id: crate::ident::ClassId(Symbol::from("ActiveRecord::ErrorDetail")), args: vec![].into() };
    let mut class = Expr::new(span, ExprNode::Const { path: vec![Symbol::from("ActiveRecord"), Symbol::from("ErrorDetail")] });
    class.ty = Some(error_ty.clone());
    let mut record = Expr::new(
        span,
        ExprNode::Send {
            recv: Some(class),
            method: Symbol::from("new"),
            args: vec![field, typed_str(kind), typed_str(message), full_message, bool_lit(span, symbolic), bool_lit(span, optionless)],
            block: None,
            parenthesized: true,
        },
    );
    record.ty = Some(error_ty);
    let mut push_detail = Expr::new(
        span,
        ExprNode::Send {
            recv: Some(error_details(owner, span)),
            method: Symbol::from("<<"),
            args: vec![record],
            block: None,
            parenthesized: false,
        },
    );
    push_detail.ty = Some(error_details_ty());
    let ty = push_message.ty.clone();
    let mut both = Expr::new(span, ExprNode::Seq { exprs: vec![push_message, push_detail] });
    both.ty = ty;
    both
}

pub(crate) fn error_details_ty() -> crate::ty::Ty {
    crate::ty::Ty::Array {
        elem: std::sync::Arc::new(crate::ty::Ty::Class { id: crate::ident::ClassId(Symbol::from("ActiveRecord::ErrorDetail")), args: vec![].into() }),
    }
}

/// `<owner>.error_details`, typed.
pub(crate) fn error_details(owner: Option<Expr>, span: crate::span::Span) -> Expr {
    let mut e = Expr::new(
        span,
        ExprNode::Send { recv: owner, method: Symbol::from("error_details"), args: Vec::new(), block: None, parenthesized: false },
    );
    e.ty = Some(error_details_ty());
    e
}

fn bool_lit(span: crate::span::Span, value: bool) -> Expr {
    let mut e = Expr::new(span, ExprNode::Lit { value: Literal::Bool { value } });
    e.ty = Some(crate::ty::Ty::Bool);
    e
}

fn typed_str(mut e: Expr) -> Expr {
    if e.ty.is_none() {
        e.ty = Some(crate::ty::Ty::Str);
    }
    e
}

/// A message the push may evaluate twice, once for the full text and once for the record.
fn reads_without_effect_deep(e: &Expr) -> bool {
    match &*e.node {
        ExprNode::Lit { .. } | ExprNode::Var { .. } | ExprNode::Ivar { .. } | ExprNode::SelfRef | ExprNode::Const { .. } => true,
        ExprNode::StringInterp { parts } => parts.iter().all(|p| match p {
            InterpPart::Text { .. } => true,
            InterpPart::Expr { expr } => reads_without_effect_deep(expr),
        }),
        // Not a zero-arg call either: nothing proves it leaves state alone, and it would run twice
        _ => false,
    }
}

/// A class type (or a nilable one), i.e. not the runtime accumulator.
/// `ActiveModel::Errors` is the framework's collection, which the
/// analyzer types `errors` as on AR records and on includers of
/// `ActiveModel::Validations`/`Model`: the shared runtime implements it
/// as that very accumulator, so it grounds like an untyped receiver.
/// `(:type, message: "…")` after the field: a Symbol error type and a
/// sole `message:` option.
fn typed_message_option(args: &[Expr]) -> bool {
    let [_, kind, options] = args else { return false };
    matches!(&*kind.node, ExprNode::Lit { value: Literal::Sym { .. } })
        && matches!(&*options.node, ExprNode::Hash { entries, .. }
            if entries.len() == 1
                && matches!(&*entries[0].0.node, ExprNode::Lit { value: Literal::Sym { value } } if value.as_str() == "message"))
}

fn names_own_class(ty: &Ty) -> bool {
    let app_class = |v: &Ty| matches!(v, Ty::Class { id, .. } if id.0.as_str() != "ActiveModel::Errors");
    match ty {
        Ty::Class { .. } => app_class(ty),
        Ty::Union { variants } => {
            variants.iter().any(app_class)
                && variants.iter().all(|v| app_class(v) || matches!(v, Ty::Nil))
        }
        _ => false,
    }
}

fn str_lit(span: crate::span::Span, value: &str) -> Expr {
    typed(span, ExprNode::Lit { value: Literal::Str { value: value.to_string() } }, Ty::Str)
}

fn typed(span: crate::span::Span, node: ExprNode, ty: Ty) -> Expr {
    let mut e = Expr::new(span, node);
    e.ty = Some(ty);
    e
}

/// The count-free messages of activemodel 8.2.0.alpha's
/// lib/active_model/locale/en.yml and activerecord's
/// lib/active_record/locale/en.yml (`required`, `taken`), by error type.
/// `confirmation` interpolates the attribute and the length/number
/// types a count, so they are not here.
fn default_error_message(error_type: &str) -> Option<&'static str> {
    Some(match error_type {
        "inclusion" => "is not included in the list",
        "exclusion" => "is reserved",
        "invalid" => "is invalid",
        "accepted" => "must be accepted",
        "empty" => "can't be empty",
        "blank" => "can't be blank",
        "present" => "must be blank",
        "password_too_long" => "is too long",
        "not_a_number" => "is not a number",
        "not_an_integer" => "must be an integer",
        "odd" => "must be odd",
        "even" => "must be even",
        "required" => "must exist",
        "taken" => "has already been taken",
        _ => return None,
    })
}

/// `<errors reader>.full_message(:field, message)`: the one shape whose
/// text `rewrite_errors_add` bakes. Shared with analyze, which types it
/// `String` only when this holds.
pub(crate) fn full_message_bakes(recv: Option<&Expr>, args: &[Expr], block: Option<&Expr>) -> bool {
    block.is_none()
        && args.len() == 2
        && matches!(&*args[0].node, ExprNode::Lit { value: Literal::Sym { .. } })
        && !matches!(&*args[1].node, ExprNode::Hash { .. })
        && recv.is_some_and(|r| match &*r.node {
            ExprNode::Send { recv: owner, method, args, .. } => {
                method.as_str() == "errors" && args.is_empty() && owner.as_ref().is_none_or(|o| reads_without_effect(o) || is_record(o))
            }
            // `errors = record.errors`: the collection in a local, of a record no model is known for.
            ExprNode::Var { .. } => {
                matches!(&r.ty, Some(Ty::Class { id, .. }) if id.0.as_str() == "ActiveModel::Errors")
            }
            _ => false,
        })
}

/// A record built for its `errors`, which `&&` evaluates before the text and which is never falsy.
fn is_record(e: &Expr) -> bool {
    matches!(&e.ty, Some(crate::ty::Ty::Class { .. }))
}

/// Folding drops the receiver, so only one whose evaluation does nothing.
fn reads_without_effect(e: &Expr) -> bool {
    matches!(&*e.node, ExprNode::Var { .. } | ExprNode::Ivar { .. } | ExprNode::SelfRef)
}

/// The record an `errors` reader is on, when reading it again is the same record: self, or a plain read.
pub(crate) fn owner_reads_once(owner: Option<&Expr>) -> bool {
    // A bare zero-arg call counts: a view's or partial's local reaches here in that shape
    owner.is_none_or(|o| {
        reads_without_effect(o)
            || matches!(&*o.node, ExprNode::Send { recv: None, args, block: None, .. } if args.is_empty())
    })
}

/// A looked-up message as a string: a literal when every part is known,
/// else an interpolation reading `field` off `owner` (self when `None`)
/// where `%{value}` stood.
pub(crate) fn parts_expr(
    span: crate::span::Span,
    parts: Vec<crate::i18n::Part>,
    owner: Option<&Expr>,
    field: &str,
) -> Expr {
    if let [crate::i18n::Part::Text(text)] = parts.as_slice() {
        return Expr::new(span, ExprNode::Lit { value: Literal::Str { value: text.clone() } });
    }
    let parts = parts
        .into_iter()
        .map(|p| match p {
            crate::i18n::Part::Text(value) => InterpPart::Text { value },
            crate::i18n::Part::Value => InterpPart::Expr {
                expr: Expr::new(
                    span,
                    ExprNode::Send {
                        recv: owner.cloned(),
                        method: Symbol::from(field),
                        args: vec![],
                        block: None,
                        parenthesized: false,
                    },
                ),
            },
        })
        .collect();
    Expr::new(span, ExprNode::StringInterp { parts })
}

/// The full message Rails stores for `field` given a String message:
/// `errors.format` over the attribute's human name, the bare message for
/// `:base`.
fn baked_message(
    i18n: &crate::i18n::ModelI18n,
    span: crate::span::Span,
    field: &str,
    msg: Option<Expr>,
) -> Expr {
    let lit = |value: String| Expr::new(span, ExprNode::Lit { value: Literal::Str { value } });
    let Some(msg) = msg else {
        return parts_expr(span, i18n.full_error(field, "invalid", &Default::default()), None, field);
    };
    if field == "base" {
        return msg;
    }
    if let ExprNode::Lit { value: Literal::Str { value } } = &*msg.node {
        return lit(i18n.full_message(field, value));
    }
    // A dynamic message splices into the format where `%{message}` stands.
    let format = i18n.errors_format().replace("%{attribute}", &i18n.human_attribute_name(field));
    let (before, after) = format.split_once("%{message}").unwrap_or((format.as_str(), ""));
    let mut parts = vec![InterpPart::Text { value: before.to_string() }, InterpPart::Expr { expr: msg }];
    if !after.is_empty() {
        parts.push(InterpPart::Text { value: after.to_string() });
    }
    Expr::new(span, ExprNode::StringInterp { parts })
}
