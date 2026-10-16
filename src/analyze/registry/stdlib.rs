//! Ruby stdlib singletons (`File`/`Dir`/`SecureRandom`/…), the
//! `Rails`/`Time`/`Date`/`DateTime` singletons, and the gem-ecosystem
//! catalog fold. Extracted verbatim from `Analyzer::with_adapter`.

use crate::det::HashMap;

use crate::analyze::ClassInfo;
use crate::ident::{ClassId, Symbol};
use crate::ty::Ty;

pub(in crate::analyze) fn register(classes: &mut HashMap<ClassId, ClassInfo>) {
    // Rails singleton — `Rails.application` / `Rails.logger` /
    // `Rails.cache` / `Rails.env` / `Rails.root` are pervasive
    // call shapes in real Rails code. Each maps to a runtime
    // object that's not modeled structurally here; return
    // `Ty::Untyped` (gradual escape) so method chains off them
    // propagate through dispatch without bottoming out at Var.
    // `Rails.env` is the one we can type concretely as Str.
    let mut rails_cls = ClassInfo::default();
    // `Rails.app` is railties' alias of `application`.
    // `Rails.autoloaders` is runtime/spinel/rails_autoloaders.rb's, gated
    // (`analyze::runtime_reads`) to the trees that carry it.
    // `Rails.cache=` is railties' `attr_writer :cache` (runtime/ruby/rails.rb
    // answers it); Core's test memcached helper swaps the store with it.
    for m in ["application", "app", "logger", "cache", "cache=", "configuration", "autoloaders", "root", "gem_version", "public_path"] {
        rails_cls.class_methods.insert(Symbol::from(m), Ty::Untyped);
    }
    // `Rails.env` is an ActiveSupport::StringInquirer (a String
    // that also answers `development?`/`production?`/… as Bool),
    // not a plain Str — see the StringInquirer dispatch in send.rs.
    rails_cls.class_methods.insert(
        Symbol::from("env"),
        Ty::Class {
            id: ClassId(Symbol::from("ActiveSupport::StringInquirer")),
            args: vec![].into(),
        },
    );
    // `Rails.version` (railties gem_version.rb): "8.2.0.alpha".
    rails_cls.class_methods.insert(Symbol::from("version"), Ty::Str);
    // `Rails.event` / `Rails.error` — ActiveSupport's EventReporter and
    // ErrorReporter, answered by runtime/ruby/active_support_reporters.rb.
    // Registered here because a runtime `.rbs` does not feed app analysis.
    // A block-taking method answers its block's value, which this table
    // cannot see: `Untyped`.
    let class = |name: &str| Ty::Class { id: ClassId(Symbol::from(name)), args: vec![].into() };
    let sym_hash = || Ty::Hash { key: std::sync::Arc::new(Ty::Sym), value: std::sync::Arc::new(Ty::Untyped) };
    let untyped_arr = || Ty::Array { elem: std::sync::Arc::new(Ty::Untyped) };
    let event_reporter = class("ActiveSupport::EventReporter");
    let error_reporter = class("ActiveSupport::ErrorReporter");
    rails_cls.class_methods.insert(Symbol::from("event"), event_reporter.clone());
    rails_cls.class_methods.insert(Symbol::from("error"), error_reporter.clone());
    classes.insert(ClassId(Symbol::from("Rails")), rails_cls);
    register_stdlib_class(classes, "ActiveSupport", &[
        ("event_reporter", event_reporter.clone()), ("error_reporter", error_reporter.clone()),
        ("event_reporter=", event_reporter.clone()), ("error_reporter=", error_reporter.clone()),
        ("filter_parameters", untyped_arr()), ("filter_parameters=", untyped_arr()),
    ], &[]);
    register_stdlib_class(classes, "ActiveSupport::EventReporter", &[
        ("new", event_reporter.clone()),
        ("context_store", Ty::Untyped), ("context_store=", Ty::Untyped),
    ], &[
        ("notify", Ty::Nil), ("debug", Ty::Nil),
        ("tagged", Ty::Untyped), ("with_debug", Ty::Untyped),
        ("set_context", sym_hash()), ("clear_context", sym_hash()), ("context", sym_hash()),
        ("subscribe", untyped_arr()), ("unsubscribe", untyped_arr()),
        ("subscribers", untyped_arr()), ("debug_mode?", Ty::Bool),
        ("debug_mode=", Ty::Bool), ("raise_on_error=", Ty::Bool),
        ("reload_payload_filter", Ty::Untyped),
    ]);
    register_stdlib_class(classes, "ActiveSupport::ErrorReporter", &[
        ("new", error_reporter.clone()),
    ], &[
        ("report", Ty::Nil), ("unexpected", Ty::Nil),
        ("handle", Ty::Untyped), ("record", Ty::Untyped), ("disable", Ty::Untyped),
        ("set_context", Ty::Untyped),
        ("subscribe", untyped_arr()), ("unsubscribe", untyped_arr()),
        ("add_middleware", untyped_arr()),
        ("logger", Ty::Untyped), ("logger=", Ty::Untyped),
        ("debug_mode", Ty::Bool), ("debug_mode=", Ty::Bool),
    ]);
    register_stdlib_class(classes, "ActiveSupport::ErrorReporter::UnexpectedError", &[], &[
        ("message", Ty::Str), ("cause", Ty::Untyped), ("backtrace", Ty::Array { elem: std::sync::Arc::new(Ty::Str) }),
    ]);
    // activesupport's execution_context.rb: the per-thread context an
    // error report merges under its own `context:`.
    register_stdlib_class(classes, "ActiveSupport::ExecutionContext", &[
        ("set", Ty::Untyped), ("[]=", Ty::Untyped), ("to_h", sym_hash()),
        ("clear", Ty::Nil), ("push", Ty::Untyped), ("pop", Ty::Untyped), ("flush", Ty::Untyped),
        ("nestable", Ty::Bool), ("nestable=", Ty::Bool), ("after_change", Ty::Untyped),
    ], &[]);

    // `Rails.backtrace_cleaner` — railties' Rails::BacktraceCleaner over
    // ActiveSupport::BacktraceCleaner, answered by
    // runtime/spinel/active_support_backtrace_cleaner.rb (Rails
    // 8.2.0.alpha), which only the PostgreSQL CRuby tree carries
    // (`runtime_reads` refuses the read wherever the tree lacks it). A
    // backtrace is Strings or Thread::Backtrace::Locations, and `clean`
    // hands its input back whole under ENV["BACKTRACE"], so it is
    // `Array[untyped]`; `add_filter` / `add_silencer` answer the list.
    let str_or_nil = || Ty::Union { variants: vec![Ty::Str, Ty::Nil].into() };
    for (name, parent) in [
        ("ActiveSupport::BacktraceCleaner", None),
        ("Rails::BacktraceCleaner", Some("ActiveSupport::BacktraceCleaner")),
    ] {
        let cleaner = class(name);
        let mut class_methods = vec![("new", cleaner.clone())];
        if parent.is_some() {
            class_methods.push(("root", str_or_nil()));
        }
        register_stdlib_class(classes, name, &class_methods, &[
            ("clean", untyped_arr()), ("filter", untyped_arr()),
            ("clean_locations", untyped_arr()), ("clean_frame", str_or_nil()),
            ("first_clean_frame", str_or_nil()), ("first_clean_location", Ty::Untyped),
            ("add_filter", untyped_arr()), ("add_silencer", untyped_arr()),
            ("remove_silencers!", untyped_arr()), ("remove_filters!", untyped_arr()),
            ("dup", cleaner.clone()), ("clone", cleaner.clone()), ("freeze", cleaner.clone()),
        ]);
        if let Some(parent) = parent {
            if let Some(info) = classes.get_mut(&ClassId(Symbol::from(name))) {
                info.parent.get_or_insert_with(|| ClassId(Symbol::from(parent)));
            }
        }
    }
    register_stdlib_class(classes, "Rails", &[
        ("backtrace_cleaner", class("Rails::BacktraceCleaner")),
    ], &[]);
    // ActiveSupport::Deprecation and its Deprecators collection,
    // `ActiveSupport.deprecator` and `Rails.deprecator`, answered by
    // runtime/spinel/active_support_deprecation.rb (Rails 8.2.0.alpha),
    // which only the PostgreSQL CRuby tree carries (`runtime_reads`
    // refuses the read elsewhere). The instance methods come from the
    // Behavior/Reporting/Disallowed modules the class includes, which
    // the runtime index does not follow. `warn` answers the full message
    // or nil when silenced; a block-taking method answers its block.
    let deprecation = class("ActiveSupport::Deprecation");
    let deprecators = class("ActiveSupport::Deprecation::Deprecators");
    register_stdlib_class(classes, "ActiveSupport::Deprecation", &[
        ("new", deprecation.clone()), ("_instance", deprecation.clone()),
    ], &[
        ("warn", Ty::Union { variants: vec![Ty::Str, Ty::Nil].into() }),
        ("deprecation_warning", Ty::Str),
        ("silence", Ty::Untyped), ("allow", Ty::Untyped),
        ("begin_silence", Ty::Int), ("end_silence", Ty::Int),
        ("silenced", Ty::Untyped), ("silenced=", Ty::Untyped),
        ("behavior", untyped_arr()), ("behavior=", Ty::Untyped),
        ("disallowed_behavior", untyped_arr()), ("disallowed_behavior=", Ty::Untyped),
        ("disallowed_warnings", Ty::Untyped), ("disallowed_warnings=", Ty::Untyped),
        ("debug", Ty::Untyped), ("debug=", Ty::Untyped),
        ("gem_name", Ty::Untyped), ("gem_name=", Ty::Untyped),
        ("deprecation_horizon", Ty::Untyped), ("deprecation_horizon=", Ty::Untyped),
    ]);
    register_stdlib_class(classes, "ActiveSupport::Deprecation::Deprecators", &[
        ("new", deprecators.clone()),
    ], &[
        ("[]", Ty::Union { variants: vec![deprecation.clone(), Ty::Nil].into() }), ("[]=", deprecation.clone()),
        ("each", Ty::Untyped), ("silence", Ty::Untyped),
        ("silenced=", Ty::Untyped), ("debug=", Ty::Untyped), ("behavior=", Ty::Untyped),
        ("disallowed_behavior=", Ty::Untyped), ("disallowed_warnings=", Ty::Untyped),
    ]);
    register_stdlib_class(classes, "Rails", &[("deprecator", deprecation.clone())], &[]);
    register_stdlib_class(classes, "ActiveSupport", &[("deprecator", deprecation.clone())], &[]);
    // `ActiveRecord.version` / `ActiveSupport.version` and their
    // `gem_version` (activerecord and activesupport gem_version.rb and
    // version.rb, Rails 8.2.0.alpha): a Gem::Version, which compares
    // with a version string through its Comparable `<=>`. Answered by
    // runtime/ruby/active_record.rb and runtime/ruby/active_support_ext.rb.
    // softer_delete's module body branches on
    // `ActiveRecord.version >= "7.2.0.alpha"`.
    let gem_version = class("Gem::Version");
    for framework in ["ActiveRecord", "ActiveSupport"] {
        register_stdlib_class(classes, framework, &[
            ("version", gem_version.clone()), ("gem_version", gem_version.clone()),
        ], &[]);
    }
    // The orderings Gem::Version's `include Comparable` gives it, which
    // the vendored RBS declares but dispatch does not follow, and the
    // readers spinel's builtin Gem::Version shares with RubyGems'.
    // RubyGems' `<=>` (and spinel's) takes a version string as well as a
    // Version, and answers nil for an operand it cannot compare.
    register_stdlib_class(classes, "Gem::Version", &[], &[
        ("<=>", Ty::Union { variants: vec![Ty::Int, Ty::Nil].into() }),
        ("<", Ty::Bool), ("<=", Ty::Bool), (">", Ty::Bool), (">=", Ty::Bool),
        ("between?", Ty::Bool), ("prerelease?", Ty::Bool), ("version", Ty::Str),
        ("segments", Ty::Array { elem: std::sync::Arc::new(Ty::Union { variants: vec![Ty::Int, Ty::Str].into() }) }),
    ]);
    // ActiveSupport::Inflector and its Inflections, answered by
    // runtime/spinel/active_support_inflector.rb (Rails 8.2.0.alpha),
    // which only the PostgreSQL CRuby tree carries (`runtime_reads`
    // refuses the read elsewhere). The module `extend self`s, which the
    // runtime index does not follow, so its methods answer on both sides.
    // `inflections` with a block answers the block's value.
    let inflections = class("ActiveSupport::Inflector::Inflections");
    let uncountables = class("ActiveSupport::Inflector::Inflections::Uncountables");
    let inflector: Vec<(&str, Ty)> = [
        "pluralize", "singularize", "camelize", "underscore", "humanize", "upcase_first",
        "downcase_first", "titleize", "tableize", "classify", "dasherize", "demodulize",
        "deconstantize", "foreign_key", "parameterize", "transliterate", "ordinal", "ordinalize",
    ]
    .into_iter()
    .map(|m| (m, Ty::Str))
    .chain([("constantize", Ty::Untyped), ("safe_constantize", Ty::Untyped), ("inflections", Ty::Untyped)])
    .collect();
    register_stdlib_class(classes, "ActiveSupport::Inflector", &inflector, &inflector);
    register_stdlib_class(classes, "ActiveSupport::Inflector::Inflections", &[
        ("new", inflections.clone()), ("instance", inflections.clone()),
        ("instance_or_fallback", inflections.clone()), ("all_instances", untyped_arr()),
    ], &[
        ("plurals", untyped_arr()), ("singulars", untyped_arr()), ("humans", untyped_arr()),
        ("uncountables", uncountables.clone()),
        ("acronyms", Ty::Hash { key: std::sync::Arc::new(Ty::Str), value: std::sync::Arc::new(Ty::Str) }),
        ("acronym", Ty::Untyped), ("plural", Ty::Untyped), ("singular", Ty::Untyped),
        ("irregular", Ty::Untyped), ("uncountable", Ty::Untyped), ("human", Ty::Untyped),
        ("clear", Ty::Untyped),
    ]);
    register_stdlib_class(classes, "ActiveSupport::Inflector::Inflections::Uncountables", &[
        ("new", uncountables.clone()),
    ], &[
        ("uncountable?", Ty::Bool), ("to_a", Ty::Array { elem: std::sync::Arc::new(Ty::Str) }),
        ("<<", uncountables.clone()), ("add", uncountables.clone()), ("delete", Ty::Untyped),
        ("empty?", Ty::Bool), ("each", Ty::Untyped),
    ]);
    // ActionDispatch::Http::URL's module functions and the Journey
    // escapers they reach, answered by runtime/spinel/
    // action_dispatch_http_url.rb (8.2.0.alpha actionpack), which the
    // Ruby-family CRuby trees carry where the app names the module
    // (`runtime_reads` refuses the read elsewhere). The module defines
    // them in `class << self`, which the runtime index does not follow.
    let str_arr = Ty::Array { elem: std::sync::Arc::new(Ty::Str) };
    register_stdlib_class(classes, "ActionDispatch::Http::URL", &[
        ("url_for", Ty::Str), ("full_url_for", Ty::Str), ("path_for", Ty::Str),
        ("extract_domain", Ty::Union { variants: vec![Ty::Str, Ty::Nil].into() }),
        ("extract_subdomains", str_arr.clone()), ("extract_subdomain", Ty::Str),
        ("secure_protocol", Ty::Bool), ("secure_protocol=", Ty::Bool),
        ("tld_length", Ty::Int), ("tld_length=", Ty::Int),
        ("domain_extractor", Ty::Untyped), ("domain_extractor=", Ty::Untyped),
    ], &[]);
    register_stdlib_class(classes, "ActionDispatch::Journey::Router::Utils", &[
        ("escape_path", Ty::Str), ("escape_segment", Ty::Str), ("escape_fragment", Ty::Str),
    ], &[]);
    // activerecord log_subscriber.rb:9, `class_attribute :backtrace_cleaner`
    // (runtime/spinel/active_record_log_subscriber.rb, PostgreSQL tree only).
    let as_cleaner = class("ActiveSupport::BacktraceCleaner");
    register_stdlib_class(classes, "ActiveRecord::LogSubscriber", &[
        ("backtrace_cleaner", as_cleaner.clone()), ("backtrace_cleaner=", as_cleaner.clone()),
        ("backtrace_cleaner?", Ty::Bool),
    ], &[
        ("backtrace_cleaner", as_cleaner.clone()), ("backtrace_cleaner=", as_cleaner.clone()),
        ("backtrace_cleaner?", Ty::Bool),
    ]);
    // activerecord query_logs.rb, the tags API an app configures from an
    // initializer (runtime/spinel/active_record_query_logs.rb, PostgreSQL
    // tree only).
    register_stdlib_class(classes, "ActiveRecord::QueryLogs", &[
        ("tags", Ty::Untyped), ("tags=", Ty::Untyped),
        ("taggings", Ty::Untyped), ("taggings=", Ty::Untyped),
        ("tags_formatter", Ty::Untyped), ("tags_formatter=", Ty::Untyped),
        ("prepend_comment", Ty::Untyped), ("prepend_comment=", Ty::Untyped),
        ("cache_query_log_tags", Ty::Untyped), ("cache_query_log_tags=", Ty::Untyped),
        ("call", Ty::Str), ("clear_cache", Ty::Untyped), ("query_source_location", Ty::Untyped),
    ], &[]);
    // GlobalID mint + Locator — unsigned `param`/`uri`/`signed` and the
    // locate / locate_signed class methods. Return types for locate*
    // with a literal `only:` are refined in `body/send.rs` to the named
    // model (nilable); Untyped here is the gradual fallback for a
    // computed `only:`.
    {
        let mut gid = ClassInfo::default();
        for m in ["param", "uri", "signed"] {
            gid.class_methods.insert(Symbol::from(m), Ty::Str);
        }
        classes.insert(ClassId(Symbol::from("GlobalID")), gid);
        let mut locator = ClassInfo::default();
        locator
            .class_methods
            .insert(Symbol::from("locate"), Ty::Untyped);
        locator
            .class_methods
            .insert(Symbol::from("locate_signed"), Ty::Untyped);
        classes.insert(ClassId(Symbol::from("GlobalID::Locator")), locator);
    }

    // `ActionController::BrowserBlocker.blocked?(user_agent, floors)` —
    // the gate `ingest::allow_browser` synthesizes into a controller
    // body for `allow_browser`, answered by
    // runtime/ruby/action_controller/browser_blocker.rb. Registered
    // here because a runtime `.rbs` does not feed app analysis, and an
    // unresolved send in a controller body is a dispatch error, not a
    // gradual escape.
    let mut blocker = ClassInfo::default();
    blocker.class_methods.insert(Symbol::from("blocked?"), Ty::Bool);
    classes.insert(ClassId(Symbol::from("ActionController::BrowserBlocker")), blocker);

    // `ActionController::RateLimiter.exceeded?(key, within, to)` — the
    // counter behind `rate_limit`, called from the filter method
    // `ingest::rate_limit` synthesizes; runtime/ruby/action_controller/
    // rate_limiter.rb answers it. Registered for the same reason.
    let mut limiter = ClassInfo::default();
    limiter.class_methods.insert(Symbol::from("exceeded?"), Ty::Bool);
    classes.insert(ClassId(Symbol::from("ActionController::RateLimiter")), limiter);

    // `SorbetRuntimeCheck.cast(value, type, "T.let", path, line)` — the
    // check `ingest::sorbet_check` lowers `T.let` / `T.cast` / `T.bind` /
    // `T.assert_type!` to, and the type-object builders its type
    // argument is made of; the CRuby overlay's sorbet_runtime_check.rb
    // answers them, and `type_alias { ... }`, a carried sorbet type alias
    // (`sorbet_check::alias_value`). The call's value is the source
    // `Cast`'s to type.
    let mut sorbet_check = ClassInfo::default();
    for method in ["cast", "nilable", "any", "all", "proc_type", "void", "generic", "class_of", "fixed_hash", "fixed_array", "untyped", "anything", "boolean", "type_alias", "mixes_in_class_methods"] {
        sorbet_check.class_methods.insert(Symbol::from(method), Ty::Untyped);
    }
    classes.insert(ClassId(Symbol::from(crate::ingest::sorbet_check::HELPER)), sorbet_check);
    // `ActionController::InvisibleCaptcha.spam?(params)` — the honeypot
    // gate `ingest::invisible_captcha` synthesizes into a controller
    // body; runtime/ruby/action_controller/invisible_captcha.rb answers
    // it. Registered for the same reason as RateLimiter / BrowserBlocker.
    let mut captcha = ClassInfo::default();
    captcha.class_methods.insert(Symbol::from("spam?"), Ty::Bool);
    classes.insert(ClassId(Symbol::from("ActionController::InvisibleCaptcha")), captcha);

    // Time singleton — `Time.now` (Ruby core) / `Time.current`
    // (Rails) / `Time.at` all yield a Time *value*, and `Time.zone`
    // is a TimeZone whose `.now`/`.at`/`.local` likewise yield Time,
    // so modeling it as Time too lets those chains resolve. Time
    // values are already modeled structurally (`time_method` in
    // send.rs) and AR datetime columns type as Time, so these
    // constructors join that same surface — `Time.now.to_i` → Int,
    // `Time.current.utc` → Time — instead of bottoming out at the
    // `Untyped` gradual escape (and dragging every chained call into
    // it). `Time - x` arithmetic still resolves to `Untyped` inside
    // `time_method` because receiver-only dispatch can't tell a
    // Duration arg (→ Time) from a Time arg (→ Float).
    let time_ty = || Ty::Class {
        id: ClassId(Symbol::from("Time")),
        args: vec![].into(),
    };
    let mut time_cls = ClassInfo::default();
    time_cls.class_methods.insert(Symbol::from("current"), time_ty());
    time_cls.class_methods.insert(Symbol::from("now"), time_ty());
    time_cls.class_methods.insert(Symbol::from("zone"), time_ty());
    time_cls.class_methods.insert(Symbol::from("at"), time_ty());
    classes.insert(ClassId(Symbol::from("Time")), time_cls);

    // The seconds-based Duration value class the Ruby-family trees ship
    // (`runtime/spinel/active_support_duration.rb`, its `.rbs`, and the
    // CRuby overlay twin), which `lower::duration` grounds `30.minutes`
    // to. Its surface is what both files define, so a declared
    // `#: ActiveSupport::Duration` names a modeled dependency. `project`
    // refuses the value on trees that ship no such class.
    let duration_ty = || Ty::Class { id: ClassId(Symbol::from("ActiveSupport::Duration")), args: vec![].into() };
    let units = [
        "seconds", "second", "minutes", "minute", "hours", "hour", "days", "day", "weeks", "week",
        "fortnights", "fortnight", "months", "month", "years", "year",
    ];
    let class_side: Vec<(&str, Ty)> = units.iter().map(|u| (*u, duration_ty())).collect();
    register_stdlib_class(classes, "ActiveSupport::Duration", &class_side, &[
        ("ago", time_ty()), ("until", time_ty()), ("from_now", time_ty()), ("since", time_ty()),
        ("before", time_ty()), ("after", time_ty()), ("eql?", Ty::Bool), ("hash", Ty::Int),
        ("to_i", Ty::Int), ("seconds", Ty::Int), ("to_f", Ty::Float), ("to_s", Ty::Str),
        ("in_seconds", Ty::Int), ("in_minutes", Ty::Float), ("in_hours", Ty::Float),
        ("in_days", Ty::Float), ("in_weeks", Ty::Float), ("in_months", Ty::Float),
        ("in_years", Ty::Float),
        ("==", Ty::Bool), ("<", Ty::Bool), ("<=", Ty::Bool), (">", Ty::Bool), (">=", Ty::Bool),
        // Integer `<=>` answers nil for an incomparable operand; Duration
        // forwards there after unwrapping another Duration.
        ("<=>", Ty::Union { variants: vec![Ty::Int, Ty::Nil].into() }),
    ]);

    // Date instances are date-only values, including constructors and
    // parsers. The send dispatcher validates their argument contracts;
    // do not duplicate unvalidated return declarations here. DateTime
    // keeps its existing timestamp representation.
    classes.insert(ClassId(Symbol::from("Date")), ClassInfo::default());
    for name in ["DateTime"] {
        let mut cls = ClassInfo::default();
        cls.class_methods.insert(Symbol::from("current"), Ty::Untyped);
        cls.class_methods.insert(Symbol::from("today"), Ty::Untyped);
        cls.class_methods.insert(Symbol::from("now"), Ty::Untyped);
        // The parse family has a concrete answer where `current` /
        // `today` / `now` above do not yet: `Ty::Time` is roundhouse's
        // timestamp type. Upgrading the three older
        // entries would move types in every app that calls them, so it
        // is deliberately not part of this change.
        for parser in ["parse", "strptime", "iso8601", "civil"] {
            cls.class_methods.insert(Symbol::from(parser), Ty::Time);
        }
        classes.insert(ClassId(Symbol::from(name)), cls);
    }

    // Ruby stdlib singletons + Set — referenced by ~every Rails app but
    // not structurally modeled. Register the common call surface so
    // `File.read`, `SecureRandom.hex`, `CGI.escape`, `Set#<<` resolve to
    // a return type instead of "no known method". Return types follow
    // the official rbs gem core/stdlib signatures, narrowed to the
    // concrete cases; opaque/handle returns (`File.open`, `URI.parse`)
    // and unparameterized collection elements degrade to `Untyped` (the
    // gradual escape) so chained calls still flow. Hardcoded like the
    // Rails/Time/Date blocks above — `register_stdlib_class` never
    // clobbers an app-defined method/class of the same name.
    let str_arr = || Ty::Array { elem: std::sync::Arc::new(Ty::Str) };
    register_stdlib_class(classes, "SecureRandom", &[
        ("hex", Ty::Str), ("base64", Ty::Str), ("urlsafe_base64", Ty::Str),
        ("base58", Ty::Str), ("uuid", Ty::Str), ("alphanumeric", Ty::Str),
        ("random_bytes", Ty::Str), ("random_number", Ty::Untyped),
    ], &[]);
    // `Random` carries the same `Random::Formatter` surface as
    // SecureRandom since Ruby 3.3 (`Random.uuid` is campfire's test
    // notification body); `rand`/`random_number` answer Int or Float
    // by argument, so they stay gradual.
    register_stdlib_class(classes, "Random", &[
        ("uuid", Ty::Str), ("hex", Ty::Str), ("base64", Ty::Str),
        ("urlsafe_base64", Ty::Str), ("alphanumeric", Ty::Str),
        ("bytes", Ty::Str), ("random_bytes", Ty::Str),
        ("rand", Ty::Untyped), ("random_number", Ty::Untyped),
        ("new_seed", Ty::Int), ("seed", Ty::Int),
    ], &[
        ("rand", Ty::Untyped), ("bytes", Ty::Str), ("seed", Ty::Int),
    ]);
    // `OpenSSL::Random.random_bytes(n)` — lobsters' `Utils.random_str`.
    // The openssl library's own on the ruby family; on spinel the
    // façade alias in `runtime/spinel/facades/sponge.rb`.
    register_stdlib_class(classes, "OpenSSL::Random", &[("random_bytes", Ty::Str)], &[]);
    // `Turbo::StreamsChannel` — turbo-rails' CLASS-side broadcast API,
    // the one an app reaches for when the payload is not a record's
    // partial: a rendered component, a counter, a status panel. The
    // record-side surface is registered on every model (it is mixed
    // into `ActiveRecord::Base`); this is the same five actions with
    // the stream, target and markup passed explicitly. They answer the
    // broadcast, which nothing consumes: `Nil`.
    register_stdlib_class(classes, "Turbo::StreamsChannel", &[
        ("broadcast_append_to", Ty::Nil), ("broadcast_prepend_to", Ty::Nil),
        ("broadcast_replace_to", Ty::Nil), ("broadcast_update_to", Ty::Nil),
        ("broadcast_remove_to", Ty::Nil),
    ], &[]);
    // `ENV` — a Rails app's configuration seam, and the one stdlib
    // object whose values have a single concrete type: every entry is
    // a String. `fetch` narrows to that, which is right for the two
    // forms an app writes (`fetch(key)` and `fetch(key, "default")`)
    // and a narrowing for the rarer `fetch(key, 3000)` / block forms —
    // the same trade the neighbours above make.
    let str_or_nil = || Ty::Union { variants: vec![Ty::Str, Ty::Nil].into() };
    register_stdlib_class(classes, "ENV", &[
        ("fetch", Ty::Str), ("[]", str_or_nil()), ("[]=", Ty::Str),
        ("key?", Ty::Bool), ("has_key?", Ty::Bool), ("include?", Ty::Bool),
        ("member?", Ty::Bool), ("key", str_or_nil()), ("delete", str_or_nil()),
        ("keys", str_arr()), ("values", str_arr()),
        ("to_h", Ty::Hash { key: std::sync::Arc::new(Ty::Str), value: std::sync::Arc::new(Ty::Str) }),
        ("to_hash", Ty::Hash { key: std::sync::Arc::new(Ty::Str), value: std::sync::Arc::new(Ty::Str) }),
        ("replace", Ty::Hash { key: std::sync::Arc::new(Ty::Str), value: std::sync::Arc::new(Ty::Str) }),
    ], &[]);
    // `Pathname` — `Rails.root.join(...)` territory. The path-returning
    // methods answer a Pathname so a chain stays typed to its `to_s`.
    let pathname = || Ty::Class { id: ClassId(Symbol::from("Pathname")), args: vec![].into() };
    register_stdlib_class(classes, "Pathname", &[
        ("new", pathname()), ("pwd", pathname()), ("getwd", pathname()),
    ], &[
        ("relative_path_from", pathname()), ("join", pathname()),
        ("basename", pathname()), ("dirname", pathname()), ("expand_path", pathname()),
        ("parent", pathname()), ("realpath", pathname()),
        ("to_s", Ty::Str), ("to_path", Ty::Str), ("extname", Ty::Str),
        ("read", Ty::Str), ("write", Ty::Int),
        ("exist?", Ty::Bool), ("file?", Ty::Bool), ("directory?", Ty::Bool),
        ("absolute?", Ty::Bool), ("relative?", Ty::Bool),
        ("children", Ty::Array { elem: std::sync::Arc::new(pathname()) }),
    ]);
    // `ActiveSupport::SecurityUtils.secure_compare` — the constant-time
    // comparison every hand-rolled token or basic-auth check calls.
    register_stdlib_class(classes, "ActiveSupport::SecurityUtils", &[
        ("secure_compare", Ty::Bool), ("fixed_length_secure_compare", Ty::Bool),
    ], &[]);
    register_stdlib_class(classes, "File", &[
        ("read", Ty::Str), ("binread", Ty::Str), ("write", Ty::Int), ("binwrite", Ty::Int),
        ("exist?", Ty::Bool), ("exists?", Ty::Bool), ("file?", Ty::Bool),
        ("directory?", Ty::Bool), ("open", Ty::Untyped),
        ("unlink", Ty::Int), ("delete", Ty::Int), ("rename", Ty::Int),
        ("join", Ty::Str), ("basename", Ty::Str), ("dirname", Ty::Str),
        ("extname", Ty::Str), ("expand_path", Ty::Str), ("size", Ty::Int),
        // CRuby's `File` / `FileTest` type and permission predicates.
        ("socket?", Ty::Bool), ("symlink?", Ty::Bool), ("pipe?", Ty::Bool),
        ("readable?", Ty::Bool), ("writable?", Ty::Bool), ("executable?", Ty::Bool),
        ("zero?", Ty::Bool), ("empty?", Ty::Bool), ("absolute_path?", Ty::Bool),
        ("absolute_path", Ty::Str), ("realpath", Ty::Str),
    ], &[]);
    register_stdlib_class(classes, "Dir", &[
        ("entries", str_arr()), ("glob", str_arr()), ("[]", str_arr()),
        ("exist?", Ty::Bool), ("exists?", Ty::Bool), ("mkdir", Ty::Int),
        ("pwd", Ty::Str), ("home", Ty::Str),
    ], &[]);
    // Psych/YAML — campfire's Purchaser loads `config/purchased_by.yml`
    // via `YAML.load_file`. Register the class-side surface the corpus
    // writes so the constant is modeled (not a silent raise stub) and
    // ruby-family emit keeps the call. Return types stay gradual: the
    // document shape is whatever the file held.
    register_stdlib_class(classes, "YAML", &[
        ("load", Ty::Untyped), ("load_file", Ty::Untyped),
        ("safe_load", Ty::Untyped), ("safe_load_file", Ty::Untyped),
        ("dump", Ty::Str),
    ], &[]);
    // Framework modules an app `include`s that emit already handles
    // (Attachable sgid, Turbo stream names, ActiveModel callbacks,
    // SanitizeHelper). Empty module markers so the unresolved-include
    // gate does not refuse known seams; methods come from lowering /
    // other registry entries.
    for name in [
        "ActionText::Attachable",
        "Turbo::Streams::StreamName",
        "Turbo::Streams::StreamName::ClassMethods",
        "ActiveModel::Validations::Callbacks",
        "ActionView::Helpers::SanitizeHelper",
    ] {
        let info = classes.entry(ClassId(Symbol::from(name))).or_default();
        info.is_module = true;
    }
    // SanitizeHelper readers Opengraph::Metadata calls after include.
    if let Some(sanitize) = classes.get_mut(&ClassId(Symbol::from("ActionView::Helpers::SanitizeHelper"))) {
        for m in ["sanitize", "strip_tags", "sanitize_css"] {
            sanitize.instance_methods.entry(Symbol::from(m)).or_insert(Ty::Str);
        }
    }
    register_stdlib_class(classes, "Math", &[
        ("sqrt", Ty::Float), ("cbrt", Ty::Float), ("log", Ty::Float),
        ("log2", Ty::Float), ("log10", Ty::Float), ("exp", Ty::Float),
        ("sin", Ty::Float), ("cos", Ty::Float), ("tan", Ty::Float),
        ("atan", Ty::Float), ("atan2", Ty::Float), ("hypot", Ty::Float),
        ("pow", Ty::Float),
    ], &[]);
    register_stdlib_class(classes, "CGI", &[
        ("escape", Ty::Str), ("unescape", Ty::Str),
        ("escapeHTML", Ty::Str), ("unescapeHTML", Ty::Str),
        ("escape_html", Ty::Str), ("unescape_html", Ty::Str),
    ], &[]);
    register_stdlib_class(classes, "ERB::Util", &[
        ("html_escape", Ty::Str), ("h", Ty::Str),
        ("url_encode", Ty::Str), ("u", Ty::Str), ("json_escape", Ty::Str),
    ], &[]);
    for digest in ["Digest::MD5", "Digest::SHA1", "Digest::SHA256"] {
        register_stdlib_class(classes, digest, &[
            ("hexdigest", Ty::Str), ("digest", Ty::Str),
            ("base64digest", Ty::Str),
        ], &[]);
    }
    // `useragent` + `platform_agent`, now PORTED into
    // `runtime/ruby/user_agent.rb` rather than stubbed. Method lists are
    // that file's `.rbs` verbatim, so the analyzer and a strict target
    // agree; `PlatformAgent` is here because `ApplicationPlatform <
    // PlatformAgent` names it at LOAD time, and without the analyzer
    // knowing the pair campfire's `UserAgent.parse(ua).browser` in a
    // partial read out as `no known method parse on Class {
    // UserAgent }`.
    let user_agent_ty = Ty::Class { id: ClassId(Symbol::from("UserAgent")), args: vec![].into() };
    register_stdlib_class(classes, "UserAgent", &[
        ("parse", user_agent_ty.clone()),
    ], &[
        ("browser", Ty::Str), ("platform", Ty::Str), ("version", Ty::Str),
        ("os", Ty::Str),
    ]);
    register_stdlib_class(classes, "PlatformAgent", &[], &[
        ("browser", Ty::Str), ("version", Ty::Str), ("os", Ty::Str),
        ("match?", Ty::Bool), ("user_agent", user_agent_ty),
    ]);
    // useragent 0.16.11's `UserAgent::Version`, the gem's own file
    // (runtime/spinel/user_agent_version.rb), which only the PostgreSQL
    // CRuby tree carries. Shopify Core's secure_headers gem source builds
    // three at class-body load (`VERSION_46 = ::UserAgent::Version.new("46")`)
    // and compares versions with `>=`. The surface is that file's methods
    // and the orderings its `include ::Comparable` gives it: `<=>` answers
    // nil for an operand it cannot coerce, and `to_a` the segments (Integer
    // runs and a trailing alphabetic tag). Registered on every target so
    // the read types, like the type port's classes; `runtime_reads` gates
    // each read on the finished tree carrying the file, so every other
    // tree refuses it.
    let version_ty = Ty::Class { id: ClassId(Symbol::from("UserAgent::Version")), args: vec![].into() };
    register_stdlib_class(classes, "UserAgent::Version", &[("new", version_ty.clone())], &[
        ("<=>", Ty::Union { variants: vec![Ty::Int, Ty::Nil].into() }),
        ("==", Ty::Bool), ("eql?", Ty::Bool), ("nil?", Ty::Bool),
        ("<", Ty::Bool), ("<=", Ty::Bool), (">", Ty::Bool), (">=", Ty::Bool),
        ("between?", Ty::Bool), ("clamp", version_ty),
        ("to_a", Ty::Array { elem: std::sync::Arc::new(Ty::Union { variants: vec![Ty::Int, Ty::Str].into() }) }),
        ("to_str", Ty::Str), ("to_s", Ty::Str), ("inspect", Ty::Str),
    ]);
    // `typeid`, PORTED into `runtime/ruby/typeid.rb` as a module whose
    // `new` answers a plain String (the gem's `TypeID < String` is a
    // builtin subclass the strict targets do not carry). Registered so
    // lobsters' Token concern — `self.token ||= TypeID.new(prefix)` —
    // types its token as the String column it is written into.
    register_stdlib_class(classes, "TypeID", &[("new", Ty::Str)], &[]);
    // `ActionText::ContentHelper.allowed_attributes`, from
    // `runtime/ruby/action_text.rbs` — the one façade reader campfire's
    // `ContentFilters::SanitizeAttributes` needs typed:
    //
    //     ActionText::ContentHelper.allowed_attributes ||
    //       (sanitizer_class.allowed_attributes +
    //        ActionText::Attachment::ATTRIBUTES).to_a
    //
    // The right arm goes through a CLASS OBJECT
    // (`ContentHelper.sanitizer.class`), and neither we nor spinel can
    // dispatch statically on one — our `Ty` has no singleton and
    // spinel's `sp_Class` is dynamic. So the right arm is
    // `Array[untyped]` whatever we do here, and the result was handed
    // to a `sanitize` this same file declares takes `Array[String]`.
    // Two descriptions of one list, contradicting, and on spinel the
    // contradiction surfaces as far from its cause as it can:
    // `incompatible pointer types passing 'sp_PolyArray *' to parameter
    // of type 'sp_StrArray *'`, from the C compiler, with the campfire
    // binary failing to LINK.
    //
    // Typing the LEFT arm is what closes it, because `||` now folds to
    // a left that cannot be falsy — see `analyze::body`. The runtime
    // answers Rails' computed list rather than Rails' unconfigured
    // `nil`; that divergence is in `docs/pipeline/runtime.md`, and it
    // is why this entry is here rather than the whole module.
    //
    // `runtime/ruby/**/*.rbs` does not feed app analysis — only the
    // runtime-sweep test builds a registry from it — so a façade the
    // app calls has to be named here to be known.
    register_stdlib_class(classes, "ActionText::ContentHelper", &[
        ("allowed_attributes", Ty::Array { elem: std::sync::Arc::new(Ty::Str) }),
    ], &[]);
    // `IPAddr` — the class `runtime/ruby/ipaddr.rb` ports. Only the
    // surface that file implements is registered, so a call beyond it
    // stays an honest gap rather than a method that types and then
    // raises. `IPAddr.new(x)` reaches the universal `.new`, so the
    // predicates are looked up as INSTANCE methods on the result.
    // `Regexp` and `MatchData` — Ruby's own, and the pair a PORT needs
    // most. `runtime/ruby/user_agent.rb` tokenizes with
    // `MATCHER.match(s)` and then reads `m[0]`, and with `match`
    // unregistered every read off `m` was gradual: 45 untyped
    // sub-expressions from one method, which is what the
    // `Ty::Untyped` ratchet in `tests/runtime_src_integration.rs` is
    // there to catch.
    //
    // ONLY the surface both lanes implement, the same rule IPAddr and
    // Net::HTTP follow. `MatchData#[]` answers `String?` because a
    // group that did not participate is nil, and pretending otherwise
    // would hand a strict target a non-null it has to trust.
    let match_data = Ty::Class { id: ClassId(Symbol::from("MatchData")), args: vec![].into() };
    let str_or_nil_m = Ty::Union { variants: vec![Ty::Str, Ty::Nil].into() };
    register_stdlib_class(classes, "Regexp", &[
        // Class side: escaping is a pure String -> String function, `last_match` reads the
        // `$~` of the previous match, `union` builds a Regexp.
        ("escape", Ty::Str),
        ("quote", Ty::Str),
        ("last_match", Ty::Union { variants: vec![match_data.clone(), Ty::Nil].into() }),
        ("union", Ty::Class { id: ClassId(Symbol::from("Regexp")), args: vec![].into() }),
    ], &[
        ("match", Ty::Union { variants: vec![match_data.clone(), Ty::Nil].into() }),
        ("match?", Ty::Bool),
        ("=~", Ty::Union { variants: vec![Ty::Int, Ty::Nil].into() }),
        ("===", Ty::Bool),
        ("source", Ty::Str),
    ]);
    register_stdlib_class(classes, "MatchData", &[], &[
        ("[]", str_or_nil_m.clone()),
        ("to_s", Ty::Str),
        ("pre_match", Ty::Str),
        ("post_match", Ty::Str),
        ("captures", Ty::Array { elem: std::sync::Arc::new(str_or_nil_m) }),
        ("size", Ty::Int),
        ("length", Ty::Int),
    ]);
    // `Zlib` — `crc32` is the whole ported surface (see
    // `runtime/ruby/zlib.rb`); registering only it keeps a call to
    // `Zlib.deflate` an honest gap instead of a method that types and
    // then fails to resolve.
    // `gzip` is Ruby's own on the ruby family and spinel's
    // `packages/zlib` on spinel, so it types here and a strict target,
    // whose port has no deflate, refuses the call by name
    // (`project::RUBY_SPINEL_ONLY_METHODS`).
    register_stdlib_class(classes, "Zlib", &[
        ("crc32", Ty::Int),
        ("gzip", Ty::Str),
    ], &[]);
    // `SQLite3::Database` — the sqlite3 gem on the ruby family, and
    // `runtime/spinel/sqlite3_database.rb` (the same surface over the FFI)
    // on spinel. ONLY that surface: campfire's `ResponseCache` keeps a
    // read-only observer (`new(path, readonly: true)`,
    // `get_first_value("PRAGMA data_version")`, `close`) and its WAL
    // checkpointer opens a block-form connection, sets
    // `busy_handler_timeout=` and `execute`s a pragma. A row is an Array
    // of column values.
    let sqlite_db = Ty::Class { id: ClassId(Symbol::from("SQLite3::Database")), args: vec![].into() };
    register_stdlib_class(classes, "SQLite3::Database", &[
        ("new", sqlite_db.clone()),
        ("open", sqlite_db.clone()),
    ], &[
        ("execute", Ty::Array { elem: std::sync::Arc::new(Ty::Array { elem: std::sync::Arc::new(Ty::Untyped) }) }),
        ("get_first_row", Ty::Union { variants: vec![Ty::Array { elem: std::sync::Arc::new(Ty::Untyped) }, Ty::Nil].into() }),
        ("get_first_value", Ty::Untyped),
        ("close", Ty::Nil),
        ("closed?", Ty::Bool),
        ("readonly?", Ty::Bool),
        ("busy_timeout=", Ty::Int),
        ("busy_handler_timeout=", Ty::Int),
    ]);
    // `ActiveSupport::Cache` — `expand_cache_key` and the bounded
    // `MemoryStore`, in runtime/spinel/active_support_cache.rb (the ruby
    // family and spinel). A cached value is whatever was written.
    register_stdlib_class(classes, "ActiveSupport::Cache", &[
        ("expand_cache_key", Ty::Str),
    ], &[]);
    let memory_store = Ty::Class { id: ClassId(Symbol::from("ActiveSupport::Cache::MemoryStore")), args: vec![].into() };
    register_stdlib_class(classes, "ActiveSupport::Cache::MemoryStore", &[
        ("new", memory_store),
    ], &[
        ("read", Ty::Untyped),
        ("write", Ty::Bool),
        ("fetch", Ty::Untyped),
        ("delete", Ty::Bool),
        ("exist?", Ty::Bool),
        ("clear", Ty::Untyped),
        ("cleanup", Ty::Nil),
        ("prune", Ty::Nil),
        ("read_multi", Ty::Hash { key: std::sync::Arc::new(Ty::Untyped), value: std::sync::Arc::new(Ty::Untyped) }),
        ("write_multi", Ty::Bool),
    ]);

    // `FileUtils` — a default gem / spinel's `packages/fileutils`.
    // `mkdir_p` answers the directories it was given; `cp` nil for one source
    // and the source Array for several (CRuby 4.0.5). Both checked against
    // CRuby and Spinel (tests/support/stdlib_rnp.rs).
    register_stdlib_class(classes, "FileUtils", &[
        ("mkdir_p", Ty::Array { elem: std::sync::Arc::new(Ty::Str) }), ("makedirs", Ty::Untyped),
        ("cp", Ty::Union { variants: vec![Ty::Nil, Ty::Array { elem: std::sync::Arc::new(Ty::Str) }].into() }),
        ("rm_rf", Ty::Untyped), ("rm_f", Ty::Untyped),
        ("remove_entry", Ty::Nil), ("touch", Ty::Untyped),
    ], &[]);
    // `ActiveSupport::JSON` — Rails' coder, in `runtime/ruby/
    // active_support_ext.rb` (the ruby family and spinel). `encode`
    // answers the document; `decode` whatever the document holds.
    register_stdlib_class(classes, "ActiveSupport::JSON", &[
        ("encode", Ty::Str),
        ("decode", Ty::Untyped),
    ], &[]);
    // `Rack::Utils` — the two encoding-negotiation functions, ported
    // into `runtime/ruby/rack_utils.rb` (the rack gem's own on the ruby
    // family). ONLY that surface, the IPAddr rule. A pair is
    // `[name, quality]`; the name is nil for an empty header part.
    let q_pair = Ty::Tuple {
        elems: vec![Ty::Union { variants: vec![Ty::Str, Ty::Nil].into() }, Ty::Float].into(),
    };
    register_stdlib_class(classes, "Rack::Utils", &[
        ("q_values", Ty::Array { elem: std::sync::Arc::new(q_pair) }),
        ("select_best_encoding", Ty::Union { variants: vec![Ty::Str, Ty::Nil].into() }),
    ], &[]);
    register_stdlib_class(classes, "IPAddr", &[], &[
        ("ipv4?", Ty::Bool), ("ipv6?", Ty::Bool), ("ipv4_mapped?", Ty::Bool),
        ("loopback?", Ty::Bool), ("private?", Ty::Bool), ("link_local?", Ty::Bool),
        ("to_s", Ty::Str),
        ("octets", Ty::Array { elem: std::sync::Arc::new(Ty::Int) }),
    ]);
    // `Surfguard` — basecamp/surfguard's SSRF address policy, ported
    // into `runtime/ruby/surfguard.rb`. Class methods only: the module
    // has no instances. Registered to the same rule as IPAddr and
    // Zlib — ONLY the surface the port implements, so a call to one of
    // the three URL-taking entry points the port declines
    // (`enforce_public_ip` and friends) stays an honest gap instead of
    // typing clean and resolving to nothing.
    register_stdlib_class(classes, "Surfguard", &[
        ("resolve_public_ips", Ty::Array { elem: std::sync::Arc::new(Ty::Str) }),
        ("blocked_address?", Ty::Bool),
    ], &[]);
    // `Concurrent` — concurrent-ruby's thread pools, barrier and Map, ported
    // into `runtime/spinel/concurrent.rb` (the ruby family runs the gem).
    // ONLY the surface the port implements, the IPAddr rule: campfire's
    // `WebPush::Pool` posts, shuts down, kills and waits on its two
    // pools, and its suite reads `completed_task_count`. `FixedThreadPool`
    // is the same surface — it IS a `ThreadPoolExecutor` in the gem and
    // in the port — registered under its own name because the universal
    // `.new` yields `Class { id: <name> }` and the registry has no
    // subclass walk for stdlib entries.
    for pool in ["Concurrent::ThreadPoolExecutor", "Concurrent::FixedThreadPool"] {
        register_stdlib_class(classes, pool, &[], &[
            ("post", Ty::Bool),
            ("shutdown", Ty::Nil),
            ("kill", Ty::Nil),
            ("wait_for_termination", Ty::Bool),
            ("completed_task_count", Ty::Int),
            ("length", Ty::Int),
            ("queue_length", Ty::Int),
            ("max_length", Ty::Int),
            ("running?", Ty::Bool),
            ("shutdown?", Ty::Bool),
        ]);
    }
    register_stdlib_class(classes, "Concurrent::CyclicBarrier", &[], &[
        ("wait", Ty::Bool),
        ("parties", Ty::Int),
        ("number_waiting", Ty::Int),
    ]);
    let concurrent_map = Ty::Class { id: ClassId(Symbol::from("Concurrent::Map")), args: vec![].into() };
    register_stdlib_class(classes, "Concurrent::Map", &[], &[
        ("[]", Ty::Untyped),
        ("[]=", Ty::Untyped),
        ("compute_if_absent", Ty::Untyped),
        ("clear", concurrent_map),
    ]);
    // `Net::HTTP` — a real client on BOTH lanes: CRuby's own stdlib, and
    // spinel's `packages/net` (HTTPS included, since the openssl package
    // landed). So there is nothing to port here, unlike IPAddr — only the
    // types, plus the `require "net/http"` that `project::BUNDLED` writes.
    //
    // ONLY THE SURFACE BOTH LANES IMPLEMENT IS REGISTERED. spinel's
    // client is a declared subset — no keep-alive, no proxy, no redirect
    // following, and no STREAMING body (`#request` with a block,
    // `#read_body` with a block) — so those names are deliberately absent
    // here even though CRuby has them. Registering them would make
    // `Opengraph::Fetch` type clean and fail at run time on spinel, which
    // is worse than the honest gap it currently reports. Same rule as
    // IPAddr: a call beyond the implemented surface stays a gap.
    let http_response = Ty::Class {
        id: ClassId(Symbol::from("Net::HTTPResponse")),
        args: vec![].into(),
    };
    let str_or_nil = Ty::Union { variants: vec![Ty::Str, Ty::Nil].into() };
    register_stdlib_class(classes, "Net::HTTP", &[
        ("get", Ty::Str),
        ("get_response", http_response.clone()),
        ("post_form", http_response.clone()),
    ], &[
        // Writers answer the value assigned. `use_ssl=` really is a bool
        // (`uri.scheme == "https"`); the timeouts take whatever the app
        // hands them — campfire assigns an ActiveSupport::Duration — so
        // asserting Int there would be a narrower answer than the truth.
        ("use_ssl=", Ty::Bool),
        ("open_timeout=", Ty::Untyped),
        ("read_timeout=", Ty::Untyped),
        ("use_ssl?", Ty::Bool), ("started?", Ty::Bool),
        ("address", Ty::Str), ("port", Ty::Int),
        ("finish", Ty::Nil),
        ("request", http_response.clone()),
        ("get", http_response.clone()),
        ("post", http_response.clone()),
    ]);
    // The EXCEPTION FAMILY. A `rescue => e` binds `e` as
    // `Class{StandardError}` (see `analyze::body`'s rescue arm) and a
    // `rescue Foo => e` binds the class named — and nothing registered
    // what any of them ANSWER, so the single most common thing anyone
    // does with a rescued exception, `e.message`, typed as a gap.
    //
    // It surfaced twice in one afternoon: once in a runtime file, where
    // `tests/runtime_src_integration.rs` refuses an untyped
    // sub-expression, and once in a gem façade whose whole job was to
    // report an exception. Registering the shape is cheaper than either
    // workaround, and it is the same shape for every member.
    //
    // The registry does no inheritance for stdlib classes, so each name
    // carries the surface — the same reason the `Net::HTTP::<verb>`
    // loop below repeats itself. The list is Ruby's own hierarchy under
    // `StandardError` plus `Exception` itself, which `rescue Exception`
    // names and campfire's `MessagesHelper` actually writes.
    // `cause` is the exception being handled when this one was raised, or
    // nil; `exception` answers the exception itself (`raise e` and
    // `e.exception("msg")` both go through it).
    let exception_surface = [
        ("message", Ty::Str),
        ("to_s", Ty::Str),
        ("full_message", Ty::Str),
        ("detailed_message", Ty::Str),
        ("inspect", Ty::Str),
        ("backtrace", Ty::Array { elem: std::sync::Arc::new(Ty::Str) }),
        (
            "backtrace_locations",
            Ty::Array {
                elem: std::sync::Arc::new(Ty::Class {
                    id: ClassId(Symbol::from("Thread::Backtrace::Location")),
                    args: vec![].into(),
                }),
            },
        ),
        ("set_backtrace", Ty::Untyped),
        ("exception", Ty::Untyped),
        (
            "cause",
            Ty::Union {
                variants: vec![
                    Ty::Class { id: ClassId(Symbol::from("Exception")), args: vec![].into() },
                    Ty::Nil,
                ].into(),
            },
        ),
    ];
    // One frame of `caller_locations` / `backtrace_locations`, as
    // rbs/core/thread.rbs declares it.
    let optional_str = || Ty::Union { variants: vec![Ty::Str, Ty::Nil].into() };
    register_stdlib_class(classes, "Thread::Backtrace::Location", &[], &[
        ("absolute_path", optional_str()),
        ("base_label", optional_str()),
        ("label", optional_str()),
        ("lineno", Ty::Int),
        ("path", optional_str()),
        ("to_s", Ty::Str),
        ("inspect", Ty::Str),
    ]);
    for exc in [
        "Exception", "StandardError", "RuntimeError", "ArgumentError",
        "TypeError", "NameError", "NoMethodError", "IndexError",
        "KeyError", "RangeError", "IOError", "NotImplementedError",
        "FrozenError", "ZeroDivisionError", "StopIteration",
        "ThreadError", "ClosedQueueError",
        // CRuby's bundled libraries and Spinel's uri/net/json packages
        // recognize these exception names; emitted requires load them.
        "URI::Error", "URI::InvalidURIError", "Net::OpenTimeout", "Net::ReadTimeout",
        "SocketError", "OpenSSL::OpenSSLError", "JSON::ParserError", "JSON::GeneratorError",
        // Campfire tip: `rescue SystemCallError` / `OpenSSL::SSL::SSLError`
        // on pooled web-push connections; `rescue Vips::Error` beside
        // ActiveStorage::PreviewError when drawing attachment variants.
        "SystemCallError", "OpenSSL::SSL::SSLError", "Vips::Error",
        // `Timeout.timeout` / `rescue Timeout::Error` — Campfire unfurl
        // deadline and TimeLimitedVideoPreviewer#capture.
        "Timeout::Error",
        // `rescue EOFError` around `readpartial` on a pipe or a pty.
        "EOFError",
        // lobsters' `Sponge::OTHER_PEOPLES_PROBLEMS`, a class-body Array
        // of exception classes. zlib's and Encoding's are Ruby's own and
        // spinel's (`packages/zlib`, `lib/sp_exc.c`). net/protocol's
        // `HTTPBadResponse` is CRuby's only: spinel's `packages/net`
        // does not define it, so `project::unavailable_class_module_construct`
        // ledgers it there.
        "Zlib::DataError", "Encoding::CompatibilityError", "Net::HTTPBadResponse",
        // `rescue SQLite3::Exception` — campfire's `ResponseCache` drops
        // its observer on any driver error (see `SQLite3::Database`).
        "SQLite3::Exception", "SQLite3::CantOpenException",
        "SQLite3::BusyException", "SQLite3::SQLException",
        // `rescue ArgumentError, RQRCodeCore::QRCodeRunTimeError` —
        // campfire's `QrCodeController#show` answers 400 for data too
        // long to encode. rqrcode_core's classes on the ruby family, the
        // spinel-rqrcode package's (same names, same raise) on spinel.
        "RQRCodeCore::QRCodeRunTimeError", "RQRCodeCore::QRCodeArgumentError",
        // `rescue CSV::MalformedCSVError` around `CSV.parse` (csv is loaded
        // by the emitted require, on Spinel by its csv package).
        "CSV::MalformedCSVError",
    ] {
        register_stdlib_class(classes, exc, &[], &exception_surface);
    }
    // `rescue Errno::ENOENT` / `Errno::EIO`: every Errno class that both
    // CRuby (on every POSIX platform) and Spinel's runtime define. The
    // lookup is by exact name, so the family is registered whole.
    register_stdlib_class(classes, "Errno", &[], &[]);
    for name in ERRNO_CLASSES {
        register_stdlib_class(classes, &format!("Errno::{name}"), &[], &exception_surface);
    }
    // CRuby's own core and default-gem classes, which only the CRuby and
    // JRuby trees load natively ([`CRUBY_ONLY_STDLIB`]).
    for (name, exception, class_methods) in CRUBY_ONLY_STDLIB {
        let class_side: Vec<(&str, Ty)> = class_methods.iter().map(|(m, ty)| (*m, ty())).collect();
        let instance_side: &[(&str, Ty)] = if *exception { &exception_surface } else { &[] };
        register_stdlib_class(classes, name, &class_side, instance_side);
    }
    for (exc, extra) in [
        // Rails' root, from runtime/ruby/active_record/errors.rb. Named
        // here so a gem RBI that reopens it (semian includes its
        // AdapterError into it) does not claim the runtime's class.
        ("ActiveRecord::ActiveRecordError", None),
        ("ActiveRecord::RecordNotFound", None),
        ("ActiveRecord::RecordNotUnique", None),
        ("ActiveRecord::ValueTooLong", None),
        // The CRuby tree's `Db` translates a driver error into these
        // (runtime/spinel/db_cruby.rb `translate`); `project` refuses
        // them on every other target, whose `Db` does not. Not
        // `InvalidForeignKey`: the harness's file database leaves
        // `PRAGMA foreign_keys` off and the schema loader drops
        // `add_foreign_key`, so nothing raises it there; its name stays
        // unresolved and `project` refuses it.
        ("ActiveRecord::StatementInvalid", Some(("sql", Ty::Union { variants: vec![Ty::Str, Ty::Nil].into() }))),
        ("ActiveRecord::NotNullViolation", Some(("sql", Ty::Union { variants: vec![Ty::Str, Ty::Nil].into() }))),
        ("ActiveRecord::CheckViolation", Some(("sql", Ty::Union { variants: vec![Ty::Str, Ty::Nil].into() }))),
        // Raised by the CRuby tree's SQLite3 adapter (ruby_overlay
        // active_record_sqlite3_adapter.rb).
        ("ActiveRecord::TransactionIsolationError", None),
        // The `sqlite3` gem's statement error, the cause of a
        // StatementInvalid on that tree.
        ("SQLite3::SQLException", None),
        // The `sqlite3` gem's constraint error, raised by its driver on that tree.
        ("SQLite3::ConstraintException", None),
        ("ActiveRecord::SoleRecordExceeded", None),
        // Not `ActiveRecord::Base`: no instance surface is registered there, so `e.record.errors` would still fail.
        ("ActiveRecord::RecordInvalid", Some(("record", Ty::Untyped))),
        ("ActiveRecord::RecordNotSaved", Some(("record", Ty::Untyped))),
        ("ActiveRecord::ReadOnlyRecord", None),
        // Names overlap `project::RUBY_FAMILY_RUNTIME_CONSTANTS` (emit
        // ledger). Keep extras here — inference needs the readers.
        ("ActionController::ParameterMissing", Some(("param", Ty::Str))),
        ("ActionController::UnpermittedParameters", None),
        ("ActionController::UnknownFormat", None),
        ("ActionController::BadRequest", None),
        ("ActionController::InvalidAuthenticityToken", None),
        ("ActionController::RoutingError", Some(("failures", Ty::Array { elem: std::sync::Arc::new(Ty::Str) }))),
        // What a strict validation raises (runtime/ruby/active_record/error_classes.rb).
        ("ActiveModel::StrictValidationFailed", None),
        // A nil `delegate_missing_to` target's error (ruby_overlay
        // active_support_core_ext.rb, a NoMethodError); `project` refuses
        // it off the CRuby and JRuby trees.
        ("ActiveSupport::DelegationError", None),
        ("AbstractController::ActionNotFound", None),
    ] {
        let mut methods = exception_surface.to_vec();
        methods.extend(extra);
        let methods: Vec<(&str, Ty)> = methods;
        register_stdlib_class(classes, exc, &[], &methods);
    }
    // The class `ActiveRecord::Base.connection` answers on the CRuby
    // tree (ruby_overlay active_record_sqlite3_adapter.rb), as a name;
    // `project` refuses it on every other target.
    register_stdlib_class(classes, "ActiveRecord::ConnectionAdapters::SQLite3Adapter", &[], &[]);
    // The type name a `where` chain is checked against
    // (runtime/ruby/active_record/relation.rb): no instance exists.
    register_stdlib_class(classes, "ActiveRecord::QueryMethods", &[], &[]);
    register_stdlib_class(classes, "ActiveRecord::QueryMethods::WhereChain", &[], &[]);
    // The built-in validators `runtime/spinel/active_model_validations.rb`
    // ports (EachValidator's surface); a memoized instance
    // (`#: ActiveModel::Validations::LengthValidator`) answers
    // `validate_each` and `validate` as it does in Rails.
    for validator in [
        "Absence", "Acceptance", "Comparison", "Confirmation", "Exclusion", "Format", "Inclusion",
        "Length", "Numericality", "Presence",
    ] {
        register_stdlib_class(classes, &format!("ActiveModel::Validations::{validator}Validator"), &[], &[
            ("validate_each", Ty::Untyped),
            ("validate", Ty::Untyped),
            ("attributes", Ty::Untyped),
            ("options", Ty::Untyped),
            ("kind", Ty::Untyped),
        ]);
    }
    // `ActiveModel::Type.default_value` (activemodel 8.2.0.alpha
    // type.rb): the `Type::Value` an attribute with no declared type
    // answers. The Ruby family's type port defines it
    // (runtime/spinel/active_model_type.rb); the AR attribute API reads it.
    register_stdlib_class(classes, "ActiveModel::Type", &[
        ("default_value", Ty::Class { id: ClassId(Symbol::from("ActiveModel::Type::Value")), args: vec![].into() }),
    ], &[]);
    // `ActiveModel::Type::Boolean.new.cast(v)`: nil for a blank value, else the boolean.
    // `ActiveSupport::TimeZone` is `runtime/ruby/active_support_ext.rb`'s port; `[]` is typed in dispatch, by its argument.
    {
        let zone = || Ty::Class { id: ClassId(Symbol::from("ActiveSupport::TimeZone")), args: vec![].into() };
        register_stdlib_class(classes, "ActiveSupport::TimeZone", &[
            ("all", Ty::Array { elem: std::sync::Arc::new(zone()) }),
            ("lookup", Ty::Union { variants: vec![zone(), Ty::Nil].into() }),
        ], &[
            ("name", Ty::Str), ("utc_offset", Ty::Int), ("formatted_offset", Ty::Str),
            ("to_s", Ty::Str), ("now", Ty::Time),
        ]);
    }
    register_stdlib_class(classes, crate::analyze::indifferent::CLASS, &[], &[]);
    // Not answered here beyond `new`: the dispatch arm for `ActiveSupport::StringInquirer` types its predicates and String surface.
    register_stdlib_class(classes, "ActiveSupport::StringInquirer", &[("new", Ty::Str)], &[]);
    // `ActiveRecord::Type::Boolean` is a subclass with the same casting; `deserialize` casts the same way for a boolean.
    for name in ["ActiveModel::Type::Boolean", "ActiveRecord::Type::Boolean"] {
        register_stdlib_class(classes, name, &[], &[
            ("cast", Ty::Union { variants: vec![Ty::Bool, Ty::Nil].into() }),
            ("deserialize", Ty::Union { variants: vec![Ty::Bool, Ty::Nil].into() }),
        ]);
    }
    // Not a typed store: a thread-local slot holds whatever the caller put there, so `[]` answers untyped.
    // `new` / `pass` / `kill` / `join` — `runtime/ruby/timeout.rb`'s wall-clock
    // port (Spinel lane) starts a worker and kills it past the deadline.
    let thread = Ty::Class { id: ClassId(Symbol::from("Thread")), args: vec![].into() };
    register_stdlib_class(classes, "Thread", &[
        ("current", thread.clone()),
        ("new", thread.clone()),
        ("pass", Ty::Nil),
    ], &[
        ("[]", Ty::Untyped),
        ("[]=", Ty::Untyped),
        ("kill", thread.clone()),
        ("join", thread.clone()),
    ]);
    // The spinel `csv` package's writer surface: `CSV.generate { |csv| csv << row }` answers the accumulated String.
    let csv = Ty::Class { id: ClassId(Symbol::from("CSV")), args: vec![].into() };
    register_stdlib_class(classes, "CSV", &[("generate", Ty::Str), ("generate_line", Ty::Str)], &[
        ("<<", csv.clone()),
        ("add_row", csv.clone()),
        ("string", Ty::Str),
    ]);
    // The response. `code` is a String here as it is in CRuby ("200",
    // not 200) — campfire compares `response.code == "200"`, which folds
    // to a constant false against an Int.
    register_stdlib_class(classes, "Net::HTTPResponse", &[], &[
        ("code", Ty::Str), ("body", Ty::Str), ("message", Ty::Str),
        ("http_version", Ty::Str),
        ("content_type", str_or_nil.clone()),
        ("[]", str_or_nil.clone()),
        ("key?", Ty::Bool),
    ]);
    // The request family. CRuby builds these as `Net::HTTP::Post.new(…)`
    // and the surface lives on a shared parent; the registry does no
    // inheritance for stdlib classes, so each verb carries it.
    // `Patch` is in CRuby's net/http and Spinel's packages/net (same
    // request-family surface as Put). Jumpstart Pro's ApplicationClient
    // and every JSON API client that mirrors Rails' patch routes name it.
    for verb in ["Get", "Post", "Put", "Patch", "Delete", "Head"] {
        register_stdlib_class(classes, &format!("Net::HTTP::{verb}"), &[], &[
            ("body=", Ty::Str), ("body", Ty::Str),
            ("[]=", Ty::Str), ("[]", str_or_nil.clone()),
            ("content_type=", Ty::Str),
            ("set_form_data", Ty::Str),
            ("method", Ty::Str), ("path", Ty::Str),
        ]);
    }
    // `Net::ProtocolError` — Spinel's packages/net defines it (with
    // OpenTimeout / ReadTimeout). `Net::HTTPBadResponse` is registered
    // above for Lobsters and Spinel-ledgers via
    // `unavailable_class_module_construct`. `Net::HTTPHeaderSyntaxError`
    // stays a CRuby-only constant gap until Spinel grows it (invariant 6).
    register_stdlib_class(classes, "Net::ProtocolError", &[], &exception_surface);
    // `Mime::Type` — the class `runtime/ruby/mime.rb` ports from
    // actionpack. `lookup` is deliberately NON-nilable: upstream answers
    // a Type for any well-formed string (symbol nil when unregistered)
    // and raises for the rest, which is what makes
    // `if t = Mime::Type.lookup(ct)` a safe guard in app code. Giving it
    // a `| Nil` arm here would be a lie the type system can act on.
    let mime_type = Ty::Class { id: ClassId(Symbol::from("Mime::Type")), args: vec![].into() };
    register_stdlib_class(classes, "Mime::Type", &[
        ("lookup", mime_type.clone()),
        ("lookup_by_extension", Ty::Union {
            variants: vec![mime_type.clone(), Ty::Nil].into(),
        }),
        ("valid?", Ty::Bool),
    ], &[
        ("symbol", Ty::Union { variants: vec![Ty::Sym, Ty::Nil].into() }),
        ("to_sym", Ty::Union { variants: vec![Ty::Sym, Ty::Nil].into() }),
        ("to_s", Ty::Str), ("to_str", Ty::Str), ("inspect", Ty::Str),
        ("hash", Ty::Int), ("==", Ty::Bool), ("eql?", Ty::Bool),
        ("html?", Ty::Bool), ("all?", Ty::Bool),
    ]);
    // `Mime::ALL`'s class: the `*/*` wildcard, a Type whose `all?` and
    // `html?` answer true (actionpack mime_type.rb:433-447).
    let all_type = Ty::Class { id: ClassId(Symbol::from("Mime::AllType")), args: vec![].into() };
    register_stdlib_class(classes, "Mime::AllType", &[("instance", all_type)], &[]);
    if let Some(all) = classes.get_mut(&ClassId(Symbol::from("Mime::AllType"))) {
        all.parent.get_or_insert_with(|| ClassId(Symbol::from("Mime::Type")));
    }
    // `URI.parse` returns a URI object we don't model; `Untyped` lets
    // chained `.scheme` / `.host` flow gradually instead of erroring.
    register_stdlib_class(classes, "URI", &[
        ("parse", Ty::Untyped), ("join", Ty::Untyped),
        ("escape", Ty::Str), ("unescape", Ty::Str),
        ("encode_www_form", Ty::Str), ("decode_www_form", Ty::Untyped),
    ], &[]);
    // A class test such as `URI.parse(url).is_a?(URI::HTTP)` names the
    // real bundled class, without claiming any extra instance methods.
    register_stdlib_class(classes, "URI::HTTP", &[], &[]);
    register_stdlib_class(classes, "URI::HTTPS", &[], &[]);
    // `res.is_a?(Net::HTTPSuccess)` — lobsters' Github; spinel's
    // `packages/net` defines the same response family.
    for response in ["Net::HTTPRedirection", "Net::HTTPOK", "Net::HTTPSuccess"] {
        register_stdlib_class(classes, response, &[], &[]);
    }
    // The implementation is bundled on Ruby/Spinel. Other targets
    // report the missing runtime at project emission.
    let string_io = Ty::Class { id: ClassId(Symbol::from("StringIO")), args: vec![].into() };
    register_stdlib_class(classes, "StringIO", &[], &[
        ("string", Ty::Str), ("<<", string_io),
    ]);
    // `Process` — Ruby core, and on both Ruby lanes: CRuby's own, and
    // Spinel's codegen, which lowers `Process.clock_gettime(id, unit)`
    // and the four POSIX clock ids below to the C clock. `pid` and
    // `clock_gettime` are typed by the send dispatcher (the unit
    // argument picks Float or Integer), so no method is declared here;
    // what this adds is the module itself and its clock ids, which are
    // Integers in Ruby. Only the ids Spinel emits are listed; any other
    // `Process::` constant (`WNOHANG`) is the one Ruby's signatures
    // declare (`rbs/core/process.rbs`), read on the CRuby and JRuby
    // trees. `project` refuses the module value, and such a constant,
    // on every target whose runtime does not supply it.
    let process = classes.entry(ClassId(Symbol::from("Process"))).or_default();
    process.is_module = true;
    for clock in ["CLOCK_MONOTONIC", "CLOCK_REALTIME", "CLOCK_PROCESS_CPUTIME_ID", "CLOCK_THREAD_CPUTIME_ID"] {
        process.constants.entry(Symbol::from(clock)).or_insert(Ty::Int);
    }
    // `IO` / `Process` / `Timeout` — Campfire tip names these as Consts
    // in indexed app source (`time_limited_video_previewer`, web-push
    // connection pool, unfurl deadline). They were listed in
    // `RUBY_TOP_LEVEL` (declared-type noise) but never registered, so
    // Const resolution raised Unsupported. Register the class objects
    // for Const resolution only. Class-method returns for `Process.*`,
    // `Timeout.timeout`, and `IO.popen` / `copy_stream` live in the
    // send special-cases (`body/send.rs`) — catalog entries would win
    // before those cases and kill unit-aware `clock_gettime` (Float for
    // `:millisecond`). `IO::NULL` is an empty ClassId the way `URI::HTTP`
    // is; its value type comes from Ruby's signatures (`stdlib_rbs`).
    // Instance methods on an `IO` handle still belong here. `winsize` /
    // `winsize=` wait on carrying `require "io/console"` into the
    // emitted tree; admitting them without that load is check-quiet /
    // runtime `NoMethodError`.
    let io = Ty::Class { id: ClassId(Symbol::from("IO")), args: vec![].into() };
    register_stdlib_class(classes, "IO", &[], &[
        ("pid", Ty::Int),
        ("read", Ty::Str),
        ("readpartial", Ty::Str),
        ("write", Ty::Int),
        ("closed?", Ty::Bool),
        ("rewind", Ty::Int),
        ("binmode", io.clone()),
        ("close", Ty::Nil),
    ]);
    register_stdlib_class(classes, "IO::NULL", &[], &[]);
    // `File < IO` — Ruby's hierarchy (not a PTY detail). File's class
    // methods are registered earlier; the parent is set once `IO` exists
    // so instance methods (`readpartial`, `closed?`, …) resolve on a
    // File handle, including `PTY.spawn`'s reader/writer.
    classes
        .get_mut(&ClassId(Symbol::from("File")))
        .expect("File registered above")
        .parent = Some(ClassId(Symbol::from("IO")));
    // `require "pty"`. `PTY.spawn`'s return is a send special case: its
    // block form answers nil; block yields are typed in `block_params_for`.
    register_stdlib_class(classes, "PTY", &[], &[]);
    // A default gem a booted Rails app has already loaded; the BUNDLED
    // row emits its require.
    register_stdlib_class(classes, "Shellwords", &[
        ("escape", Ty::Str), ("shellescape", Ty::Str),
        ("join", Ty::Str), ("shelljoin", Ty::Str),
        ("split", str_arr()), ("shellsplit", str_arr()), ("shellwords", str_arr()),
    ], &[]);
    // `Process` and its clock ids are registered above as a module with
    // Integer constants (what Ruby answers), not as empty ClassIds.
    // Module Const only — `timeout` return is the send special-case.
    // Exception is `Timeout::Error` above. CRuby loads via BUNDLED
    // `require "timeout"`; Spinel gets `runtime/ruby/timeout.rb`.
    register_stdlib_class(classes, "Timeout", &[], &[]);
    // GC / OpenSSL::HMAC / Enumerator::Product / Enumerator::Lazy (and
    // FileUtils' `mkdir_p` / `cp`, in its registration above), as an API
    // reaches for them: `GC.start` between batches, an HMAC
    // signature of a webhook payload, `Enumerator::Product.new(*lists)` over
    // route segments, and `is_a?(Enumerator::Lazy)` in a serializer. Only
    // the calls checked against CRuby and Spinel are registered
    // (tests/support/stdlib_rnp.rs); another FileUtils call stays a dispatch
    // error rather than a guess. Each library is loaded by its emitted
    // require; on Spinel by its fileutils / openssl package.
    register_stdlib_class(classes, "GC", &[("start", Ty::Nil)], &[]);
    register_stdlib_class(classes, "OpenSSL::HMAC", &[("digest", Ty::Str), ("hexdigest", Ty::Str)], &[]);
    let product = Ty::Class { id: ClassId(Symbol::from("Enumerator::Product")), args: vec![].into() };
    register_stdlib_class(classes, "Enumerator::Product", &[("new", product)], &[
        // `size` is left out: it answers nil or Float::INFINITY for an
        // unsized or endless factor, and it is not checked on both lanes.
        ("to_a", Ty::Array { elem: std::sync::Arc::new(Ty::Array { elem: std::sync::Arc::new(Ty::Untyped) }) }),
    ]);
    register_stdlib_class(classes, "Enumerator::Lazy", &[], &[]);
    // JSON dispatch is already intrinsic in BodyTyper and the emitters;
    // a source-backed reference must also recognize its exact namespace.
    register_stdlib_class(classes, "JSON", &[], &[]);
    // Ruby and Spinel provide both core classes. The existing constructor
    // rule carries Struct.new's value into an assigned constant, so reads
    // of that constant and Mutex.new retain their source expressions.
    // Do not invent member or synchronization return types here.
    register_stdlib_class(classes, "Struct", &[], &[]);
    register_stdlib_class(classes, "Mutex", &[], &[]);
    // A Ruby core class the Ruby-family runtimes themselves use
    // (`Fiber[:db_handle]`, Spinel's scheduled server); `project`
    // refuses it where a target supplies none. No surface is invented.
    register_stdlib_class(classes, "Fiber", &[], &[]);
    // The queue constructors, Thread's core aliases, and the standard
    // mixins likewise need exact entries for source constant resolution.
    // No queue element, synchronization or mixin method types are added;
    // the Ruby-family runtimes supply the actual behavior. Not the
    // top-level `Queue` / `SizedQueue`: Ruby's signatures alias them to
    // these Thread classes (`Queue.name` is "Thread::Queue"), and an
    // entry of their own would type `Queue` as a class of that name.
    for name in ["Thread::Queue", "Thread::SizedQueue", "Thread::Mutex", "Comparable", "Enumerable"] {
        register_stdlib_class(classes, name, &[], &[]);
    }
    // Ruby and Spinel supply these too, once the BUNDLED requires are
    // written. Only the calls checked on both (tests/stdlib_constant_execution.rs)
    // are typed; what `ConditionVariable#wait` answers is not checked, so it
    // stays untyped. `Find.find` answers nil with a block and an Enumerator
    // without one, so it stays untyped as `File.open` does.
    register_stdlib_class(classes, "Monitor", &[], &[]);
    for name in ["ConditionVariable", "Thread::ConditionVariable"] {
        let condition_variable = Ty::Class { id: ClassId(Symbol::from(name)), args: vec![].into() };
        register_stdlib_class(classes, name, &[], &[
            ("signal", condition_variable.clone()),
            ("broadcast", condition_variable),
            ("wait", Ty::Untyped),
        ]);
    }
    let unix_socket = Ty::Class { id: ClassId(Symbol::from("UNIXSocket")), args: vec![].into() };
    register_stdlib_class(classes, "UNIXSocket", &[], &[]);
    register_stdlib_class(classes, "UNIXServer", &[], &[("accept", unix_socket)]);
    classes.get_mut(&ClassId(Symbol::from("UNIXSocket"))).expect("registered above").parent =
        Some(ClassId(Symbol::from("IO")));
    classes.get_mut(&ClassId(Symbol::from("UNIXServer"))).expect("registered above").parent =
        Some(ClassId(Symbol::from("UNIXSocket")));
    register_stdlib_class(classes, "Open3", &[
        ("capture3", Ty::Tuple { elems: vec![Ty::Str, Ty::Str, Ty::Untyped].into() }),
    ], &[]);
    register_stdlib_class(classes, "Find", &[("find", Ty::Untyped), ("prune", Ty::Bottom)], &[]);
    for (class, method) in BUILTIN_BLOCK_VALUE_METHODS {
        classes
            .entry(ClassId(Symbol::from(*class)))
            .or_default()
            .block_value_methods
            .insert(Symbol::from(*method));
    }
    // `Array.wrap` is folded by `lower::enumerable_ext` before emit.
    // Registered so the analyzer does not report it as unknown. The
    // element type is not known from a scalar argument.
    register_stdlib_class(
        classes,
        "Array",
        &[("wrap", Ty::Array { elem: std::sync::Arc::new(Ty::Untyped) })],
        &[],
    );
    // CRuby supplies Sets here, the Spinel port supplies Arrays. Both
    // implement the collection operations the app uses; don't invent
    // one concrete representation for the two runtimes.
    register_stdlib_class(classes, "Rails::HTML5::SafeListSanitizer", &[
        ("allowed_tags", Ty::Union { variants: vec![Ty::Array { elem: std::sync::Arc::new(Ty::Str) }, Ty::Class { id: ClassId(Symbol::from("Set")), args: vec![].into() }].into() }),
        ("allowed_attributes", Ty::Union { variants: vec![Ty::Array { elem: std::sync::Arc::new(Ty::Str) }, Ty::Class { id: ClassId(Symbol::from("Set")), args: vec![].into() }].into() }),
    ], &[]);
    // `Set` is a value type: `Set.new` yields `Class { Set }` (via the
    // universal `.new`), then these instance methods dispatch on it.
    // Mutators return the receiver (self) for chaining; element-typed
    // accessors are `Untyped` (Set isn't parameterized here).
    let set_self = Ty::Class { id: ClassId(Symbol::from("Set")), args: vec![].into() };
    // Enumerable's filters and sorts answer an Array, not a Set.
    let untyped_array = Ty::Array { elem: std::sync::Arc::new(Ty::Untyped) };
    register_stdlib_class(classes, "Set", &[("[]", set_self.clone())], &[
        ("<<", set_self.clone()), ("add", set_self.clone()),
        ("delete", set_self.clone()), ("merge", set_self.clone()),
        ("subtract", set_self.clone()), ("clear", set_self.clone()),
        ("keep_if", set_self.clone()), ("delete_if", set_self.clone()),
        ("|", set_self.clone()), ("&", set_self.clone()),
        ("-", set_self.clone()), ("+", set_self.clone()), ("^", set_self.clone()),
        ("union", set_self.clone()), ("intersection", set_self.clone()),
        ("difference", set_self.clone()),
        ("dup", set_self.clone()), ("to_set", set_self.clone()),
        ("add?", Ty::Untyped),
        ("delete?", Ty::Union { variants: vec![set_self.clone(), Ty::Nil].into() }),
        ("each", Ty::Untyped),
        ("map", untyped_array.clone()), ("flat_map", untyped_array.clone()),
        ("filter_map", untyped_array.clone()),
        ("select", untyped_array.clone()), ("filter", untyped_array.clone()),
        ("reject", untyped_array.clone()),
        ("sort", untyped_array.clone()), ("sort_by", untyped_array.clone()),
        ("partition", Ty::Array { elem: std::sync::Arc::new(untyped_array.clone()) }),
        ("to_a", untyped_array.clone()),
        ("find", Ty::Untyped), ("detect", Ty::Untyped),
        ("first", Ty::Untyped),
        ("max", Ty::Untyped), ("min", Ty::Untyped),
        ("max_by", Ty::Untyped), ("min_by", Ty::Untyped),
        ("sum", Ty::Untyped), ("inject", Ty::Untyped), ("reduce", Ty::Untyped),
        ("each_with_object", Ty::Untyped),
        ("group_by", Ty::Hash {
            key: std::sync::Arc::new(Ty::Untyped),
            value: std::sync::Arc::new(untyped_array.clone()),
        }),
        ("include?", Ty::Bool), ("member?", Ty::Bool), ("empty?", Ty::Bool),
        ("any?", Ty::Bool), ("all?", Ty::Bool), ("none?", Ty::Bool), ("one?", Ty::Bool),
        ("intersect?", Ty::Bool), ("disjoint?", Ty::Bool),
        ("exclude?", Ty::Bool),
        ("size", Ty::Int), ("length", Ty::Int), ("count", Ty::Int),
        ("subset?", Ty::Bool), ("superset?", Ty::Bool),
    ]);

    // Gem / ecosystem catalog (`crate::catalog::gems`). Targeting
    // Rails realistically means targeting its gem ecosystem;
    // rather than enumerate every gem, we register the surface
    // apps actually call (Arel, ROTP, Nokogiri, …) by discovery.
    // Registered like the stdlib singletons — `or_insert`, so a
    // user class of the same name still wins.
    for gem in crate::catalog::GEM_CATALOG {
        let class_methods: Vec<(&str, Ty)> =
            gem.class_methods.iter().map(|(n, k)| (*n, k.to_ty())).collect();
        let instance_methods: Vec<(&str, Ty)> =
            gem.instance_methods.iter().map(|(n, k)| (*n, k.to_ty())).collect();
        register_stdlib_class(classes, gem.name, &class_methods, &instance_methods);
    }
}

fn untyped() -> Ty {
    Ty::Untyped
}

fn string() -> Ty {
    Ty::Str
}

/// Ruby core and default-gem constants the CRuby and JRuby trees load
/// natively: their interpreter defines them, or the tree writes the
/// require (`project::CRUBY_STDLIB_REQUIRES`). No other runtime defines
/// them, so `project` refuses them there. Each row is the name, whether
/// it is an exception class, and its class-side surface. Results that
/// depend on the data read (`YAML.load_file`) are gradual.
pub(crate) const CRUBY_ONLY_STDLIB: &[(&str, bool, &[(&str, fn() -> Ty)])] = &[
    ("Interrupt", true, &[]),
    ("SignalException", true, &[]),
    ("SystemExit", true, &[]),
    ("ScriptError", true, &[]),
    ("LoadError", true, &[]),
    ("SyntaxError", true, &[]),
    ("SecurityError", true, &[]),
    ("ThreadError", true, &[]),
    ("FiberError", true, &[]),
    ("EncodingError", true, &[]),
    ("LocalJumpError", true, &[]),
    ("FloatDomainError", true, &[]),
    ("ClosedQueueError", true, &[]),
    ("UncaughtThrowError", true, &[]),
    ("NoMatchingPatternError", true, &[]),
    ("NoMatchingPatternKeyError", true, &[]),
    ("SystemCallError", true, &[]),
    ("Errno::ENOENT", true, &[]),
    ("Errno::EEXIST", true, &[]),
    ("Errno::EACCES", true, &[]),
    ("Errno::EPERM", true, &[]),
    ("Errno::ESRCH", true, &[]),
    ("Errno::ECHILD", true, &[]),
    ("Errno::EPIPE", true, &[]),
    ("Errno::EINVAL", true, &[]),
    ("Errno::EAGAIN", true, &[]),
    ("Errno::EBADF", true, &[]),
    ("Errno::EISDIR", true, &[]),
    ("Errno::ENOTDIR", true, &[]),
    ("Errno::ENOTEMPTY", true, &[]),
    ("Errno::ENOTTY", true, &[]),
    ("Errno::EOPNOTSUPP", true, &[]),
    ("Errno::ECONNREFUSED", true, &[]),
    ("Errno::ECONNRESET", true, &[]),
    ("Errno::ETIMEDOUT", true, &[]),
    ("Errno::EADDRINUSE", true, &[]),
    ("Errno::EHOSTUNREACH", true, &[]),
    // Core since Ruby 3.2: an `IO#timeout` expiry (Essentials'
    // OidService retries it).
    ("IO::TimeoutError", true, &[]),
    ("Timeout", false, &[("timeout", untyped)]),
    ("Timeout::Error", true, &[]),
    ("Timeout::ExitException", true, &[]),
    // psych/exception.rb and psych/syntax_error.rb: what `Psych.safe_load`
    // raises. paquito's SafeYAML#load rescues `Psych::DisallowedClass`
    // into UnsupportedType and any other `Psych::Exception` into
    // UnpackError.
    ("Psych::Exception", true, &[]),
    ("Psych::BadAlias", true, &[]),
    ("Psych::AliasesNotEnabled", true, &[]),
    ("Psych::AnchorNotDefined", true, &[]),
    ("Psych::DisallowedClass", true, &[]),
    ("Psych::SyntaxError", true, &[]),
    ("Marshal", false, &[("dump", string), ("load", untyped)]),
    ("FileUtils", false, &[
        ("mkdir_p", untyped), ("makedirs", untyped), ("mkdir", untyped), ("rm", untyped),
        ("rm_f", untyped), ("rm_r", untyped), ("rm_rf", untyped), ("rmdir", untyped),
        ("mv", untyped), ("cp", untyped), ("cp_r", untyped), ("touch", untyped),
        ("ln_s", untyped), ("ln_sf", untyped), ("chmod", untyped), ("compare_file", untyped),
    ]),
    ("YAML", false, &[
        ("load", untyped), ("load_file", untyped), ("safe_load", untyped),
        ("safe_load_file", untyped), ("unsafe_load", untyped), ("unsafe_load_file", untyped),
        ("load_stream", untyped), ("dump", string),
    ]),
];

/// Library methods that answer their block's value, by Ruby's (or
/// ActiveSupport's) definition rather than by inference:
/// `mutex.synchronize { … }` — campfire's `ResponseCache#version` is
/// `@mutex.synchronize { current_version }` — and a cache `fetch`, which
/// answers the block's value on a miss and what such a block wrote on a
/// hit. Only an informative block type is adopted (`block_value_return`).
/// Re-seeded after every harvest of the app's own block-value methods
/// (`Analyzer::harvest_block_value_methods`), which starts from empty.
pub(crate) const BUILTIN_BLOCK_VALUE_METHODS: &[(&str, &str)] = &[
    ("Mutex", "synchronize"),
    ("Thread::Mutex", "synchronize"),
    ("Monitor", "synchronize"),
    ("ActiveSupport::Cache::MemoryStore", "fetch"),
];

fn register_stdlib_class(
    classes: &mut HashMap<ClassId, ClassInfo>,
    name: &str,
    class_methods: &[(&str, Ty)],
    instance_methods: &[(&str, Ty)],
) {
    let cls = classes.entry(ClassId(Symbol::from(name))).or_default();
    for (m, ty) in class_methods {
        cls.class_methods
            .entry(Symbol::from(*m))
            .or_insert_with(|| ty.clone());
    }
    for (m, ty) in instance_methods {
        cls.instance_methods
            .entry(Symbol::from(*m))
            .or_insert_with(|| ty.clone());
    }
}

/// The Errno classes CRuby defines on every POSIX platform that Spinel's
/// runtime (`lib/sp_exc.c`) defines too.
const ERRNO_CLASSES: &[&str] = &[
    "EPERM", "ENOENT", "ESRCH", "EINTR", "EIO", "ENXIO", "E2BIG", "ENOEXEC", "EBADF",
    "ECHILD", "EAGAIN", "ENOMEM", "EACCES", "EFAULT", "EBUSY", "EEXIST", "EXDEV", "ENODEV",
    "ENOTDIR", "EISDIR", "EINVAL", "ENFILE", "EMFILE", "ENOTTY", "EFBIG", "ENOSPC", "ESPIPE",
    "EROFS", "EMLINK", "EPIPE", "EDOM", "ERANGE", "EDEADLK", "ENAMETOOLONG", "ENOLCK",
    "ENOSYS", "ENOTEMPTY", "ELOOP", "ENOTSOCK", "EMSGSIZE", "EPROTOTYPE", "ENOPROTOOPT",
    "EPROTONOSUPPORT", "ENOTSUP", "EOPNOTSUPP", "EAFNOSUPPORT", "EADDRINUSE",
    "EADDRNOTAVAIL", "ENETDOWN", "ENETUNREACH", "ENETRESET", "ECONNABORTED", "ECONNRESET",
    "ENOBUFS", "EISCONN", "ENOTCONN", "ETIMEDOUT", "ECONNREFUSED", "EHOSTUNREACH",
    "EALREADY", "EINPROGRESS", "ESTALE", "EDQUOT", "ECANCELED", "EOVERFLOW", "EILSEQ",
];
