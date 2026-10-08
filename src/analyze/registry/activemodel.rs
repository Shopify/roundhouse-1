//! `ActiveModel::Validations` / `Model` module surfaces and the
//! `ActiveModel::Errors` collection + individual `ActiveModel::Error`
//! classes. Extracted verbatim from `Analyzer::with_adapter`.

use crate::det::HashMap;

use crate::analyze::ClassInfo;
use crate::ident::{ClassId, Symbol};
use crate::ty::Ty;

pub(in crate::analyze) fn register(classes: &mut HashMap<ClassId, ClassInfo>) {
    // `ActiveModel::Validations` / `ActiveModel::Model` — mixed into
    // plain-Ruby form/query objects (`class Search; include
    // ActiveModel::Validations`). A class that includes them gains the
    // validation surface, resolved via the includer's `includes` and
    // `lookup_in_module`. Registered as module ClassInfos carrying that
    // surface. `ActiveModel::Model` bundles Validations + Conversion +
    // attribute assignment, so it gets the same predicates plus the
    // persistence-shape readers.
    {
        let errors_ty = Ty::Class {
            id: ClassId(Symbol::from("ActiveModel::Errors")),
            args: vec![],
        };
        let mut validations = ClassInfo::default();
        for (m, ty) in [
            ("valid?", Ty::Bool),
            ("invalid?", Ty::Bool),
            ("validate", Ty::Bool),
            ("validate!", Ty::Bool),
            ("errors", errors_ty.clone()),
        ] {
            validations.instance_methods.insert(Symbol::from(m), ty);
        }
        classes
            .entry(ClassId(Symbol::from("ActiveModel::Validations")))
            .or_insert(validations.clone());

        let mut model = validations;
        for (m, ty) in [
            ("persisted?", Ty::Bool),
            ("new_record?", Ty::Bool),
            ("to_model", Ty::Untyped),
            ("model_name", Ty::Untyped),
        ] {
            model.instance_methods.insert(Symbol::from(m), ty);
        }
        classes
            .entry(ClassId(Symbol::from("ActiveModel::Model")))
            .or_insert(model);
    }

    // ActiveModel::Errors — the collection returned by `model.errors`.
    // Supports count/[]/any?/each and flows a Error instance to blocks.
    let error_ty = Ty::Class {
        id: ClassId(Symbol::from("ActiveModel::Error")),
        args: vec![],
    };
    // The collection self-type, returned by the mutating/chaining methods
    // (`<<`/`add`/`clear`) so a validate-method chain stays typed.
    let errors_ty = Ty::Class {
        id: ClassId(Symbol::from("ActiveModel::Errors")),
        args: vec![],
    };
    let str_arr = || Ty::Array { elem: Box::new(Ty::Str) };
    let mut errors_cls = ClassInfo::default();
    for (m, ty) in [
        ("count", Ty::Int),
        ("size", Ty::Int),
        ("any?", Ty::Bool),
        ("none?", Ty::Bool),
        ("empty?", Ty::Bool),
        ("include?", Ty::Bool),
        ("full_messages", str_arr()),
        // `errors[:title]` returns an Array<String> of messages for that attribute.
        ("[]", str_arr()),
        ("messages_for", str_arr()),
        // `details`: each attribute's Errors as `{error: type, **options}`,
        // which the accumulator keeps only where the push recorded them
        // (runtime/spinel/active_model_errors.rb); recorded and refused
        // as `attribute_names` is.
        (
            "details",
            Ty::Hash {
                key: Box::new(Ty::Sym),
                value: Box::new(Ty::Array {
                    elem: Box::new(Ty::Hash { key: Box::new(Ty::Sym), value: Box::new(Ty::Untyped) }),
                }),
            },
        ),
        ("messages", Ty::Hash { key: Box::new(Ty::Sym), value: Box::new(str_arr()) }),
        // `.each` yields an Error — registered via block_params_for below.
        ("each", error_ty.clone()),
        // `errors << "message"` is the transpiled-shape idiom for adding
        // errors from a model's `validate` method. Returns the errors
        // collection (same as Array#<<). `add` is the semantically-
        // equivalent Rails idiom.
        ("<<", errors_ty.clone()),
        ("add", errors_ty.clone()),
        ("clear", errors_ty.clone()),
        // The attributes with an error, as Symbols. The accumulator keeps
        // only messages; the CRuby and JRuby trees record each message's
        // attribute beside it (runtime/spinel/active_model_errors.rb) and
        // the read is refused where that port is absent
        // (`analyze::runtime_reads::GATED_METHODS`).
        ("attribute_names", Ty::Array { elem: Box::new(Ty::Sym) }),
        // `to_hash(full_messages = false)` and `as_json(options = nil)`:
        // the messages (short, or full) grouped by attribute. Recorded
        // and refused as `attribute_names` is; `messages` too, where
        // `lower::errors_index` does not ground the read.
        ("to_hash", Ty::Hash { key: Box::new(Ty::Sym), value: Box::new(str_arr()) }),
        ("as_json", Ty::Hash { key: Box::new(Ty::Sym), value: Box::new(str_arr()) }),
        // `merge!(other)`: another collection's errors, each with its
        // attribute; answers `other`'s Error objects.
        ("merge!", Ty::Array { elem: Box::new(error_ty.clone()) }),
        // The queries by attribute (and type, and options), `delete`
        // (the short messages removed, or nil) and `import(error,
        // override_options)`, answered from the same records.
        ("where", Ty::Array { elem: Box::new(error_ty.clone()) }),
        ("key?", Ty::Bool),
        ("has_key?", Ty::Bool),
        ("exclude?", Ty::Bool),
        ("of_kind?", Ty::Bool),
        ("added?", Ty::Bool),
        ("full_messages_for", str_arr()),
        ("full_message", Ty::Str),
        ("delete", Ty::Union { variants: vec![str_arr(), Ty::Nil] }),
        ("import", Ty::Array { elem: Box::new(error_ty.clone()) }),
    ] {
        errors_cls.instance_methods.insert(Symbol::from(m), ty);
    }
    // Additional collection APIs require shared runtime/lowering support.
    classes.insert(
        ClassId(Symbol::from("ActiveModel::Errors")),
        errors_cls,
    );

    // `record.association(:name)`: the association object
    // (runtime/spinel/active_record_associations.rb, CRuby and JRuby
    // trees). The kinds share one surface here; `target` is gradual
    // because it is a record for a singular association and an Array for
    // a collection.
    {
        let assoc_ty = Ty::Class { id: ClassId(Symbol::from("ActiveRecord::Associations::Association")), args: vec![] };
        let nilable_assoc = Ty::Union { variants: vec![assoc_ty.clone(), Ty::Nil] };
        let mut assoc = ClassInfo::default();
        for (m, ty) in [
            ("owner", Ty::Untyped),
            ("target", Ty::Untyped),
            ("target=", Ty::Untyped),
            ("loaded?", Ty::Bool),
            ("loaded!", Ty::Nil),
            ("reset", Ty::Nil),
            ("reload", nilable_assoc),
            ("reader", Ty::Untyped),
            ("load_target", Ty::Untyped),
            // The target class (nil for a polymorphic belongs_to with
            // no type), the association's Relation, and whether a key
            // write left a loaded target behind.
            ("klass", Ty::Untyped),
            ("scope", Ty::Untyped),
            ("stale_target?", Ty::Bool),
        ] {
            assoc.instance_methods.insert(Symbol::from(m), ty);
        }
        for kind in [
            "Association",
            "SingularAssociation",
            "CollectionAssociation",
            "BelongsToAssociation",
            "BelongsToPolymorphicAssociation",
            "HasOneAssociation",
            "HasManyAssociation",
            "HasManyThroughAssociation",
        ] {
            classes
                .entry(ClassId(Symbol::from(format!("ActiveRecord::Associations::{kind}"))))
                .or_insert(assoc.clone());
        }
    }

    // Individual Error with its Rails API.
    let mut error_cls = ClassInfo::default();
    for (m, ty) in [
        ("full_message", Ty::Str),
        ("message", Ty::Str),
        ("attribute", Ty::Sym),
        ("type", Ty::Sym),
        // The type as added (a Symbol, or the message added as text),
        // its options, and `details` (`{error: raw_type, **options}`):
        // runtime/spinel/active_model_errors.rb's Error answers them
        // where the push recorded them.
        ("raw_type", Ty::Untyped),
        ("options", Ty::Hash { key: Box::new(Ty::Sym), value: Box::new(Ty::Untyped) }),
        ("details", Ty::Hash { key: Box::new(Ty::Sym), value: Box::new(Ty::Untyped) }),
        ("detail", Ty::Hash { key: Box::new(Ty::Sym), value: Box::new(Ty::Untyped) }),
        ("match?", Ty::Bool),
        ("strict_match?", Ty::Bool),
    ] {
        error_cls.instance_methods.insert(Symbol::from(m), ty);
    }
    classes.insert(
        ClassId(Symbol::from("ActiveModel::Error")),
        error_cls,
    );
}
