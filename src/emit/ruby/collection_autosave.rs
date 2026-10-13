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
//! The save is entered through Rails' own name. Rails defines a public
//! `autosave_associated_records_for_<name>(*args)` per association
//! (`define_non_cyclic_method`, with its `@_already_called` loop guard)
//! and registers that name as the after_create / after_update callback,
//! so an app that wraps it (`alias_method` then `define_method`, as
//! Core's `Podding::Database::DeferCallback#defer_autosave_to_after_transaction`
//! does at class load) changes what the save runs. Emitted the same way:
//! the hooks call `autosave_associated_records_for_<name>`, defined
//! unless the model defines it, and it reads whether the owner was new
//! from `@new_record_before_save`, which the save sets around the write
//! as Rails' `around_save_collection_association` does (a nested save of
//! the same record sees false; a call after the save, such as a deferred
//! one after commit, sees the restored value). A has_one gets the same
//! entry point (`save_has_one_association`): its held record is saved
//! when it is new or its key is not the owner's (`_record_changed?`);
//! with `autosave: true` the entry point is the save its after_save
//! already runs (`_autosave_<name>`).
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

/// One has_one whose held record the owner's save saves
/// (`save_has_one_association`).
struct Single {
    span: Span,
    name: Symbol,
    foreign_key: Symbol,
    inverse: Option<Symbol>,
    /// `autosave: true`: the lowering's `_autosave_<name>` in after_save
    /// is the save (`model_to_library::associations`).
    autosave: bool,
}

/// Rails' entry point for an association's save callback.
fn entry_point(name: &Symbol) -> String {
    format!("autosave_associated_records_for_{}", name.as_str())
}

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

pub(crate) fn apply(
    lcs: &mut [LibraryClass],
    app: &App,
    subclass_views: &crate::det::HashMap<crate::ident::ClassId, crate::dialect::Model>,
) {
    if !super::expr::native_keywords() {
        return;
    }
    for lc in lcs.iter_mut() {
        accept_held_required_targets(lc, app, subclass_views);
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
        let singles: Vec<Single> = model
            .spanned_associations()
            .filter_map(|(span, assoc)| {
                let Association::HasOne { name, target, foreign_key, foreign_key_explicit, as_interface, scope, autosave, .. } =
                    assoc
                else {
                    return None;
                };
                let inverse = if *foreign_key_explicit || scope.is_some() || as_interface.is_some() {
                    None
                } else {
                    inverse_of(app, model, target, foreign_key)
                };
                let saved_by_lowering = *autosave && defines(lc, &format!("_autosave_{}", name.as_str()));
                (memoizes(lc, name) && (saved_by_lowering || !*autosave)).then(|| Single {
                    span,
                    name: name.clone(),
                    foreign_key: foreign_key.clone(),
                    inverse,
                    autosave: *autosave,
                })
            })
            .collect();
        if collections.is_empty() && singles.is_empty() {
            continue;
        }
        let owner_type = base_class_name(app, model);
        let mut src = String::new();
        for c in &collections {
            if c.validate {
                src.push_str(&validate_records_source(c));
            }
            src.push_str(&save_records_source(c));
            if !defines(lc, &entry_point(&c.name)) {
                let inner = format!("_autosave_associated_records_for_{}(@new_record_before_save == true)", c.name.as_str());
                src.push_str(&entry_point_source(&c.name, &inner));
            }
        }
        for s in &singles {
            let inner = if s.autosave {
                format!("_autosave_{}", s.name.as_str())
            } else {
                src.push_str(&save_has_one_source(s));
                format!("_save_has_one_association_{}", s.name.as_str())
            };
            if !defines(lc, &entry_point(&s.name)) {
                src.push_str(&entry_point_source(&s.name, &inner));
            }
        }
        let overrides_save = defines(lc, "save_after_validation");
        if !collections.is_empty() {
            src.push_str(&insert_record_source(&collections, &owner_type));
            src.push_str(AROUND_SAVE);
            if !overrides_save {
                src.push_str(&save_after_validation_source(&collections));
            }
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
            // `parse_methods` reads `*args` as a plain positional; the
            // entry point takes any arguments, as Rails' does.
            if m.name.as_str().starts_with("autosave_associated_records_for_") {
                m.params = vec![crate::dialect::Param::rest(Symbol::from("args"))];
            }
            for prefix in ["_validate_associated_records_for_", "_autosave_associated_records_for_"] {
                if let Some(name) = m.name.as_str().strip_prefix(prefix) {
                    let name = Symbol::from(name);
                    crate::lower::model_to_library::validations::tag_error_attribute(&mut m.body, &name);
                }
            }
            m.enclosing_class = Some(lc.name.0.clone());
            lc.methods.push(m);
        }
        // The model's own `save_after_validation` keeps its body, inside
        // the same state the generated one sets.
        if overrides_save && !collections.is_empty() {
            around_save_collection_association(lc);
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
        // Rails' `after_create save_method` / `after_update save_method`,
        // by name, in declaration order, ahead of the callbacks the
        // model's body adds after them.
        let mut saved: Vec<(Span, &Symbol)> = collections.iter().map(|c| (c.span, &c.name)).collect();
        saved.extend(singles.iter().filter(|s| !s.autosave).map(|s| (s.span, &s.name)));
        let order: Vec<&Symbol> = model
            .spanned_associations()
            .filter_map(|(span, assoc)| saved.iter().find(|(s, n)| *s == span && *n == assoc.name()).map(|(_, n)| *n))
            .collect();
        for hook_name in ["after_create", "after_update"] {
            let at = hook(lc, hook_name);
            let body = &mut lc.methods[at].body;
            let mut stmts = statements(body);
            for (i, name) in order.iter().enumerate() {
                stmts.insert(i, call(&entry_point(name), None));
            }
            *body = Expr::new(body.span, ExprNode::Seq { exprs: stmts });
        }
        // An `autosave: true` has_one keeps its save where the lowering
        // folded it (after_save), entered by Rails' name.
        for s in singles.iter().filter(|s| s.autosave) {
            let lowered = format!("_autosave_{}", s.name.as_str());
            let entry = entry_point(&s.name);
            for m in lc.methods.iter_mut().filter(|m| {
                m.name.as_str() == "after_save" && m.receiver == MethodReceiver::Instance && m.params.is_empty()
            }) {
                rename_bare_calls(&mut m.body, &lowered, &entry);
            }
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
/// It answers the records it went through (Rails' `records.each`), nil
/// when the association was never loaded.
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
  records
end
"#
    )
}

/// The owner's save runs in a transaction while an association that
/// saves its records holds some (`Association.saving`): a failed record
/// save rolls the owner's INSERT or UPDATE back and `save` answers
/// false. With none loaded nothing but the owner is written, so the
/// save goes as before. Either way inside Rails'
/// `around_save_collection_association` (`AROUND_SAVE`).
fn save_after_validation_source(collections: &[Collection]) -> String {
    let loaded: Vec<String> = collections.iter().map(|c| format!("@{}_loaded == true", c.name.as_str())).collect();
    format!(
        "def save_after_validation\n  _around_save_collection_association do\n    if {}\n      \
         ActiveRecord::Associations::Association.saving(self) {{ super() }}\n    else\n      super()\n    end\n  end\nend\n",
        loaded.join(" || ")
    )
}

/// Rails' `around_save_collection_association`: whether the owner is
/// new for this save, which the collections' entry points read (a nested
/// save of the same record is not), restored once it is done.
const AROUND_SAVE: &str = r#"def _around_save_collection_association
  previously_new_record_before_save = (@new_record_before_save ||= false)
  @new_record_before_save = !previously_new_record_before_save && new_record?
  yield
ensure
  @new_record_before_save = previously_new_record_before_save
end
"#;

/// The model's own `save_after_validation`, its body run inside
/// `_around_save_collection_association` as the generated one is.
fn around_save_collection_association(lc: &mut LibraryClass) {
    const PLACEHOLDER: &str = "__roundhouse_save_after_validation_body";
    let Some(m) = lc.methods.iter_mut().find(|m| {
        m.name.as_str() == "save_after_validation" && m.receiver == MethodReceiver::Instance && m.params.is_empty()
    }) else {
        return;
    };
    let template = format!("def save_after_validation\n  _around_save_collection_association do\n    {PLACEHOLDER}\n  end\nend\n");
    let mut wrapper = crate::runtime_src::parse_methods(&template)
        .ok()
        .and_then(|mut ms| ms.pop())
        .map(|m| m.body)
        .expect("generated save wrapper parses");
    let mut body = Some(m.body.clone());
    replace_bare_call(&mut wrapper, PLACEHOLDER, &mut body);
    if body.is_none() {
        wrapper.span = m.body.span;
        m.body = wrapper;
    }
}

/// Replaces the first receiverless, argumentless call to `name` in
/// `expr` with `with` (taken).
fn replace_bare_call(expr: &mut Expr, name: &str, with: &mut Option<Expr>) {
    if with.is_none() {
        return;
    }
    if matches!(&*expr.node, ExprNode::Send { recv: None, method, args, block: None, .. }
        if method.as_str() == name && args.is_empty())
    {
        *expr = with.take().expect("checked above");
        return;
    }
    expr.node.for_each_child_mut(&mut |c| replace_bare_call(c, name, with));
}

/// Renames each receiverless, argumentless call to `from` in `expr`.
fn rename_bare_calls(expr: &mut Expr, from: &str, to: &str) {
    expr.node.for_each_child_mut(&mut |c| rename_bare_calls(c, from, to));
    if let ExprNode::Send { recv: None, method, args, block: None, .. } = &mut *expr.node
        && method.as_str() == from
        && args.is_empty()
    {
        *method = Symbol::from(to);
    }
}

/// Whether the model defines the no-argument instance method `name`.
fn defines(lc: &LibraryClass, name: &str) -> bool {
    lc.methods.iter().any(|m| m.name.as_str() == name && m.receiver == MethodReceiver::Instance)
}

/// Rails' `define_non_cyclic_method`: the public entry point
/// `autosave_associated_records_for_<name>(*args)`, which runs `inner`
/// unless a call of it is already running on this record.
fn entry_point_source(name: &Symbol, inner: &str) -> String {
    let m = entry_point(name);
    format!(
        r#"def {m}(*args)
  result = true
  @_already_called ||= {{}}
  unless @_already_called[:{m}]
    begin
      @_already_called[:{m}] = true
      result = {inner}
    ensure
      @_already_called[:{m}] = false
    end
  end
  result
end
"#
    )
}

/// Rails' `save_has_one_association` for a has_one without `autosave:`
/// over the record the reader holds: saved (validated) when it is new,
/// its key is not the owner's or a change of its key is pending
/// (`_record_changed?`), after the owner's key and the inverse are set
/// on it. It answers whether it saved, nil when nothing was to save.
fn save_has_one_source(s: &Single) -> String {
    let n = s.name.as_str();
    let fk = s.foreign_key.as_str();
    let inverse = match &s.inverse {
        Some(inverse) => format!("  record.{} = self\n", inverse.as_str()),
        None => String::new(),
    };
    format!(
        r#"def _save_has_one_association_{n}
  return nil unless @{n}_loaded == true
  record = @{n}_cache
  return nil if record.nil? || record.destroyed?
  key = id
  return nil unless record.new_record? || record.{fk} != key || record.attribute_changed?({fk:?})
  record.{fk} = key unless record.{fk} == key
{inverse}  record.save
end
"#
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
fn accept_held_required_targets(
    lc: &mut LibraryClass,
    app: &App,
    subclass_views: &crate::det::HashMap<crate::ident::ClassId, crate::dialect::Model>,
) {
    let Some(model) = app.models.iter().find(|m| m.name == lc.name).or_else(|| subclass_views.get(&lc.name)) else {
        return;
    };
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

/// `if <cond>; errors << "<message>"; end`, the push possibly followed
/// by the `error_details` push of a tree that keeps them.
fn pushes_message(stmt: &Expr, message: &str) -> bool {
    let ExprNode::If { then_branch, .. } = &*stmt.node else { return false };
    let push = match &*then_branch.node {
        ExprNode::Seq { exprs } if exprs.len() == 2 => &exprs[0],
        _ => then_branch,
    };
    let ExprNode::Send { method, args, .. } = &*push.node else { return false };
    method.as_str() == "<<"
        && matches!(args.as_slice(), [m] if matches!(&*m.node, ExprNode::Lit { value: Literal::Str { value } } if value == message))
}
