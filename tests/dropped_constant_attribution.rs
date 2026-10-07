//! A class that the app's own sources define but survey-mode ingest
//! dropped surfaces at every reference as a `constant` refusal. That
//! refusal is the gap's shadow: a coverage note (as in
//! gap_cascade_attribution, upstream's policy) that names the
//! definition's file and the gap that failed the file's class ingest,
//! while the gap itself stays an error. A gap that ingest absorbed,
//! leaving the class in the IR, is never named, and an undefined constant
//! stays an error.

use roundhouse::det::HashMap;
use std::path::PathBuf;

use roundhouse::analyze::{Analyzer, attribution::attribute_ingest_gaps, diagnose};
use roundhouse::diagnostic::{Diagnostic, DiagnosticKind, Severity};
use roundhouse::ingest::{ingest_app_from_tree, survey};

const BASE_STORE: &str = "class BaseStore\n  def delete_entry(key)\n    key\n  end\nend\n";
// A visibility change naming its method through a local is not
// modeled, and fails the file's class ingest.
const VISIBILITY_GAP: &str = "visibility method names must be literal symbols or strings";

/// Survey-ingest, analyze and attribute an app of `files`. Checks that
/// a second attribution pass changes nothing.
fn check(files: &[(&str, &str)]) -> Vec<Diagnostic> {
    let tree: HashMap<PathBuf, Vec<u8>> = [
        ("db/schema.rb", "ActiveRecord::Schema.define do\nend\n"),
        ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
    ]
    .iter()
    .chain(files)
    .map(|(path, text)| (PathBuf::from(path), text.as_bytes().to_vec()))
    .collect();
    survey::activate();
    let mut app = ingest_app_from_tree(tree).expect("survey ingest continues past the gap");
    let gaps = survey::drain();
    assert!(!gaps.is_empty(), "the app records a gap");
    Analyzer::new(&app).analyze(&mut app);
    let mut diags = diagnose(&app);
    attribute_ingest_gaps(&mut diags, &app, &gaps);
    // Attribution is idempotent across front ends that rerun it.
    let once = diags.clone();
    attribute_ingest_gaps(&mut diags, &app, &gaps);
    assert_eq!(diags, once);
    diags
}

fn constant_refusal<'a>(diags: &'a [Diagnostic], name: &str, severity: Severity) -> &'a Diagnostic {
    let d = diags
        .iter()
        .find(|d| matches!(&d.kind,
            DiagnosticKind::Unsupported { construct, detail, .. }
                if construct.as_str() == "constant" && detail == name))
        .unwrap_or_else(|| panic!("no constant refusal for {name}: {diags:#?}"));
    assert_eq!(d.severity, severity, "{}", d.message);
    d
}

fn gap_error(diags: &[Diagnostic]) {
    assert!(
        diags.iter().any(|d| d.severity == Severity::Error
            && matches!(&d.kind, DiagnosticKind::IngestUnsupported { detail } if detail == VISIBILITY_GAP)),
        "the gap itself stays an error: {diags:#?}"
    );
}

fn dropped(written: &str, name: &str, file: &str, gap: &str) -> String {
    format!(
        "constant not supported (all targets): {written} — likely roundhouse coverage, \
         not an app error (ingest gap in {file}: {name} was not compiled: {gap})"
    )
}

#[test]
fn reference_to_a_gap_dropped_class_names_its_file_and_gap() {
    let diags = check(&[
        ("app/lib/base_store.rb", BASE_STORE),
        (
            "app/lib/stores/local_store.rb",
            "TOP_LIMIT = 9\n\nmodule Stores\n  class LocalStore < BaseStore\n    LIMIT = 3\n\n    def reset\n      1\n    end\n\n    name = :reset\n    private name\n  end\nend\n",
        ),
        (
            "app/lib/stores/user.rb",
            "module Stores\n  class User\n    def go\n      LocalStore.new.reset\n    end\n\n    def limit\n      [Stores::LocalStore::LIMIT, TOP_LIMIT]\n    end\n\n    def missing\n      NoSuchStore.new\n    end\n  end\nend\n",
        ),
    ]);
    gap_error(&diags);
    let file = "app/lib/stores/local_store.rb";
    // The written name resolves lexically to the nested declaration.
    assert_eq!(
        constant_refusal(&diags, "LocalStore", Severity::Info).message,
        dropped("LocalStore", "Stores::LocalStore", file, VISIBILITY_GAP)
    );
    // Constants that the dropped file assigns, nested or top-level.
    assert_eq!(
        constant_refusal(&diags, "Stores::LocalStore::LIMIT", Severity::Info).message,
        dropped("Stores::LocalStore::LIMIT", "Stores::LocalStore::LIMIT", file, VISIBILITY_GAP)
    );
    assert_eq!(
        constant_refusal(&diags, "TOP_LIMIT", Severity::Info).message,
        dropped("TOP_LIMIT", "TOP_LIMIT", file, VISIBILITY_GAP)
    );
    // A constant nothing defines is not attributed to the gap.
    assert_eq!(
        constant_refusal(&diags, "NoSuchStore", Severity::Error).message,
        "constant not supported (all targets): NoSuchStore"
    );
}

/// The file's integer-literal gap comes first, but ingest absorbs it with
/// a `nil` placeholder. The visibility change is the gap that failed the
/// file's class ingest, so it is the one named.
#[test]
fn the_named_gap_is_the_one_that_failed_the_file() {
    let diags = check(&[
        ("app/lib/base_store.rb", BASE_STORE),
        (
            "app/lib/local_store.rb",
            "class Helper\n  def big\n    99999999999999999999999\n  end\nend\n\nclass LocalStore < BaseStore\n  def reset\n    1\n  end\n\n  name = :reset\n  private name\nend\n",
        ),
        ("app/lib/user.rb", "class User\n  def go\n    LocalStore.new.reset\n  end\nend\n"),
    ]);
    assert_eq!(
        constant_refusal(&diags, "LocalStore", Severity::Info).message,
        dropped("LocalStore", "LocalStore", "app/lib/local_store.rb", VISIBILITY_GAP)
    );
}

/// A gap that ingest absorbed leaves `Settings` and its constants in the
/// IR. Their refusals (untyped values) have another cause and keep their
/// plain message.
#[test]
fn a_gap_that_kept_the_class_is_not_named() {
    let diags = check(&[
        (
            "app/lib/settings.rb",
            "class Settings\n  A = Object.new.frobnicate\n  B = ENV.fetch(\"X\").zork\n\n  def huge\n    99999999999999999999999\n  end\nend\n",
        ),
        ("app/lib/user.rb", "class User\n  def go\n    [Settings::A, Settings::B]\n  end\nend\n"),
    ]);
    for name in ["Settings::A", "Settings::B"] {
        assert_eq!(
            constant_refusal(&diags, name, Severity::Error).message,
            format!("constant not supported (all targets): {name}")
        );
    }
}

/// A gap in a constant's own value: ingest kept the module but held the
/// constant as its `nil` placeholder. Core's
/// `MetaobjectsInternal::Codec::DoubleLayout::SIGN_BIT =
/// 0x8000000000000000` is wider than 64 bits. Its references are the
/// gap's shadow; a sibling constant ingest compiled is not named.
#[test]
fn a_constant_whose_value_was_the_gap_names_it() {
    let diags = check(&[
        (
            "app/lib/codec.rb",
            "module Codec\n  module DoubleLayout\n    TAG = 0x11 #: Integer\n    SIGN_BIT = 0x8000000000000000 #: Integer\n\n    class << self\n      #: (Integer) -> Integer\n      def pack(bits)\n        bits.anybits?(SIGN_BIT) ? TAG : 0\n      end\n    end\n  end\nend\n",
        ),
        ("app/lib/user.rb", "class User\n  def go\n    [Codec::DoubleLayout::SIGN_BIT, Codec::DoubleLayout::TAG, Codec::Missing]\n  end\nend\n"),
    ]);
    let d = constant_refusal(&diags, "Codec::DoubleLayout::SIGN_BIT", Severity::Info);
    assert!(
        d.message.contains("ingest gap in app/lib/codec.rb: Codec::DoubleLayout::SIGN_BIT was not compiled: integer literal"),
        "{}",
        d.message
    );
    assert!(
        !diags.iter().any(|d| matches!(&d.kind, DiagnosticKind::Unsupported { detail, .. } if detail == "Codec::DoubleLayout::TAG")),
        "{diags:#?}"
    );
    assert_eq!(constant_refusal(&diags, "Codec::Missing", Severity::Error).message, "constant not supported (all targets): Codec::Missing");
}
