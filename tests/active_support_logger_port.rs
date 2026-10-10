//! activesupport's `ActiveSupport::Logger` is a `::Logger` subclass
//! (Shopify/rails 5813559f, 8.2.0.alpha: logger.rb, logger_silence.rb,
//! logger_thread_safe_level.rb). Shopify Core's StructuredLogger
//! (components/platform/essentials/lib/structured_logger.rb) subclasses it
//! and relies on that shape: it reads the severities `DEBUG` .. `UNKNOWN`
//! as inherited constants, overrides `add(severity, message, progname,
//! **)`, assigns `@level` and reaches `level` through `super`. The runtime's
//! hand-written logger (runtime/ruby/logger.rb) is no `::Logger`, so each
//! severity read was refused (8 check rows), as were
//! `ActiveSupport::Logger::DEBUG` (schema-migrations) and
//! `ActiveSupport::Logger::SimpleFormatter` (gems/ci).
//!
//! The CRuby and JRuby trees load Ruby's own logger and carry the port of
//! activesupport's (runtime/spinel/active_support_logger.rb,
//! `project::RUBY_FAMILY_LOGGER`). The file below, Core's StructuredLogger
//! with `Rails.event.notify` written as an append to `$events`, runs
//! natively on Core's pinned activesupport (tests/support/rails_oracle.rb)
//! and emitted on the port, and must print the same. Spinel has no
//! stdlib logger; it refuses each read by name.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;
use std::process::Command;

const STRUCTURED_LOGGER: &str = r#"class StructuredLogger < ActiveSupport::Logger
  SEVERITY_LABEL = {
    0 => "DEBUG",
    1 => "INFO",
    2 => "WARN",
    3 => "ERROR",
    4 => "FATAL",
    5 => "UNKNOWN",
  }.freeze #: Hash[Integer, String]

  def initialize(event_name)
    super(nil)
    @event_name = event_name
    @level = self.class.default_level
  end

  def level
    if $debug_mode
      DEBUG
    else
      super
    end
  end

  def debug(progname = nil, &block)
    add(DEBUG, nil, progname, caller_depth: 2, &block)
  end

  def info(progname = nil, &block)
    add(INFO, nil, progname, caller_depth: 2, &block)
  end

  def warn(progname = nil, &block)
    add(WARN, nil, progname, caller_depth: 2, &block)
  end

  def error(progname = nil, &block)
    add(ERROR, nil, progname, caller_depth: 2, &block)
  end

  def fatal(progname = nil, &block)
    add(FATAL, nil, progname, caller_depth: 2, &block)
  end

  def unknown(progname = nil, &block)
    add(UNKNOWN, nil, progname, caller_depth: 2, &block)
  end

  def add(severity, message = nil, progname = nil, **kwargs, &block)
    caller_depth = kwargs[:caller_depth] || 1
    severity = case severity
    when 0..5
      severity
    else
      UNKNOWN
    end

    log_level = SEVERITY_LABEL[severity]

    return true if severity < level

    if message.nil?
      message = if block_given?
        yield
      else
        progname.to_s
      end
    end

    case severity
    when DEBUG
      $events << [@event_name, message, log_level, caller_depth + 6]
    when INFO, WARN, UNKNOWN
      $events << [@event_name, message, log_level, caller_depth + 6]
    when ERROR, FATAL
      if (exception = progname).is_a?(Exception)
        $events << [@event_name, exception.message, log_level, exception.class.name]
      else
        $events << [@event_name, message, log_level, caller_depth + 6]
      end
    end
  end

  class << self
    def default_level
      @default_level ||= ::Logger::Severity::INFO
    end

    attr_writer :default_level
  end
end
"#;

/// The other two Core reads (schema-migrations' logging_helper.rb, gems/ci's
/// test_selector/cli.rb) and an `ActiveSupport::Logger` used directly.
const LOG_USERS: &str = r#"module LogUsers
  def self.levels
    [Logger::INFO, ActiveSupport::Logger::DEBUG, ::Logger::WARN, Logger::Severity::ERROR, Logger::UNKNOWN]
  end

  def self.write(io)
    logger = ActiveSupport::Logger.new(io)
    logger.add(Logger::WARN, "careful")
    logger.info("hello")
    logger.level = :warn
    logger.info("dropped")
    logger.silence do
      logger.warn("silenced")
      logger.error("loud")
    end
    logger.log_at(:debug) { logger.debug("traced") }
    [logger.level, logger.silencer, ActiveSupport::Logger.logger_outputs_to?(logger, io)]
  end

  def self.simple(io)
    logger = ActiveSupport::Logger.new(io)
    logger.formatter = ActiveSupport::Logger::SimpleFormatter.new
    logger.info([1, "two"])
    logger.warn("plain")
    logger
  end

  # Core's shapes (config/environments/test.rb, essentials' logging.rb):
  # a TaggedLogging BroadcastLogger, broadcast to a second logger.
  def self.broadcast(io, other)
    logger = ActiveSupport::TaggedLogging.new(ActiveSupport::BroadcastLogger.new(ActiveSupport::Logger.new(io)))
    second = ActiveSupport::Logger.new(other)
    logger.broadcast_to(second)
    logger.info("both")
    logger.debug { "a block, run once" }
    logger.tagged("req-1", "shop") { logger.warn("tagged") }
    logger.tagged("outer").info("no block")
    logger.level = :warn
    logger.info("dropped")
    logger.silence do
      logger.warn("silenced")
      logger.error("loud")
    end
    logger.stop_broadcasting_to(second)
    logger.error("first only")
    [logger.is_a?(ActiveSupport::BroadcastLogger), logger.broadcasts.size, logger.level, logger.progname,
     logger.formatter.class.name, logger.respond_to?(:datetime_format), logger.datetime_format]
  end
end
"#;

/// Run after the two files load, natively and emitted.
const OBSERVE: &str = r#"p LogUsers.levels
io = StringIO.new
p LogUsers.write(io)
p io.string
io = StringIO.new
p LogUsers.simple(io).formatter.class.name, io.string
p ActiveSupport::Logger.superclass, ActiveSupport::Logger.ancestors.include?(ActiveSupport::LoggerSilence)
p StructuredLogger::DEBUG, StructuredLogger::UNKNOWN
$events = []
logger = StructuredLogger.new("core.event")
logger.debug("hidden")
logger.info("info")
logger.warn { "from a block" }
logger.error("plain error")
logger.fatal(RuntimeError.new("boom"))
logger.unknown("unknown")
logger.add(9, "out of range")
p logger.level
$debug_mode = true
logger.debug("now shown")
p logger.level
$debug_mode = false
StructuredLogger.default_level = ::Logger::Severity::ERROR
quiet = StructuredLogger.new("quiet")
quiet.warn("dropped")
quiet.error("kept")
logger.local_level = :error
logger.info("thread-local level drops this")
logger.local_level = nil
logger.info("and keeps this")
io = StringIO.new
other = StringIO.new
p LogUsers.broadcast(io, other), io.string, other.string
tagged = ActiveSupport::TaggedLogging.new(ActiveSupport::BroadcastLogger.new(StructuredLogger.new("semian")))
tagged.info("via broadcast")
tagged.error { "block via broadcast" }
tagged.tagged("t") { tagged.warn("tagged via broadcast") }
p tagged.broadcasts.map(&:class), tagged.level
$events.each { |event| p event }
"#;

const EXPECTED: &str = r#"[1, 0, 2, 3, 5]
[2, true, true]
"careful\nhello\nloud\ntraced\n"
"#;

fn app(postgresql: bool) -> emit_and_run::Overlay {
    let app = emit_and_run::empty_app()
        .write("lib/structured_logger.rb", STRUCTURED_LOGGER)
        .write("lib/log_users.rb", LOG_USERS);
    if postgresql {
        app.write("roundhouse.yml", "test_paths:\n  - test\ndatabase_adapter: postgresql\n")
            .write("config/database.yml", "test:\n  adapter: postgresql\n  database: rh_active_support_logger\n")
    } else {
        app.write("roundhouse.yml", "test_paths:\n  - test\n")
    }
}

fn native() -> String {
    let oracle = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/rails_oracle.rb");
    let dir = std::env::temp_dir().join(format!("rh_active_support_logger_native_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("structured_logger.rb"), STRUCTURED_LOGGER).unwrap();
    std::fs::write(dir.join("log_users.rb"), LOG_USERS).unwrap();
    let program = format!(
        "require {oracle:?}\nRailsOracle.activate(\"activesupport\")\n\
         require \"active_support\"\nrequire \"active_support/logger\"\nrequire \"stringio\"\n\
         require \"active_support/tagged_logging\"\nrequire \"active_support/broadcast_logger\"\n\
         # `logger_outputs_to?` calls `try`, which a booted app has loaded.\n\
         require \"active_support/core_ext/object/try\"\n\
         abort \"native: activesupport is not Core's\" unless Gem.loaded_specs[\"activesupport\"].full_gem_path.start_with?(RailsOracle::CORE_GEM_HOME)\n\
         require_relative \"structured_logger\"\nrequire_relative \"log_users\"\n\
         {OBSERVE}"
    );
    std::fs::write(dir.join("native.rb"), program).unwrap();
    let out = Command::new("ruby")
        .arg("native.rb")
        .current_dir(&dir)
        .env("GLIBC_TUNABLES", "glibc.rtld.optional_static_tls=262144")
        .output()
        .expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "native run failed:\n{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    std::fs::remove_dir_all(&dir).unwrap();
    stdout
}

#[test]
fn structured_logger_runs_on_the_postgresql_tree_like_activesupport() {
    let (dir, errors) = app(true).emit(BuildTarget::Ruby);
    // `check` names the PostgreSQL adapter as having no runtime of its
    // own in target-independent analysis; the CRuby tree carries one.
    let errors: Vec<_> = errors.iter().filter(|e| !e.contains("database_adapter not supported")).collect();
    assert!(errors.is_empty(), "{errors:#?}");
    let logger = std::fs::read_to_string(dir.join("runtime/logger.rb")).expect("runtime/logger.rb");
    assert!(logger.contains("require \"logger\"") && logger.contains("require_relative \"active_support_logger\""), "{logger}");
    assert!(!logger.contains("def write_entry"), "the hand-written logger stays off the CRuby tree: {logger}");
    for file in ["app/models/structured_logger.rb", "app/models/log_users.rb"] {
        let text = std::fs::read_to_string(dir.join(file)).unwrap_or_else(|e| panic!("{file}: {e}"));
        assert!(!text.contains("roundhouse:"), "{file}: {text}");
    }
    let script = format!(
        "require_relative 'runtime/logger'\nrequire_relative 'app/models/structured_logger'\nrequire_relative 'app/models/log_users'\n{OBSERVE}"
    );
    let out = emit_and_run::ruby().current_dir(&*dir).args(["-W:no-deprecated", "-e", &script]).output().expect("ruby");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    assert!(stdout.starts_with(EXPECTED), "{stdout}");
    assert!(stdout.contains("[\"core.event\", \"boom\", \"FATAL\", \"RuntimeError\"]"), "{stdout}");
    assert_eq!(stdout, native(), "emitted on the port vs native activesupport");
}

/// The SQLite CRuby tree is a Ruby-family tree too: it boots the same
/// logger. Spinel has no stdlib logger; the severity reads are CRuby's own
/// there and `SimpleFormatter` is the Ruby-family port's.
#[test]
fn the_sqlite_tree_boots_it_and_spinel_refuses_the_reads() {
    let (dir, errors) = app(false).emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let script = format!("require File.expand_path('main', Dir.pwd)\nrequire_relative 'app/models/structured_logger'\nrequire_relative 'app/models/log_users'\n{OBSERVE}");
    let out = emit_and_run::ruby().current_dir(&*dir).args(["-W0", "-e", &script]).env("BLOG_DB", ":memory:").output().expect("ruby");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    assert!(stdout.starts_with(EXPECTED), "{stdout}");

    let (_dir, errors) = app(false).emit(BuildTarget::Spinel);
    for name in ["ActiveSupport::Logger::DEBUG", "Logger::WARN"] {
        let expected = format!("{name} is CRuby's own; the spinel runtime does not define it");
        assert!(errors.iter().any(|e| e.contains(&expected)), "spinel {name}: {errors:#?}");
    }
    for name in ["DEBUG", "INFO", "WARN", "ERROR", "FATAL", "UNKNOWN"] {
        let expected = format!(": {name} is CRuby's own; the spinel runtime does not define it");
        assert!(errors.iter().any(|e| e.contains(&expected)), "spinel bare {name}: {errors:#?}");
    }
    assert!(
        errors.iter().any(|e| e.contains(
            "ActiveSupport::Logger::SimpleFormatter is declared by the Ruby-family runtime (runtime/active_support_logger.rb), which spinel does not emit"
        )),
        "{errors:#?}"
    );
}
