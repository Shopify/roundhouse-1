//! `attr_reader(*LIST)`: an `attr_*` declaration splatting a list the
//! class body holds.
//!
//! country_db 23.21.0's `CountryDb::Country` declares its readers as
//! `PUBLIC_ATTRIBUTES = [:code, :format_extended_v2, …]`,
//! `private_constant :PUBLIC_ATTRIBUTES`, `attr_reader(*PUBLIC_ATTRIBUTES)`
//! (Zone and LocalizationKeys the same), and Core's Zendesk resources
//! splat `ALLOWED_ATTRIBUTES + USER_FIELDS.keys` and
//! `EXTERNALLY_ACCESSIBLE_ATTRIBUTES | EXTERNALLY_IMMUTABLE_ATTRIBUTES`.
//! The ingest kept only symbol arguments, so the splat was dropped with
//! no diagnostic and every `country.format_extended_v2` was a NameError
//! in the emitted tree. A list the ingest can read declares its names; a
//! splat of anything else (a method call) is written back in place where
//! the body can carry it, and named where it cannot.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::det::HashMap;
use roundhouse::dialect::MethodReceiver;
use roundhouse::ingest::ingest_app_from_tree;
use roundhouse::project::BuildTarget;
use std::path::PathBuf;

const COUNTRY: &str = r#"module CountryDb
  class Country
    PUBLIC_ATTRIBUTES = [
      :code,
      :format_extended_v2, # a comment, as country.rb has
      :zones,
    ]
    private_constant :PUBLIC_ATTRIBUTES
    attr_reader(*PUBLIC_ATTRIBUTES)

    ATTRS = [:a, :b].freeze
    attr_reader(*ATTRS)

    ALLOWED_ATTRIBUTES = [:id, :email].freeze
    USER_FIELDS = {
      out_of_office: "agent_ooo",
    }.freeze #: Hash[Symbol, String]
    attr_accessor(*(ALLOWED_ATTRIBUTES + USER_FIELDS.keys))

    ACCESSIBLE = ["subject", "comment"].freeze
    IMMUTABLE = ["uid", "subject"].freeze
    ALL_ATTRIBUTES = ACCESSIBLE | IMMUTABLE #: Array[String]
    attr_reader(*ALL_ATTRIBUTES)

    attr_writer "label", *%i[note]

    HEADER_CLASSES = { hsts: 1, csp: 2, referrer_policy: 3, csp_report_only: 4, cookies: 5 }.freeze
    HEADER_KEYS = HEADER_CLASSES.keys.freeze
    NOT_COOKIES = (HEADER_KEYS - [:cookies]).freeze
    attr_writer(*(HEADER_CLASSES.reject { |key| [:csp, :csp_report_only].include?(key) }.keys))
    attr_reader(*NOT_COOKIES)

    protected

    attr_writer(*IMMUTABLE)

    def initialize
      @code = "CA"
      @format_extended_v2 = "{firstName}{lastName}"
      @zones = []
      @a = 1
      @b = 2
      @id = 3
      @out_of_office = true
      @subject = "s"
      @uid = "u"
      @hsts = 1
      @csp = 2
      @referrer_policy = 3
      @csp_report_only = 4
    end

    def self.report
      c = new
      c.email = "e@x"
      c.label = "l"
      c.note = "n"
      c.referrer_policy = "same-origin"
      [c.code, c.format_extended_v2, c.zones, c.a, c.b, c.id, c.email, c.out_of_office,
       c.subject, c.comment, c.uid, c.respond_to?(:uid=), c.protected_methods.sort,
       c.instance_variable_get(:@label), c.instance_variable_get(:@note),
       c.hsts, c.csp, c.referrer_policy, c.csp_report_only, c.respond_to?(:cookies), c.respond_to?(:csp=),
       c.respond_to?(:hsts=), c.respond_to?(:cookies=), c.respond_to?(:referrer_policy=)].inspect
    end
  end
end
"#;

/// What CRuby prints for `CountryDb::Country.report`.
const EXPECTED: &str = r#"["CA", "{firstName}{lastName}", [], 1, 2, 3, "e@x", true, "s", nil, "u", false, [:subject=, :uid=], "l", "n", 1, 2, "same-origin", 4, false, false, true, true, true]"#;

fn tree(files: &[(&str, &str)]) -> HashMap<PathBuf, Vec<u8>> {
    files.iter().map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec())).collect()
}

#[test]
fn a_splat_of_a_literal_list_constant_declares_each_name() {
    let mut app = ingest_app_from_tree(tree(&[("lib/country_db/country.rb", COUNTRY)])).expect("ingest");
    roundhouse::session::analyze_and_lower(&mut app);
    let country = app
        .library_classes
        .iter()
        .find(|lc| lc.name.0.as_str() == "CountryDb::Country")
        .expect("CountryDb::Country is ingested");
    let instance: Vec<&str> = country
        .methods
        .iter()
        .filter(|m| m.receiver == MethodReceiver::Instance)
        .map(|m| m.name.as_str())
        .collect();
    for name in [
        "code",
        "format_extended_v2",
        "zones",
        "a",
        "b",
        "id",
        "id=",
        "email",
        "email=",
        "out_of_office",
        "out_of_office=",
        "subject",
        "comment",
        "uid",
        "subject=",
        "uid=",
        "label=",
        "note=",
        "referrer_policy=",
        "hsts=",
        "cookies=",
    ] {
        assert!(instance.contains(&name), "{name} not declared: {instance:?}");
    }
    // `ACCESSIBLE | IMMUTABLE` is a union: `subject` once.
    assert_eq!(instance.iter().filter(|n| **n == "subject").count(), 1, "{instance:?}");
}

#[test]
fn the_declared_readers_run_as_in_cruby() {
    let native = emit_and_run::ruby()
        .args(["-e", &format!("{COUNTRY}\nputs CountryDb::Country.report\n")])
        .output()
        .expect("CRuby control");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    assert_eq!(String::from_utf8_lossy(&native.stdout).trim(), EXPECTED, "native control");

    let run = emit_and_run::empty_app()
        .write("lib/country_db/country.rb", COUNTRY)
        .run_ruby("puts CountryDb::Country.report\n");
    run.assert_passes();
    assert_eq!(run.stdout.trim(), EXPECTED, "emitted Ruby diverges from CRuby");
}

/// A splat of a method call is no list the ingest reads. Where the body
/// carries its statements in place (a public declaration), Ruby runs it
/// there; where it cannot (below `private`), it is named, not dropped.
#[test]
fn a_splat_the_ingest_cannot_read_is_carried_or_named() {
    const DYNAMIC: &str = r#"class Dynamic
  def self.names
    [:x, :y]
  end
  attr_reader(*names)

  def initialize
    @x = 1
    @y = 2
  end

  def self.report
    d = new
    [d.x, d.y].inspect
  end
end
"#;
    let run = emit_and_run::empty_app().write("lib/dynamic.rb", DYNAMIC).run_ruby("puts Dynamic.report\n");
    run.assert_passes();
    assert_eq!(run.stdout.trim(), "[1, 2]");

    const HIDDEN: &str = r#"class Hidden
  def self.names
    [:x]
  end

  private

  attr_reader(*names)
end
"#;
    let (_tree, errors) = emit_and_run::empty_app().write("lib/hidden.rb", HIDDEN).emit(BuildTarget::Ruby);
    assert!(
        errors.iter().any(|e| e.contains("an `attr_*` declaration splatting a list the ingest cannot read")
            && e.contains("attr_reader(*names)")),
        "{errors:#?}"
    );
}
