//! A tableless ActiveModel class that writes its own `valid?` and calls
//! `super` in it (Core's `Home::Traits::Matcher`, `Home::BaseEvent`). The
//! lowering synthesized `valid?` only when the source did not define one,
//! so `super` found no superclass method and raised NoMethodError where
//! Rails reaches `ActiveModel::Validations#valid?`.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;
use std::process::Command;

/// `Matcher#valid?(context = nil)`: `super && arguments.all?(&:valid?)`.
const MATCHER: &str = r#"class Matcher
  include ActiveModel::Validations

  attr_reader :name, :arguments

  validates :name, presence: true

  def initialize(name, arguments = [])
    @name = name
    @arguments = arguments
  end

  def valid?(context = nil)
    super && arguments.all? { |argument| argument.valid? }
  end
end
"#;

/// `BaseEvent#valid?`: `result = super` then acts on it.
const EVENT: &str = r#"class Event
  include ActiveModel::Validations

  attr_reader :handle, :logged

  validates :handle, presence: true

  def initialize(handle)
    @handle = handle
    @logged = []
  end

  def valid?
    result = super
    @logged << errors.full_messages unless result
    result
  end
end
"#;

/// A rule `on:` a context, reached through `super(context)`.
const BOOKING: &str = r#"class Booking
  include ActiveModel::Validations

  attr_accessor :seat

  validates :seat, presence: true, on: :booking

  def valid?(context = nil)
    super(context)
  end
end
"#;

const PROBE: &str = r##"class ValidSuperProbe
  def self.lines
    out = []
    out << "ok: #{Matcher.new("a").valid?}"
    out << "blank: #{Matcher.new(nil).valid?}"
    out << "child bad: #{Matcher.new("a", [Matcher.new(nil)]).valid?}"
    out << "child ok: #{Matcher.new("a", [Matcher.new("b")]).valid?}"
    m = Matcher.new(nil)
    out << "invalid: #{m.invalid?} #{m.errors.full_messages.inspect}"
    event = Event.new("")
    out << "event bad: #{event.valid?} #{event.logged.inspect}"
    out << "event ok: #{Event.new("x").valid?}"
    booking = Booking.new
    out << "booking none: #{booking.valid?}"
    out << "booking context: #{booking.valid?(:booking)} #{booking.errors.full_messages.inspect}"
    booking.seat = "A1"
    out << "booking seated: #{booking.valid?(:booking)}"
    out
  end
end
"##;

const EXPECTED: &[&str] = &[
    "ok: true",
    "blank: false",
    "child bad: false",
    "child ok: true",
    r#"invalid: true ["Name can't be blank"]"#,
    r#"event bad: false [["Handle can't be blank"]]"#,
    "event ok: true",
    "booking none: true",
    r#"booking context: false ["Seat can't be blank"]"#,
    "booking seated: true",
];

const FILES: &[(&str, &str)] = &[
    ("app/services/matcher.rb", MATCHER),
    ("app/services/event.rb", EVENT),
    ("app/services/booking.rb", BOOKING),
    ("app/models/valid_super_probe.rb", PROBE),
];

const SCHEMA: &str = "ActiveRecord::Schema.define(version: 1) do\nend\n";

fn app() -> emit_and_run::Overlay {
    FILES
        .iter()
        .fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text))
        .write("db/schema.rb", SCHEMA)
        .write("config/database.yml", "test:\n  adapter: sqlite3\n  database: \":memory:\"\n")
}

/// The files on Core's activemodel (through its activerecord).
fn native() -> Vec<String> {
    let oracle = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/rails_oracle.rb");
    let dir = std::env::temp_dir().join(format!("rh_active_model_valid_super_{}", std::process::id()));
    for (path, text) in FILES {
        let file = dir.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    let loads: String = FILES.iter().map(|(path, _)| format!("load {path:?}\n")).collect();
    let program = format!(
        "require {oracle:?}\nRailsOracle.activate(\"activerecord\", sqlite3: \"2.9.2\")\nrequire \"active_support/all\"\nrequire \"active_record\"\n\
         abort \"native: activemodel is not Core's\" unless Gem.loaded_specs[\"activemodel\"].full_gem_path.start_with?(RailsOracle::CORE_GEM_HOME)\n\
         {loads}puts ValidSuperProbe.lines\n"
    );
    std::fs::write(dir.join("native.rb"), program).unwrap();
    let out = Command::new("ruby")
        .args(["-W0", "native.rb"])
        .current_dir(&dir)
        .env("GLIBC_TUNABLES", "glibc.rtld.optional_static_tls=262144")
        .output()
        .expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "native run failed:\n{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    std::fs::remove_dir_all(&dir).unwrap();
    stdout.lines().map(str::to_string).collect()
}

fn emitted() -> Vec<String> {
    let (dir, errors) = app().emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let script = "require File.expand_path('main', Dir.pwd)\nMain.configure_default_adapter!\nputs ValidSuperProbe.lines\n";
    let out = emit_and_run::ruby()
        .current_dir(&*dir)
        .args(["-W0", "-e", script])
        .env("BLOG_DB", ":memory:")
        .output()
        .expect("ruby");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "{stdout}\n{}\n{}",
        String::from_utf8_lossy(&out.stderr),
        std::fs::read_to_string(dir.join("app/services/matcher.rb")).unwrap_or_default()
    );
    stdout.lines().map(str::to_string).collect()
}

#[test]
fn a_valid_that_calls_super_reaches_activemodels() {
    let native = native();
    assert_eq!(native, EXPECTED, "native");
    assert_eq!(emitted(), native, "emitted");
}
