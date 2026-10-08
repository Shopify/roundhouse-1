//! Ruby-family pre-emit pass: a has_many validates and saves the records
//! it holds with its owner, as Rails' AutosaveAssociation does for every
//! collection association (activerecord autosave_association.rb).
//!
//! At the `has_many`, Rails declares
//! `validate :validate_associated_records_for_<name>` (unless
//! `validate: false`) and `after_create` / `after_update
//! :autosave_associated_records_for_<name>`:
//!
//! ```ruby
//! # associated_records_to_validate_or_save(association, new_record, autosave)
//! records = if new_record || custom_validation_context?
//!             association.target
//!           else
//!             association.target.find_all(&:new_record?)
//!           end
//! # validate_collection_association / association_valid?
//! records.each do |record|
//!   next if record.destroyed?
//!   context = validation_context if custom_validation_context?
//!   errors.add(:lines) unless record.valid?(context)
//! end
//! # save_collection_association (new_record: new before this save)
//! records.each do |record|
//!   next if record.destroyed?
//!   association.set_inverse_instance(record)
//!   saved = true
//!   association_saved = association.insert_record(record)  # owner key, then save
//!   if reflection.validate?
//!     errors.add(:lines) unless association_saved
//!     saved = association_saved
//!   end
//!   raise RecordInvalid.new(owner) unless saved
//! end
//! ```
//!
//! each only once the association object exists (it was read, built or
//! assigned), plus `after_validation :_ensure_no_duplicate_errors`
//! (`errors.uniq!`). So an unsaved order with an invalid built line is
//! invalid ("Lines is invalid", once for any number of invalid lines), a
//! saved one only for an invalid NEW line, `valid?(:audit)` validates
//! every loaded line in `:audit`, and saving the order saves the lines
//! it holds under its key.
//!
//! Emitted, the association's target is the memoized reader's cache
//! (`@<name>_cache` once `@<name>_loaded`, see
//! `library::apply_collection_memoization`); a reader that keeps none (a
//! `:through` one, one answering a Relation) is not one this pass
//! handles. The validation call goes into `validate` where the
//! `has_many` sits among the model's own `validates` (each check carries
//! its declaration's span); `_ensure_no_duplicate_errors` closes it,
//! since the runtime's `valid?` runs no after_validation. The save calls
//! open `after_create` / `after_update`: the has_many is declared before
//! the callbacks that the model's body adds.
//!
//! Rails' errors compare by attribute, type and options: the
//! association's own `:invalid` and a hand-written `errors.add(:lines,
//! :invalid)` are one error, a `validates_associated` one (which
//! carries `value:`) another (see `DEDUPE`).
//!
//! The inverse (`set_inverse_instance`, Rails' automatic `inverse_of`:
//! the target's non-polymorphic `belongs_to` named for the owner, on the
//! same key, with no `foreign_key:`, `as:` or scope on the has_many) is
//! set when a record is built through the association
//! (`Association.created` calls `_set_inverse_instance`) and before it
//! is saved, so a built line's required `belongs_to :order` holds the
//! unsaved order. The required check accepts a target the association
//! holds, as Rails' presence check of the association does, rather than
//! asking the database for the key (`accept_held_required_targets`).
//!
//! A record whose save fails gives the owner the association's error
//! and RecordInvalid (with `validate: false` it is passed over, as in
//! Rails); the owner's save runs in a transaction while the association
//! holds records (`save_after_validation` through `Association.saving`,
//! runtime/spinel/active_record_associations.rb), so `save` answers
//! false and `save!` raises with the owner's INSERT or UPDATE rolled
//! back and the records' persisted state and ids given back, as Rails'
//! `with_transaction_returning_status` and
//! `restore_transaction_record_state` do. `_insert_record` is also what
//! `owner.<name> << record` saves through on a saved owner
//! (`Association.concat`).
//!
//! Differences: the after-commit callbacks run before the COMMIT rather
//! than after it, and a record's attribute changes are not rewound on
//! rollback (Rails' are re-marked dirty). `autosave: true`
//! (every changed loaded record saved too, errors nested as
//! `lines.sku`) is a check error (`analyze::ingest_coverage`); emitted
//! under `--allow-unsupported` it acts as a plain has_many. CRuby and
//! JRuby only: the other trees keep no collection cache.

use crate::app::App;
use crate::dialect::{Association, LibraryClass, MethodReceiver, Model};
use crate::expr::{Expr, ExprNode, LValue, Literal};
use crate::ident::{ClassId, Symbol};
use crate::span::Span;

/// One has_many this pass validates and saves.
struct Collection {
    span: Span,
    name: Symbol,
    foreign_key: Symbol,
    /// `as:` — the interface whose `<as>_type` names the owner's class.
    as_interface: Option<Symbol>,
    validate: bool,
    /// The target's `belongs_to` back to the owner.
    inverse: Option<Symbol>,
}

pub(crate) fn apply(lcs: &mut [LibraryClass], app: &App) {
    if !super::expr::native_keywords() {
        return;
    }
    for lc in lcs.iter_mut() {
        accept_held_required_targets(lc, app);
    }
    for lc in lcs.iter_mut() {
        let Some(model) = app.models.iter().find(|m| m.name == lc.name) else { continue };
        let collections: Vec<Collection> = model
            .spanned_associations()
            .filter_map(|(span, assoc)| {
                let Association::HasMany {
                    name,
                    target,
                    foreign_key,
                    foreign_key_explicit,
                    through: None,
                    as_interface,
                    scope,
                    skip_validation,
                    ..
                } = assoc
                else {
                    return None;
                };
                let inverse = if *foreign_key_explicit || scope.is_some() || as_interface.is_some() {
                    None
                } else {
                    inverse_of(app, model, target, foreign_key)
                };
                memoizes(lc, name).then(|| Collection {
                    span,
                    name: name.clone(),
                    foreign_key: foreign_key.clone(),
                    as_interface: as_interface.clone(),
                    validate: !skip_validation,
                    inverse,
                })
            })
            .collect();
        if collections.is_empty() {
            continue;
        }
        let owner_type = base_class_name(app, model);
        let mut src = String::new();
        for c in &collections {
            if c.validate {
                src.push_str(&validate_records_source(c));
            }
            src.push_str(&save_records_source(c));
        }
        src.push_str(&insert_record_source(&collections, &owner_type));
        let overrides_save = lc.methods.iter().any(|m| {
            m.name.as_str() == "save_after_validation" && m.receiver == MethodReceiver::Instance && m.params.is_empty()
        });
        if !overrides_save {
            src.push_str(&save_after_validation_source(&collections));
        }
        let inverses: Vec<&Collection> = collections.iter().filter(|c| c.inverse.is_some()).collect();
        if !inverses.is_empty() {
            src.push_str("def _set_inverse_instance(name, record)\n  case name\n");
            for c in &inverses {
                let inverse = c.inverse.as_ref().map(|s| s.as_str()).unwrap_or_default();
                src.push_str(&format!("  when :{} then record.{inverse} = self\n", c.name.as_str()));
            }
            src.push_str("  end\n  nil\nend\n");
        }
        let validates = collections.iter().any(|c| c.validate);
        if validates {
            src.push_str(DEDUPE);
        }
        let methods = crate::runtime_src::parse_methods(&src)
            .unwrap_or_else(|e| panic!("collection_autosave: generated source failed to parse: {e}\n{src}"));
        for mut m in methods {
            for prefix in ["_validate_associated_records_for_", "_autosave_associated_records_for_"] {
                if let Some(name) = m.name.as_str().strip_prefix(prefix) {
                    let name = Symbol::from(name);
                    crate::lower::model_to_library::validations::tag_error_attribute(&mut m.body, &name);
                }
            }
            m.enclosing_class = Some(lc.name.0.clone());
            lc.methods.push(m);
        }

        if validates {
            let messages: Vec<String> =
                collections.iter().filter(|c| c.validate).map(|c| invalid_message(&c.name)).collect();
            for m in lc.methods.iter_mut().filter(|m| m.receiver == MethodReceiver::Instance) {
                list_invalid_type_errors(&mut m.body, &messages);
            }
            let at = hook(lc, "validate");
            let body = &mut lc.methods[at].body;
            let mut stmts = statements(body);
            for c in collections.iter().filter(|c| c.validate) {
                // Before the first of the model's own checks declared
                // after the has_many, in its file.
                let at = stmts
                    .iter()
                    .position(|s| !s.span.is_synthetic() && s.span.file == c.span.file && s.span.start > c.span.start)
                    .unwrap_or(stmts.len());
                stmts.insert(at, call(&format!("_validate_associated_records_for_{}", c.name.as_str()), None));
            }
            stmts.push(call("_ensure_no_duplicate_errors", None));
            *body = Expr::new(body.span, ExprNode::Seq { exprs: stmts });
        }
        for (hook_name, was_new) in [("after_create", true), ("after_update", false)] {
            let at = hook(lc, hook_name);
            let body = &mut lc.methods[at].body;
            let mut stmts = statements(body);
            for (i, c) in collections.iter().enumerate() {
                let flag = Expr::new(Span::synthetic(), ExprNode::Lit { value: Literal::Bool { value: was_new } });
                stmts.insert(i, call(&format!("_autosave_associated_records_for_{}", c.name.as_str()), Some(flag)));
            }
            *body = Expr::new(body.span, ExprNode::Seq { exprs: stmts });
        }
    }
}

/// `errors << "Lines is invalid"` grounded from `errors.add(:lines)` /
/// `errors.add(:lines, :invalid)` on the record's own errors becomes
/// `errors << _association_error("Lines is invalid")`, so the dedupe
/// keeps it once with the association's error (`DEDUPE`).
fn list_invalid_type_errors(expr: &mut Expr, messages: &[String]) {
    expr.node.for_each_child_mut(&mut |c| list_invalid_type_errors(c, messages));
    let ExprNode::Send { recv: Some(recv), method, args, .. } = &mut *expr.node else { return };
    let own_errors = matches!(&*recv.node, ExprNode::Send { recv: owner, method: errors, args: none, .. }
        if errors.as_str() == "errors"
            && none.is_empty()
            && owner.as_ref().is_none_or(|r| matches!(&*r.node, ExprNode::SelfRef)));
    if method.as_str() != "<<" || !own_errors {
        return;
    }
    let [message] = args.as_mut_slice() else { return };
    let listed = message.hint == Some(crate::expr::IrHint::InvalidErrorType)
        && matches!(&*message.node, ExprNode::Lit { value: Literal::Str { value } } if messages.contains(value));
    if listed {
        let text = message.clone();
        *message = call("_association_error", Some(text));
    }
}

fn call(method: &str, arg: Option<Expr>) -> Expr {
    let parenthesized = arg.is_some();
    Expr::new(
        Span::synthetic(),
        ExprNode::Send { recv: None, method: Symbol::from(method), args: arg.into_iter().collect(), block: None, parenthesized },
    )
}

fn statements(body: &Expr) -> Vec<Expr> {
    match &*body.node {
        ExprNode::Seq { exprs } => exprs.clone(),
        _ => vec![body.clone()],
    }
}

/// The index of the model's no-argument instance `name`, made
/// (`super()`, which reaches the parent's or Base's) when it has none.
fn hook(lc: &mut LibraryClass, name: &str) -> usize {
    if let Some(at) = lc
        .methods
        .iter()
        .position(|m| m.name.as_str() == name && m.receiver == MethodReceiver::Instance && m.params.is_empty())
    {
        return at;
    }
    let src = format!("def {name}\n  super()\nend\n");
    let mut m = crate::runtime_src::parse_methods(&src).expect("hook parses").pop().expect("one method");
    m.enclosing_class = Some(lc.name.0.clone());
    lc.methods.push(m);
    lc.methods.len() - 1
}

/// The reader keeps its records in `@<name>_cache` behind
/// `@<name>_loaded` (`library::memoize_query_reader`).
fn memoizes(lc: &LibraryClass, name: &Symbol) -> bool {
    let loaded = format!("{}_loaded", name.as_str());
    lc.methods.iter().any(|m| {
        &m.name == name
            && m.receiver == MethodReceiver::Instance
            && matches!(&*m.body.node, ExprNode::Seq { exprs } if exprs.iter().any(|e| matches!(&*e.node,
                ExprNode::Assign { target: LValue::Ivar { name }, .. } if name.as_str() == loaded)))
    })
}

/// Rails' automatic `inverse_of` for `owner.has_many :x` on `target`
/// (reflection.rb `automatic_inverse_of`): the target's `belongs_to`
/// named for the owner class, not polymorphic, on the same key.
fn inverse_of(app: &App, owner: &Model, target: &ClassId, foreign_key: &Symbol) -> Option<Symbol> {
    let inverse = crate::naming::underscore(crate::naming::demodulize(owner.name.0.as_str()));
    let target = app.models.iter().find(|m| &m.name == target)?;
    target.associations().find_map(|a| match a {
        Association::BelongsTo { name, foreign_key: fk, polymorphic: false, .. }
            if name.as_str() == inverse && fk == foreign_key =>
        {
            Some(name.clone())
        }
        _ => None,
    })
}

/// The STI base's name (Rails' `polymorphic_name`, `base_class.name`).
fn base_class_name(app: &App, model: &Model) -> String {
    let mut current = model;
    let mut seen = vec![current.name.clone()];
    while let Some(parent) = current.parent.as_ref() {
        let Some(p) = app.models.iter().find(|m| &m.name == parent) else { break };
        if crate::lower::model_to_library::markers::is_abstract_class(p) || seen.contains(&p.name) {
            break;
        }
        seen.push(p.name.clone());
        current = p;
    }
    current.name.0.as_str().to_string()
}

fn invalid_message(name: &Symbol) -> String {
    format!("{} is invalid", crate::lower::model_to_library::validations::humanize(name.as_str()))
}

fn validate_records_source(c: &Collection) -> String {
    let n = c.name.as_str();
    let message = invalid_message(&c.name);
    // A record pushed onto the Array (`order.lines << line`) did not go
    // through `Association.created`; Rails' `<<` sets its inverse.
    let inverse = match &c.inverse {
        Some(inverse) => {
            let i = inverse.as_str();
            format!(
                "  records.each {{ |record| record.{i} = self unless record.instance_variable_get(:@{i}_loaded) == true }}\n"
            )
        }
        None => String::new(),
    };
    format!(
        r#"def _validate_associated_records_for_{n}
  return nil unless @{n}_loaded == true
  context = validation_context
  custom = !(context.nil? || context == :create || context == :update)
  records = @{n}_cache
  records = records.select {{ |record| record.new_record? }} unless new_record? || custom
{inverse}  context = nil unless custom
  valid = records.map {{ |record| record.destroyed? || record.valid?(context) }}
  if valid.include?(false)
    errors << _association_error({message:?})
  end
  nil
end
"#
    )
}

/// `_insert_record(name, record)`: Rails' HasManyAssociation
/// `insert_record` — the owner's key (and `as:` type) and the inverse
/// set on the record, then its save, which answers whether it saved.
/// The record joins the open transaction's records first
/// (`Association.enroll`), so a rollback gives it back its state.
fn insert_record_source(collections: &[Collection], owner_type: &str) -> String {
    let mut src = String::from("def _insert_record(name, record)\n  case name\n");
    for c in collections {
        src.push_str(&format!("  when :{}\n", c.name.as_str()));
        if let Some(inverse) = &c.inverse {
            src.push_str(&format!("    record.{} = self\n", inverse.as_str()));
        }
        src.push_str(&format!("    record.{} = id\n", c.foreign_key.as_str()));
        if let Some(interface) = &c.as_interface {
            src.push_str(&format!("    record.{}_type = {owner_type:?}\n", interface.as_str()));
        }
    }
    src.push_str(
        "  else\n    raise ArgumentError, \"no has_many #{name} that saves its records\"\n  end\n  \
         ActiveRecord::Associations::Association.enroll(record)\n  record.save\nend\n",
    );
    src
}

/// Rails' `save_collection_association`: with `validate: false` a
/// record that does not save is passed over (`saved` stays true); with
/// it the owner gains the association's error and RecordInvalid, which
/// the owner's `save` answers as false (`save_after_validation_source`).
fn save_records_source(c: &Collection) -> String {
    let n = c.name.as_str();
    let save = if c.validate {
        format!(
            "    unless _insert_record(:{n}, record)\n      errors << {:?}\n      raise ActiveRecord::RecordInvalid.new(self)\n    end\n",
            invalid_message(&c.name)
        )
    } else {
        format!("    _insert_record(:{n}, record)\n")
    };
    format!(
        r#"def _autosave_associated_records_for_{n}(was_new)
  return nil unless @{n}_loaded == true
  records = @{n}_cache
  records = records.select {{ |record| record.new_record? }} unless was_new
  records.each do |record|
    next if record.destroyed?
{save}  end
  nil
end
"#
    )
}

/// The owner's save runs in a transaction while an association that
/// saves its records holds some (`Association.saving`): a failed record
/// save rolls the owner's INSERT or UPDATE back and `save` answers
/// false. With none loaded nothing but the owner is written, so the
/// save goes as before.
fn save_after_validation_source(collections: &[Collection]) -> String {
    let loaded: Vec<String> = collections.iter().map(|c| format!("@{}_loaded == true", c.name.as_str())).collect();
    format!(
        "def save_after_validation\n  return super() unless {}\n  \
         ActiveRecord::Associations::Association.saving(self) {{ super() }}\nend\n",
        loaded.join(" || ")
    )
}

/// Rails' errors compare by attribute, type and options
/// (`errors.uniq!`). The accumulator keeps messages, so the errors of
/// the default `:invalid` type without options (the association's own,
/// and a hand-written `errors.add(:lines)` / `errors.add(:lines,
/// :invalid)`, `IrHint::InvalidErrorType`) are fresh Strings
/// `_association_error` lists: those are kept once among themselves,
/// every other message once among the others, so a `validates_associated`
/// one (which carries `value:`) or `errors.add(:lines, "is invalid")`
/// stays beside the association's.
const DEDUPE: &str = r#"def _association_error(message)
  message = String.new(message)
  @__autosave_errors = [] if @__autosave_errors.nil?
  @__autosave_errors << message
  message
end

def _ensure_no_duplicate_errors
  own = @__autosave_errors || []
  @__autosave_errors = nil
  seen = []
  seen_own = []
  errors.select! do |message|
    if own.any? { |error| error.equal?(message) }
      if seen_own.include?(message)
        false
      else
        seen_own << message
        true
      end
    elsif seen.include?(message)
      false
    else
      seen << message
      true
    end
  end
  nil
end
"#;

/// A required `belongs_to`'s check (`validations::inline_belongs_to_check`:
/// "Order must exist" unless the key names a row) passes for a target
/// the association holds: Rails checks the presence of the association,
/// so an unsaved order assigned, or set as the inverse, satisfies it
/// before it has a key. The check is the `validate` statement carrying
/// the `belongs_to`'s span; it becomes
/// `if @order_loaded != true || @order_cache.nil?; <check>; end`.
fn accept_held_required_targets(lc: &mut LibraryClass, app: &App) {
    let Some(model) = app.models.iter().find(|m| m.name == lc.name) else { return };
    let required: Vec<(Span, Symbol)> = model
        .spanned_associations()
        .filter_map(|(span, a)| match a {
            Association::BelongsTo { name, optional: false, polymorphic: false, .. } => Some((span, name.clone())),
            _ => None,
        })
        .collect();
    if required.is_empty() {
        return;
    }
    let Some(validate) = lc
        .methods
        .iter_mut()
        .find(|m| m.name.as_str() == "validate" && m.receiver == MethodReceiver::Instance && m.params.is_empty())
    else {
        return;
    };
    let ExprNode::Seq { exprs } = &mut *validate.body.node else { return };
    for (span, name) in required {
        let message = format!("{} must exist", crate::lower::model_to_library::validations::humanize(name.as_str()));
        let Some(stmt) = exprs.iter_mut().find(|e| e.span == span && pushes_message(e, &message)) else {
            continue;
        };
        let n = name.as_str();
        let not_held = crate::runtime_src::parse_methods(&format!(
            "def __not_held\n  @{n}_loaded != true || @{n}_cache.nil?\nend\n"
        ))
        .ok()
        .and_then(|mut ms| ms.pop())
        .map(|m| m.body)
        .expect("generated guard parses");
        let check = stmt.clone();
        *stmt = Expr::new(
            check.span,
            ExprNode::If {
                cond: not_held,
                then_branch: check,
                else_branch: Expr::new(Span::synthetic(), ExprNode::Lit { value: Literal::Nil }),
            },
        );
    }
}

/// `if <cond>; errors << "<message>"; end`.
fn pushes_message(stmt: &Expr, message: &str) -> bool {
    let ExprNode::If { then_branch, .. } = &*stmt.node else { return false };
    let ExprNode::Send { method, args, .. } = &*then_branch.node else { return false };
    method.as_str() == "<<"
        && matches!(args.as_slice(), [m] if matches!(&*m.node, ExprNode::Lit { value: Literal::Str { value } } if value == message))
}
