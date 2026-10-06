// `config.filter_parameters` (railties 8.1.4) carried into the emitted
// app: the writes in config/application.rb and every initializer, in
// boot order, reach `ActiveSupport.filter_parameters` and the event
// reporter's payload filter after boot, as the
// `active_support.set_filter_parameters` initializer does it
// (activesupport railtie.rb:82-87). tests/filter_parameters_runtime.rb
// prints the same observations from a real Rails 8.1.4 boot of the same
// configuration and from the emitted real-blog; they must be equal.
//
// Both sides run the fixture's own config/initializers/
// filter_parameter_logging.rb (Rails' generated `+= [:passw, ...]`) and
// the test's files below. The native app is a bare railties app: the
// fixture's other initializers (assets, content security policy,
// inflections) need frameworks it does not load, and none of them
// touches filter_parameters.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::path::{Path, PathBuf};
use std::process::Command;

const ORACLE: &str = "tests/filter_parameters_runtime.rb";

/// Lines appended to the Application class body.
const CLASS_BODY: &str = "    config.filter_parameters += [:card_number]\n";

/// Every filter kind and every write shape Rails takes, after the
/// fixture's generated initializer (load order is alphabetical).
const APPENDS: &str = r#"Rails.application.configure do
  config.filter_parameters << "card.code"
end

Rails.application.configure do |app|
  app.config.filter_parameters.push(/\Aotp_/)
end

Rails.application.config.filter_parameters.concat([
  ->(key, value) { value.reverse! if key.to_s == "nickname" && value.is_a?(String) },
])
"#;

/// `=` starts the list over: everything the class body and the generated
/// initializer added before it is gone.
const RESET: &str = r#"Rails.application.config.filter_parameters = [:email, "secret"]
Rails.application.config.filter_parameters += [/\Apublic_/]
"#;

/// Core's shape (config/initializers/filter_parameter_logging.rb): the
/// list is whatever an app lib module computes, required by name from
/// the initializer, then extended.
const FROM_LIB: &str = r#"require "filter_list"

Rails.application.config.filter_parameters = FilterList.for_config
Rails.application.config.filter_parameters += FilterList::EXTRA
"#;

const FILTER_LIST: &str = r#"module FilterList
  SECRETS = [:passw, /\Atoken\z/i].freeze
  EXTRA = [:public_key].freeze

  def self.for_config
    nickname = lambda do |key, value, params|
      value.replace("[#{params.size} params]") if key.to_s == "nickname" && value.is_a?(String)
    end
    SECRETS + ["card.code", nickname]
  end
end
"#;

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn native(initializer: &str, lib: &[(&str, &str)]) -> String {
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let dir: PathBuf = std::env::temp_dir().join(format!(
        "filter-parameters-oracle-{}-{}",
        std::process::id(),
        N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    ));
    let init = dir.join("config/initializers");
    std::fs::create_dir_all(&init).expect("mkdir native app");
    std::fs::write(
        dir.join("config/application.rb"),
        format!(
            "require \"rails\"\nrequire \"logger\"\n\nmodule FilterOracle\n  class Application < Rails::Application\n    config.load_defaults 8.1\n    config.root = File.expand_path(\"..\", __dir__)\n    config.eager_load = false\n    config.logger = Logger.new(nil)\n{CLASS_BODY}  end\nend\n"
        ),
    )
    .expect("write native application.rb");
    std::fs::copy(
        root().join("fixtures/real-blog/config/initializers/filter_parameter_logging.rb"),
        init.join("filter_parameter_logging.rb"),
    )
    .expect("copy the generated initializer");
    std::fs::write(init.join("zz_filters.rb"), initializer).expect("write initializer");
    for (path, content) in lib {
        let full = dir.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).expect("mkdir lib");
        std::fs::write(full, content).expect("write lib file");
    }
    let out = Command::new("ruby")
        .arg(root().join(ORACLE))
        .arg("native")
        .arg(&dir)
        .output()
        .expect("ruby is on PATH");
    assert!(
        out.status.success(),
        "native Rails 8.1.4 boot failed\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn emitted(initializer: &str, lib: &[(&str, &str)]) -> String {
    let mut overlay = emit_and_run::real_blog();
    for (path, content) in lib {
        overlay = overlay.write(path, content);
    }
    let run = overlay
        .edit(
            "config/application.rb",
            "    config.autoload_lib(ignore: %w[assets tasks])\n",
            &format!("    config.autoload_lib(ignore: %w[assets tasks])\n{CLASS_BODY}"),
        )
        .write("config/initializers/zz_filters.rb", initializer)
        .run_ruby(&format!("ARGV.clear\nload {:?}\n", root().join(ORACLE).display().to_string()));
    run.assert_passes();
    run.stdout.clone()
}

fn assert_agree(initializer: &str, lib: &[(&str, &str)], expected_filters: &str) {
    let native = native(initializer, lib);
    let emitted = emitted(initializer, lib);
    assert_eq!(native, emitted, "emitted filter_parameters diverge from Rails 8.1.4");
    // Not a vacuous agreement: the list is the one Rails builds.
    assert_eq!(native.lines().next(), Some(expected_filters), "native output:\n{native}");
    assert!(native.contains("[FILTERED]"), "nothing was filtered:\n{native}");
}

#[test]
fn appends_from_application_rb_and_initializers_filter_like_rails() {
    assert_agree(
        APPENDS,
        &[],
        "filters: :card_number :passw :email :secret :token :_key :crypt :salt :certificate :otp :ssn :cvv :cvc \"card.code\" /\\Aotp_/ Proc(arity=2, lambda=true)",
    );
}

#[test]
fn an_assignment_starts_the_list_over_like_rails() {
    assert_agree(RESET, &[], "filters: :email \"secret\" /\\Apublic_/");
}

#[test]
fn a_list_an_app_lib_module_computes_filters_like_rails() {
    assert_agree(
        FROM_LIB,
        &[("lib/filter_list.rb", FILTER_LIST)],
        "filters: :passw /\\Atoken\\z/i \"card.code\" Proc(arity=3, lambda=true) :public_key",
    );
}
