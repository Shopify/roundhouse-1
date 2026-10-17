//! A Concern's `included do ... rescue_from ... end`, run on a controller
//! that includes it — Core's `ApplicationControllerExceptionHandler`:
//!
//! ```ruby
//! module ApplicationControllerExceptionHandler
//!   extend ActiveSupport::Concern
//!   NOT_FOUND = [Gone, Missing].freeze
//!   included do
//!     rescue_from(*NOT_FOUND) { |e| head :not_found }
//!     rescue_from ActionController::ParameterMissing do |exception|
//!       head :bad_request
//!     end
//!   end
//! end
//! ```
//!
//! `collect_rescue_handlers` read only a controller's own body, so the
//! concern's handlers never wrapped the dispatcher and the exception
//! escaped the controller (`Services::MilestonesControllerTest#create_
//! without_milestone_param_responds_with_400`: ParameterMissing raised
//! out of `post`). The handlers are spliced behind the `include`, as the
//! `Unknown` item a controller's own `rescue_from` is, with the module's
//! constants qualified.

use roundhouse::det::HashMap;
use std::path::PathBuf;

use roundhouse::ingest::ingest_app_from_tree;

const CONCERN: &str = r#"module ApplicationControllerExceptionHandler
  extend ActiveSupport::Concern

  NOT_FOUND_ERRORS = [ActiveRecord::RecordNotFound].freeze

  included do
    rescue_from(*NOT_FOUND_ERRORS) do |exception|
      head(:not_found)
    end

    rescue_from ActionController::ParameterMissing do |exception|
      render_problem(:bad_request, "missing #{exception.param}")
    end
  end

  def render_problem(status, text)
    render(plain: text, status: status)
  end
end
"#;

const APPLICATION_CONTROLLER: &str =
    "class ApplicationController < ActionController::Base\n  include ApplicationControllerExceptionHandler\nend\n";

const WIDGETS_CONTROLLER: &str = "class WidgetsController < ApplicationController\n  def create\n    params.require(:widget)\n    head :created\n  end\nend\n";

fn emitted(controller_suffix: &str) -> String {
    let tree: HashMap<PathBuf, Vec<u8>> = [
        ("app/controllers/concerns/application_controller_exception_handler.rb", CONCERN),
        ("app/controllers/application_controller.rb", APPLICATION_CONTROLLER),
        ("app/controllers/widgets_controller.rb", WIDGETS_CONTROLLER),
        (
            "config/routes.rb",
            "Rails.application.routes.draw do\n  resources :widgets, only: [:create]\nend\n",
        ),
    ]
    .into_iter()
    .map(|(p, s)| (PathBuf::from(p), s.as_bytes().to_vec()))
    .collect();
    let mut app = ingest_app_from_tree(tree).expect("ingest");
    roundhouse::session::analyze_and_lower(&mut app);
    let files = roundhouse::emit::ruby::emit_lowered_controllers(&app);
    files
        .iter()
        .find(|f| f.path.to_string_lossy().ends_with(controller_suffix))
        .map(|f| f.content.clone())
        .unwrap_or_else(|| panic!("{controller_suffix} not emitted"))
}

#[test]
fn an_included_block_rescue_from_wraps_the_including_controller_s_dispatcher() {
    let src = emitted("widgets_controller.rb");
    assert!(
        src.contains("rescue ActionController::ParameterMissing"),
        "the concern's rescue_from must wrap process_action:\n{src}"
    );
    assert!(
        src.contains("rescue *ApplicationControllerExceptionHandler::NOT_FOUND_ERRORS"),
        "the splatted module constant must be qualified for the includer:\n{src}"
    );
    assert!(src.contains("render_problem("), "the handler body must carry over:\n{src}");
    // Last declared wins: ParameterMissing is declared after the splat.
    assert!(
        src.find("rescue ActionController::ParameterMissing")
            < src.find("rescue *ApplicationControllerExceptionHandler"),
        "handlers are matched in reverse declaration order:\n{src}"
    );
}

const BLOCK_CONSTANT_CONCERN: &str = r#"module AdminHandler
  extend ActiveSupport::Concern

  included do
    INVALID_PARAMETERS = [ActionController::ParameterMissing].freeze
    rescue_from(*INVALID_PARAMETERS) do |exception|
      head(:bad_request)
    end
  end
end
"#;

/// The constant a concern's `included do` block writes is the concern's
/// (the block keeps its lexical scope), and the `rescue_from` spliced into
/// each includer names it by the concern's path: the module has to carry
/// it, def-less block or not
/// (`AdminAreaControllerExceptionHandler::INVALID_PARAMETERS_EXCEPTIONS`:
/// `uninitialized constant` out of every including controller's
/// `process_action`).
#[test]
fn a_constant_written_in_a_defless_included_block_is_the_concern_s() {
    let tree: HashMap<PathBuf, Vec<u8>> = [
        ("app/controllers/concerns/admin_handler.rb", BLOCK_CONSTANT_CONCERN),
        (
            "app/controllers/application_controller.rb",
            "class ApplicationController < ActionController::Base\n  include AdminHandler\nend\n",
        ),
        ("app/controllers/widgets_controller.rb", WIDGETS_CONTROLLER),
        (
            "config/routes.rb",
            "Rails.application.routes.draw do\n  resources :widgets, only: [:create]\nend\n",
        ),
    ]
    .into_iter()
    .map(|(p, s)| (PathBuf::from(p), s.as_bytes().to_vec()))
    .collect();
    let mut app = ingest_app_from_tree(tree).expect("ingest");
    roundhouse::session::analyze_and_lower(&mut app);
    let controllers = roundhouse::emit::ruby::emit_lowered_controllers(&app);
    let widgets = controllers
        .iter()
        .find(|f| f.path.to_string_lossy().ends_with("widgets_controller.rb"))
        .expect("widgets controller");
    assert!(
        widgets.content.contains("rescue *AdminHandler::INVALID_PARAMETERS"),
        "the splat reads the concern's constant:\n{}",
        widgets.content
    );
    let models = roundhouse::emit::ruby::emit_library(&app);
    let module = models
        .iter()
        .find(|f| f.path.to_string_lossy().ends_with("admin_handler.rb"))
        .unwrap_or_else(|| panic!("module not emitted: {:?}", models.iter().map(|f| &f.path).collect::<Vec<_>>()));
    assert!(
        module.content.contains("INVALID_PARAMETERS = "),
        "the module must define the constant its includers read:\n{}",
        module.content
    );
}
