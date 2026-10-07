//! A concern's CLASS-side methods reach the models and library classes
//! that include it (`splice_concern_class_methods_into_includers`).
//!
//! `include` never carries them. ActiveSupport::Concern only gets away
//! with it because `append_features` runs `base.extend ClassMethods`, and
//! the emitted modules have no Concern — so `Message.create_with_attachment!`
//! resolved in analyze (the registry fold copies the class side onto
//! includers) and NoMethodError'd at runtime.
//!
//! The carrier is the whole distinction. `module ClassMethods` and
//! `class_methods do` are inherited; a module's OWN singletons —
//! `module_function :x`, `class << self` — are not. `class_methods do`
//! flattens into the module as `def self.…`; `module ClassMethods` stays
//! a module of its own, whose instance defs the splice reads.

use roundhouse::det::HashMap;
use std::path::PathBuf;

use roundhouse::analyze::{diagnose, Analyzer, DiagnosticKind};
use roundhouse::emit::ruby;
use roundhouse::ingest::ingest_app_from_tree;
use roundhouse::dialect::MethodReceiver;
use roundhouse::ty::Ty;

fn tree(files: &[(&str, &str)]) -> HashMap<PathBuf, Vec<u8>> {
    files
        .iter()
        .map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec()))
        .collect()
}

fn app() -> roundhouse::App {
    ingest_app_from_tree(tree(&[
        (
            "db/schema.rb",
            r#"ActiveRecord::Schema.define do
  create_table "messages", force: :cascade do |t|
    t.string "body", null: false
    t.string "token", null: false
  end
end
"#,
        ),
        (
            "app/models/message.rb",
            r#"class Message < ApplicationRecord
  include Message::Attachment, Message::Tokenized, Message::Blocklist

  def self.paged?
    true
  end
end
"#,
        ),
        (
            "app/models/message/attachment.rb",
            r#"module Message::Attachment
  extend ActiveSupport::Concern

  MAX_WIDTH = 1200

  module ClassMethods
    def create_with_attachment!(attributes)
      create!(attributes)
    end

    def widest
      MAX_WIDTH
    end

    def paged?
      false
    end
  end
end
"#,
        ),
        (
            "app/models/message/tokenized.rb",
            r#"module Message::Tokenized
  extend ActiveSupport::Concern

  class_methods do
    def from_token(token)
      find_by(token: token)
    end
  end
end
"#,
        ),
        (
            // The lobsters `EmailBlocklistValidation` shape: the module's
            // OWN singleton, which Rails does not put on an includer.
            "app/models/message/blocklist.rb",
            r#"module Message::Blocklist
  extend ActiveSupport::Concern

  def blocked?
    Message::Blocklist.on_blocklist?(body)
  end

  def on_blocklist?(text)
    text == "spam"
  end

  module_function :on_blocklist?
end
"#,
        ),
    ]))
    .expect("ingest")
}

fn message() -> String {
    let files = ruby::emit_lowered_models(&app());
    files
        .iter()
        .find(|f| f.path.to_string_lossy().ends_with("app/models/message.rb"))
        .map(|f| f.content.clone())
        .expect("message.rb")
}

/// The shape that sent me here: campfire's
/// `Message::Attachment::ClassMethods#create_with_attachment!`.
#[test]
fn class_methods_module_reaches_the_including_model() {
    let m = message();
    assert!(
        m.contains("def self.create_with_attachment!(attributes)"),
        "`module ClassMethods` method lands on the model:\n{m}"
    );
}

/// `class_methods do` is sugar Concern turns into that same module, so
/// both spellings have to arrive.
#[test]
fn class_methods_block_reaches_the_including_model() {
    let m = message();
    assert!(
        m.contains("def self.from_token(token)"),
        "`class_methods do` method lands on the model:\n{m}"
    );
}

/// A module-level `class << self` defines the MODULE's own singleton,
/// Concern or not: native Ruby answers `Message.respond_to?(:from_token)`
/// false and `Message::Window.from_token` keeps working. Only
/// `class_methods do` / `module ClassMethods` reach the includer.
#[test]
fn class_shift_self_stays_on_the_module() {
    let app = ingest_app_from_tree(tree(&[
        (
            "db/schema.rb",
            "ActiveRecord::Schema.define do\n  create_table \"messages\", force: :cascade do |t|\n    t.string \"body\"\n  end\nend\n",
        ),
        (
            "app/models/message.rb",
            "class Message < ApplicationRecord\n  include Message::Window\nend\n",
        ),
        (
            "app/models/message/window.rb",
            "module Message::Window\n  extend ActiveSupport::Concern\n  class << self\n    def from_token(token)\n      token\n    end\n  end\nend\n",
        ),
    ]))
    .expect("ingest");
    let files = ruby::emit_lowered_models(&app);
    let message = files
        .iter()
        .find(|f| f.path.to_string_lossy().ends_with("app/models/message.rb"))
        .map(|f| f.content.clone())
        .expect("message.rb");
    assert!(
        !message.contains("from_token"),
        "class << self method stays off the includer:\n{message}"
    );
    let window = app.library_classes.iter().find(|lc| lc.name.0.as_str() == "Message::Window").expect("Message::Window");
    assert!(window.methods.iter().any(|m| m.name.as_str() == "from_token"), "the module keeps its singleton");
}

/// `module_function` makes a singleton on the MODULE. Rails leaves it
/// there. Copying it invented `User.email_on_blocklist?` on three
/// lobsters models before the carrier list existed.
#[test]
fn module_own_singletons_do_not_reach_the_includer() {
    let m = message();
    assert!(
        !m.contains("def self.on_blocklist?"),
        "a module_function singleton stays on its module:\n{m}"
    );
}

/// Ruby's ancestor order: the class's own definition beats the module's.
#[test]
fn the_models_own_class_method_wins() {
    let m = message();
    assert_eq!(
        m.matches("def self.paged?").count(),
        1,
        "exactly one paged?, the model's own:\n{m}"
    );
    assert!(m.contains("    true\n"), "the model's body survives:\n{m}");
}

/// A lifted body's bare constant resolved against the module it was
/// written in and would resolve against the MODEL once moved — the same
/// lexical trap the controller splice hit with lobsters' TIME_INTERVALS.
#[test]
fn a_lifted_body_keeps_its_modules_constants() {
    let m = message();
    assert!(
        m.contains("Message::Attachment::MAX_WIDTH"),
        "bare MAX_WIDTH is qualified to its module:\n{m}"
    );
}

const FACTORY: &str = r#"module Factory
  def self.included(base)
    base.extend(ClassMethods)
  end
  PREFIX = "reading:"
  def self.module_only
    "not inherited"
  end
  module ClassMethods
    def build(**fields)
      new(**fields).freeze
    end
    def fixed
      Reading.new(label: "fixed").freeze
    end
    def prefix(value = PREFIX)
      value
    end
  end
end
"#;
const READING: &str = "class Reading < T::Struct\n  include Factory\n  PREFIX = \"local:\"\n  const :label, String\nend\n";
const PACKET: &str = "class Packet < T::Struct\n  include Factory\n  const :size, Integer\nend\n";
const CONSUMER: &str = r#"class Consumer
  def self.label
    Reading.build(label: "probe").label
  end
  def self.size
    Packet.build(size: 7).size
  end
end
"#;

fn factory_app(path: &str, declarations: &str, consumer: &str, action: &str) -> roundhouse::App {
    let mut app = ingest_app_from_tree(tree(&[
        (path, FACTORY),
        ("app/services/values.rb", declarations),
        ("app/services/consumer.rb", consumer),
        ("app/controllers/gauges_controller.rb", action),
        ("app/controllers/application_controller.rb", "class ApplicationController < ActionController::Base\nend\n"),
        ("config/routes.rb", "Rails.application.routes.draw do\n  get \"/gauges\", to: \"gauges#index\"\nend\n"),
    ])).expect("ingest factory");
    Analyzer::new(&app).analyze(&mut app);
    app
}

fn return_type(app: &roundhouse::App, class: &str, method: &str) -> Ty {
    app.library_classes.iter().find(|c| c.name.0.as_str() == class).unwrap()
        .methods.iter().find(|m| m.name.as_str() == method).unwrap()
        .body.ty.clone().unwrap()
}

#[test]
fn an_inferred_concern_factory_answers_each_receiving_class_in_both_orders() {
    // Specialize before typing, rather than replacing every nominal
    // module return with SelfInstance: fixed-other factories must stay fixed.
    for declarations in [format!("{READING}{PACKET}"), format!("{PACKET}{READING}")] {
        for path in ["app/services/factory.rb", "app/helpers/factory.rb", "lib/factory.rb"] {
            let app = factory_app(path, &declarations, CONSUMER, r#"class GaugesController < ApplicationController
  def index
    @label = Reading.build(label: "probe").label
    @size = Packet.build(size: 7).size
  end
end
"#);
            let errors: Vec<_> = diagnose(&app).into_iter()
                .filter(|d| d.severity == roundhouse::diagnostic::Severity::Error).collect();
            assert!(errors.is_empty(), "{path}: factory errors = {errors:?}");
            assert_eq!(return_type(&app, "Consumer", "label"), Ty::Str);
            assert_eq!(return_type(&app, "Consumer", "size"), Ty::Int);
            for class in ["Reading", "Packet"] {
                let expected = Ty::Class {
                    id: roundhouse::ClassId(roundhouse::Symbol::new(class)), args: vec![],
                };
                assert_eq!(return_type(&app, class, "build"), expected);
                let lc = app.library_classes.iter().find(|c| c.name.0.as_str() == class).unwrap();
                let build = lc.methods.iter().find(|m| m.name.as_str() == "build").unwrap();
                let Some(Ty::Fn { ret, .. }) = &build.signature else { panic!("factory signature") };
                assert_eq!(**ret, expected, "signature must be concrete, not SelfInstance");
                assert_eq!(return_type(&app, class, "fixed"), Ty::Class {
                    id: roundhouse::ClassId(roundhouse::Symbol::new("Reading")), args: vec![],
                });
            }
        }
    }
}

#[test]
fn factory_typos_and_wrong_class_fields_still_fail_dispatch() {
    let app = factory_app("app/services/factory.rb", &format!("{READING}{PACKET}"), CONSUMER, r#"class GaugesController < ApplicationController
  def index
    @typo = Reading.build(label: "probe").lable
    @wrong = Packet.build(size: 7).label
    @fixed = Packet.fixed.size
    @missing = Reading.bulid(label: "probe")
  end
end
"#);
    let failed: Vec<_> = diagnose(&app).into_iter().filter_map(|d| match d.kind {
        DiagnosticKind::SendDispatchFailed { method, recv_ty } => {
            Some(format!("{}#{}", roundhouse::ide::render_ty(&recv_ty), method.as_str()))
        }
        _ => None,
    }).collect();
    for expected in ["Reading#lable", "Packet#label", "Reading#size", "Reading#bulid"] {
        assert!(failed.iter().any(|f| f == expected), "missing {expected}: {failed:?}");
    }
}

#[test]
fn an_includers_own_factory_and_constructor_win() {
    let reading = READING.replace("end\n", "  def self.build(**fields)\n    Packet.new(size: 31).freeze\n  end\nend\n");
    let app = factory_app("app/services/factory.rb", &format!("{reading}{PACKET}"), "", "");
    assert_eq!(return_type(&app, "Reading", "build"), Ty::Class {
        id: roundhouse::ClassId(roundhouse::Symbol::new("Packet")), args: vec![],
    });
    let reading = app.library_classes.iter().find(|c| c.name.0.as_str() == "Reading").unwrap();
    assert_eq!(reading.methods.iter().filter(|m| m.name.as_str() == "build").count(), 1);

    let packet = PACKET.replace("end\n", "  def self.new(**fields)\n    Reading.new(label: \"custom\")\n  end\nend\n");
    let app = factory_app("app/services/factory.rb", &format!("{READING}{packet}"), "", "");
    assert_eq!(return_type(&app, "Packet", "build"), Ty::Class {
        id: roundhouse::ClassId(roundhouse::Symbol::new("Reading")), args: vec![],
    });
}

/// The carrier is emitted as its own module and reaches the includers
/// through the authored hook; the spliced copies type the includers'
/// class side but are not written into them.
#[test]
fn library_factory_splicing_preserves_lexical_constants_and_carrier_boundaries() {
    let mut app = factory_app("app/services/factory.rb", &format!("{READING}{PACKET}"), CONSUMER, "");
    roundhouse::lower::class_body_new::apply_class_body_new_lowering(&mut app);
    for class in ["Reading", "Packet"] {
        let lc = app.library_classes.iter().find(|c| c.name.0.as_str() == class).unwrap();
        // The typing copy is there (and qualified to the carrier's lexical scope) ...
        let build = lc.methods.iter().filter(|m| m.name.as_str() == "build").count();
        assert_eq!(build, 1, "{class}");
        let splices = &app.class_methods_carrier_splices[&lc.name];
        assert_eq!(splices.len(), 1, "{class}: {splices:?}");
        assert_eq!(splices[0].carrier.0.as_str(), "Factory::ClassMethods");
        assert!(!splices[0].extend, "the authored hook extends, not the includer");
        for method in ["build", "fixed", "prefix"] {
            assert!(splices[0].methods.iter().any(|m| m.as_str() == method), "{class}: {splices:?}");
        }
    }
    let emitted = ruby::emit_library(&app);
    let file = |suffix: &str| emitted.iter().find(|f| f.path.to_string_lossy().ends_with(suffix))
        .unwrap_or_else(|| panic!("no {suffix}")).content.clone();
    for class in ["reading", "packet"] {
        let values = file(&format!("/{class}.rb"));
        // ... but the emitted includer gets the methods from the module.
        assert!(values.contains("include Factory"), "{values}");
        assert!(!values.contains("def self.build") && !values.contains("def self.prefix"), "{values}");
        assert!(!values.contains("def self.module_only"), "{values}");
        assert!(!values.contains("extend Factory::ClassMethods"), "{values}");
    }
    let factory = file("/factory.rb");
    assert!(factory.contains("def self.included(base)") && factory.contains("base.extend(Factory::ClassMethods)"), "{factory}");
    assert!(!factory.contains("def self.build"), "{factory}");
    let carrier = file("/factory/class_methods.rb");
    assert!(carrier.contains("module Factory\n  module ClassMethods"), "{carrier}");
    assert!(carrier.contains("def build(**fields)") && carrier.contains("def prefix(value = Factory::PREFIX)"), "{carrier}");
}

/// The vanilla bridge `def self.included(base) = base.extend(ClassMethods)`
/// is the module's real hook: it stays as authored in every spelling, and
/// the nested module stays a module whose defs are instance methods (the
/// owner answers none of them, as in Ruby).
#[test]
fn the_class_methods_bridge_is_kept_as_authored() {
    for source in [
        FACTORY.to_string(),
        FACTORY.replace("base.extend(ClassMethods)", "base.extend(ClassMethods)\n    puts \"registered\""),
        FACTORY.replace("base.extend(ClassMethods)", "base.extend(OtherMethods)"),
        FACTORY.replace("base.extend(ClassMethods)", "other.extend(ClassMethods)"),
        FACTORY.replace("included(base)", "included(base, **options)"),
        FACTORY.replace("module ClassMethods", "module OtherMethods"),
    ] {
        let classes = roundhouse::ingest::library_class::ingest_library_classes(source.as_bytes(), "factory.rb").unwrap();
        let factory = classes.iter().find(|c| c.name.0.as_str() == "Factory").unwrap();
        assert!(factory.methods.iter().any(|m| m.name.as_str() == "included" && m.receiver == MethodReceiver::Class), "{source}");
        assert!(!factory.methods.iter().any(|m| matches!(m.name.as_str(), "build" | "fixed" | "prefix")), "{source}");
        let nested = if source.contains("module OtherMethods") { "Factory::OtherMethods" } else { "Factory::ClassMethods" };
        let carrier = classes.iter().find(|c| c.name.0.as_str() == nested).unwrap_or_else(|| panic!("{nested}: {source}"));
        assert!(carrier.is_module);
        let defs: Vec<_> = carrier.methods.iter().map(|m| (m.name.as_str(), m.receiver)).collect();
        assert_eq!(defs, [("build", MethodReceiver::Instance), ("fixed", MethodReceiver::Instance), ("prefix", MethodReceiver::Instance)], "{source}");
    }
}

/// A bridge in a different reopening than its carrier is kept too, and the
/// includer's class side is typed from the carrier only when the carrier
/// is the bridge owner's own nested module.
#[test]
fn split_bridges_require_the_actual_nested_carrier_and_keep_other_callbacks() {
    let carrier = "module Factory\n  module ClassMethods\n    def build(**fields)\n      new(**fields).freeze\n    end\n  end\nend\n";
    let block_only = carrier.replace("module ClassMethods", "class_methods do");
    let other_owner = carrier.replace("module Factory", "module OtherFactory");
    let overridden = format!("{carrier}module Factory\n  def self.included(base)\n    puts \"registered\"\n  end\nend\n");
    for (hook, declaration, carrier_module) in [
        ("base.extend(ClassMethods)", carrier, Some("Factory::ClassMethods")),
        ("base.extend(ClassMethods)\n    puts \"registered\"", carrier, Some("Factory::ClassMethods")),
        ("base.extend(OtherMethods)", carrier, Some("Factory::ClassMethods")),
        ("base.extend(ClassMethods)", block_only.as_str(), None),
        ("base.extend(ClassMethods)", other_owner.as_str(), Some("OtherFactory::ClassMethods")),
        ("base.extend(ClassMethods)", overridden.as_str(), Some("Factory::ClassMethods")),
    ] {
        let bridge = format!("module Factory\n  def self.included(base)\n    {hook}\n  end\nend\n");
        let app = ingest_app_from_tree(tree(&[
            ("lib/factory.rb", declaration),
            ("lib/factory_bridge.rb", &bridge),
            ("lib/gauge.rb", "class Gauge\n  include Factory\nend\n"),
        ])).expect("ingest split carrier and bridge");
        let hooks = app.library_classes.iter()
            .filter(|c| c.name.0.as_str() == "Factory")
            .flat_map(|c| &c.methods)
            .filter(|m| m.name.as_str() == "included")
            .count();
        let expected_hooks = if declaration.contains("puts") { 2 } else { 1 };
        assert_eq!(hooks, expected_hooks, "{bridge}\n{declaration}");
        let module = carrier_module.map(|name| app.library_classes.iter().find(|c| c.name.0.as_str() == name)
            .unwrap_or_else(|| panic!("{name}")));
        assert!(module.is_none_or(|m| m.is_module && m.methods.iter().any(|d| d.name.as_str() == "build" && d.receiver == MethodReceiver::Instance)));
        let gauge = app.library_classes.iter().find(|c| c.name.0.as_str() == "Gauge").unwrap();
        let typed_build = gauge.methods.iter().any(|m| m.name.as_str() == "build" && m.receiver == MethodReceiver::Class);
        // `class_methods do` is the Concern block spelling, a carrier of
        // Factory's own; the other owner's module is not Factory's.
        let expect_build = carrier_module == Some("Factory::ClassMethods") || block_only == declaration;
        assert_eq!(typed_build, expect_build, "{bridge}\n{declaration}");
    }
}

/// A helper's nested `module ClassMethods` is its class side: none of it
/// is a bare view helper.
#[test]
fn a_helper_carrier_is_not_view_visible() {
    let app = ingest_app_from_tree(tree(&[
        ("db/schema.rb", "ActiveRecord::Schema.define do\nend\n"),
        ("app/helpers/flags_helper.rb", "module FlagsHelper\n  def self.included(base)\n    base.extend(ClassMethods)\n  end\n\n  module ClassMethods\n    def flag_names = [:a]\n  end\n\n  def flag_label = \"x\"\nend\n"),
    ])).expect("ingest");
    assert!(app.helper_method_index.contains_key(&roundhouse::Symbol::new("flag_label")));
    assert!(!app.helper_method_index.contains_key(&roundhouse::Symbol::new("flag_names")), "{:?}", app.helper_method_index);
}
