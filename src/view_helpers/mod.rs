//! The ActionView helper surface an emitted tree answers, in one place.
//!
//! An app class may `include ActionView::Helpers::DateHelper` (or any
//! other `ActionView::Helpers::*`, or `ActiveSupport::NumberHelper`). No
//! target ships that namespace, so the include itself cannot survive into
//! the emitted program: it would be an `uninitialized constant` when the
//! class body runs. What survives instead is each CALL: a bare call to a
//! member listed in [`QUALIFIED`] is rewritten by the Ruby emit to
//! `ActionView::ViewHelpers.<name>` (`emit::ruby::library`'s
//! `rewrite_helper_calls`), and a bare `tag` is expanded at compile time
//! by `lower::tag_builder`. Every other member the include brings is NOT
//! answered: dropped silently, the call would emit bare and raise
//! NoMethodError at run time while `check` reported nothing.
//!
//! So the include may be dropped only when every helper member the class
//! reaches through it is one of those. [`unanswered_members`] is that
//! decision, made by `ingest::view_helper_includes` for library classes
//! and models alike; [`QUALIFIED`] is the list the emit qualifies by.
//! Both read this module, so they cannot disagree about what "ported"
//! means. The helper members themselves are Rails' own, measured from
//! the pinned actionview (`actionview_members.txt`, regenerated and
//! compared by `tests/view_helper_includes.rs`).

use std::collections::BTreeMap;
use std::sync::OnceLock;

/// The `Unsupported` construct an unanswerable helper include is
/// refused under (`ingest::view_helper_includes`), reported for library
/// classes and models alike (`analyze::diagnostics`).
pub(crate) const REFUSAL_CONSTRUCT: &str = "view_helper_include";

/// The framework include markers: `ActionView::Helpers` and every
/// module under it, and `ActiveSupport::NumberHelper` (the same number
/// helpers). Called from both homes that build an include list —
/// `analyze::model_includes` for models, `ingest::library_class`'s decl
/// walk for everything else — because the same source line means the
/// same thing in either.
pub(crate) fn is_marker_include(path: &[&str]) -> bool {
    matches!(path, ["ActionView", "Helpers", ..] | ["ActiveSupport", "NumberHelper"])
}

/// Framework view helpers callable from a helper/model body that the
/// view-template classifier (which runs only on views) never reaches —
/// plus bare calls in view bodies the classifier has no kind for (it
/// handles a fixed set; the rest fall through to this pass). A bare call
/// to one resolves to `ActionView::ViewHelpers.<name>`; each has a
/// `def self.<name>` in the Ruby-family runtime (asserted below). Grown
/// as GET / surfaces each one: asset helpers (`avatar_img` →
/// `image_tag` → `image_path`), then the date helpers + `content_tag`
/// (`time_ago_in_words_label` calls both bare).
pub(crate) const QUALIFIED: &[&str] = &[
    "image_tag",
    "image_path",
    // The general asset helper beside the image one — same
    // body, and campfire asks it for an mp3.
    "asset_path",
    "image_url",
    "path_to_javascript",
    "javascript_path",
    "javascript_include_tag",
    "number_with_precision",
    "number_with_delimiter",
    "number_to_human",
    "number_to_human_size",
    "content_security_policy_nonce",
    "class_names",
    "label_tag",
    "url_for",
    // `polymorphic_url(record, only_path: true)` — beside
    // `url_for`, which is the same question asked of a value
    // the lowerings could answer statically. campfire's
    // `BroadcastsHelper` asks it of an Active Storage
    // representation, so the runtime member it now resolves to
    // is a raising stub; unqualified it was a bare call NOTHING
    // defines, which stops a strict build outright.
    "polymorphic_url",
    "submit_tag",
    // `link_to_if cond, name, url, opts` — `link_to` behind a
    // condition, the name escaped bare otherwise. campfire's
    // link-preview partial links the title only when the
    // preview kept an href (`web_url` drops a non-web one).
    "link_to_if",
    // The bare (builder-less) hidden field, beside its
    // `label_tag`/`submit_tag` siblings in the same overlay
    // file. campfire's quick-boost forms carry the emoji this
    // way, one per reaction on every message.
    "hidden_field_tag",
    "text_area_tag",
    // Rails' array-of-fragments concat. campfire's
    // `current_user_meta_tags` builds the two `<meta>` tags the
    // page's `<head>` needs and joins them with it.
    "safe_join",
    "form_tag",
    // Blockless `form_with` written in a HELPER body. The view
    // walker macro-inlines every `form_with` a TEMPLATE writes;
    // a helper module has no accumulator to splice into and its
    // options are a runtime value, so the call fell through
    // unqualified — a bare name nothing defines. Under CRuby
    // that was `undefined method 'form_with' for module
    // FormsHelper`; under spinel the helper's whole body failed
    // to compile and the LINKER stopped on the caller's
    // reference to the vanished symbol. Same hole and same
    // answer as `polymorphic_url` above.
    "form_with",
    "content_tag",
    "time_ago_in_words",
    "distance_of_time_in_words",
    // Rails' alias of `time_ago_in_words` (date_helper.rb's
    // `alias_method`), the spelling Core's customer models use.
    "distance_of_time_in_words_to_now",
    "raw",
    "link_to",
    // campfire's user page links the address with a bare
    // `mail_to @user.email_address`. The VIEW classifier has no
    // kind for it, so it falls through to this pass like the
    // rest of the flat helpers.
    "mail_to",
    "content_for?",
    "capture",
    "concat",
    // Every campfire message row is built by `MessagesHelper
    // .message_tag`, which opens with `dom_id(message)`. The
    // VIEW classifier has always known `dom_id`; a helper body
    // never reached it, and the bare call raised NoMethodError
    // — inside a `rescue Exception` that hid which call it was.
    "dom_id",
    // `ActionView::Helpers::SanitizeHelper`, included into a
    // MODEL — campfire's `Opengraph::Metadata` calls
    // `sanitize(strip_tags(title))` on its own attributes. Both
    // were registry-known (analyze types them) and had no
    // runtime member to dispatch to until
    // view_helpers_ext.rb grew them; the bare calls emitted
    // bare, and the surviving `include` named a namespace no
    // target ships.
    "sanitize",
    "strip_tags",
    // `h` — Rails' escape alias, and `auto_link` beside it.
    // campfire's `message_presentation` is one expression built
    // from both: `auto_link h(ContentFilters…apply(message.body
    // .body)), html: { target: "_blank" }`. Unqualified, each
    // was a NoMethodError inside that method's own `rescue
    // Exception`, which returns "" — so every message rendered
    // an EMPTY BODY, with no diagnostic anywhere.
    "h",
    "auto_link",
];

/// Helper members a lowering expands before emit, so no bare call to
/// them reaches it: `tag.<element>(…)` / `tag(:br)` (`lower::
/// tag_builder`, total over both shapes — a non-literal attribute hash
/// falls back to `ActionView::ViewHelpers.content_tag`).
pub(crate) const LOWERED: &[&str] = &["tag"];

/// Whether the Ruby emit qualifies a bare call to `name` to
/// `ActionView::ViewHelpers.<name>`.
pub(crate) fn emit_qualifies(name: &str) -> bool {
    QUALIFIED.contains(&name)
}

/// Whether a bare call to helper member `name` is answered in the
/// emitted tree: qualified by the emit or expanded by a lowering.
pub(crate) fn answers_bare_call(name: &str) -> bool {
    emit_qualifies(name) || LOWERED.contains(&name)
}

/// The methods an includer of `module` (`ActionView::Helpers::DateHelper`)
/// reaches on implicit self, per the pinned actionview. `None` for a
/// marker path Rails does not define as a helper module.
pub(crate) fn members(module: &str) -> Option<&'static [&'static str]> {
    static TABLE: OnceLock<BTreeMap<&'static str, Vec<&'static str>>> = OnceLock::new();
    TABLE
        .get_or_init(|| {
            include_str!("actionview_members.txt")
                .lines()
                .filter(|line| !line.starts_with('#'))
                // `Module::Name: a b c` — the separator is the one
                // colon followed by a space (or ending the line).
                .filter_map(|line| line.split_once(": ").or_else(|| line.strip_suffix(':').map(|m| (m, ""))))
                .map(|(module, names)| (module.trim(), names.split_whitespace().collect()))
                .collect()
        })
        .get(module.trim_start_matches("::"))
        .map(Vec::as_slice)
}

/// Of the helper members called through an include of `module`, the ones
/// the emitted tree does not answer, in the order given. `calls` are the
/// implicit-self call names (shadowed ones already removed) and whether
/// each was written with an explicit `self.` receiver, which the emit
/// never qualifies. `Err(())` when `module` is not a helper module Rails
/// defines.
pub(crate) fn unanswered_members<'a>(
    module: &str,
    calls: impl IntoIterator<Item = (&'a str, bool)>,
) -> Result<Vec<&'a str>, ()> {
    let members = members(module).ok_or(())?;
    let mut out: Vec<&'a str> = Vec::new();
    for (name, explicit_self) in calls {
        if members.contains(&name) && (explicit_self || !answers_bare_call(name)) && !out.contains(&name) {
            out.push(name);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every qualified name has a member to dispatch to: a `def self.`
    /// in the Ruby-family runtime's `ActionView::ViewHelpers`. A name
    /// listed without one is exactly the silent NoMethodError this list
    /// exists to prevent, moved one hop later.
    #[test]
    fn every_qualified_helper_has_a_runtime_member() {
        let root = concat!(env!("CARGO_MANIFEST_DIR"), "/runtime");
        let mut sources = String::new();
        for dir in ["ruby/action_view", "spinel/scaffold/ruby_overlay/runtime"] {
            for entry in std::fs::read_dir(format!("{root}/{dir}")).expect("runtime dir") {
                let path = entry.expect("entry").path();
                if path.extension().is_some_and(|e| e == "rb") {
                    sources.push_str(&std::fs::read_to_string(&path).expect("runtime file"));
                }
            }
        }
        let missing: Vec<&str> = QUALIFIED
            .iter()
            .copied()
            .filter(|name| {
                let def = format!("def self.{name}");
                !sources.match_indices(&def).any(|(at, _)| {
                    !sources[at + def.len()..].starts_with(|c: char| c.is_alphanumeric() || c == '_')
                })
            })
            .collect();
        assert!(missing.is_empty(), "qualified to ActionView::ViewHelpers but never defined there: {missing:?}");
    }

    #[test]
    fn date_helper_members_split_into_answered_and_not() {
        assert!(members("ActionView::Helpers::DateHelper").is_some_and(|m| m.contains(&"time_ago_in_words")));
        assert_eq!(members("ActionView::Helpers::NoSuchHelper"), None);
        let calls = [("time_ago_in_words", false), ("time_tag", false), ("puts", false), ("distance_of_time_in_words", true)];
        assert_eq!(
            unanswered_members("ActionView::Helpers::DateHelper", calls),
            Ok(vec!["time_tag", "distance_of_time_in_words"])
        );
    }
}
