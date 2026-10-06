# frozen_string_literal: true
#
# Roundhouse database-configuration manifest extractor (manifest version 1).
#
# Writes Rails' OWN resolved answer to "which databases does this
# environment have, and which schema file does each one load", as JSON
# data, for `roundhouse.yml`'s `database_configurations:` key. It is the
# same kind of input as Gemfile.lock: a resolution Rails performs at boot,
# recorded once, pinned by hashes, and never re-derived by Roundhouse.
#
# Run it inside the app, under exactly the environment being described:
#
#   RAILS_ENV=test DB_ADAPTER=postgresql bin/rails runner \
#     /path/to/roundhouse/scripts/rails-database-manifest.rb -- \
#     --out tmp/roundhouse-databases.json [options]
#
# Options:
#   --out PATH                 output file (refuses to overwrite)
#   --input NAME               record ENV[NAME] as a resolution input (repeatable;
#                              RAILS_ENV, RACK_ENV, DB_ADAPTER, SCHEMA and
#                              TEST_ENV_NUMBER are always recorded). A
#                              credential-bearing input (a name like *PASSWORD*,
#                              *TOKEN*, *SECRET*, *CREDENTIAL*, *_KEY, *_URL, *_DSN,
#                              or a value carrying URL userinfo) is withheld: only
#                              its NAME is recorded, under withheld_inputs
#   --config-file PATH         a configuration file the resolution read, beyond
#                              the one Rails names (repeatable); hashed
#   --schema-source NAME=PATH:FORMAT[:NOTE]
#                              the schema file the app's AUTHORED setup loads for
#                              config NAME when that is not Rails' own
#                              `schema_dump_path` (e.g. `schema_dump: false` with a
#                              versioned dump the app's rake task loads). FORMAT is
#                              ruby or sql. NOTE says who resolved it. Roundhouse
#                              never infers this filename.
#   --source-revision REV      source revision (default: `git rev-parse HEAD`)
#   --source-tree TREE         source tree id (default: `git rev-parse HEAD^{tree}`)
#
# What it records, and what it never records:
#   * per configuration: name, adapter, primary?, replica?, database_tasks?,
#     schema_format, schema_dump (as configured), schema_dump_path (Rails'
#     DatabaseTasks.schema_dump_path), the selected schema source with its
#     sha256, pragmas, schema_search_path, migrations_paths;
#   * provenance: source revision/tree, Gemfile.lock sha256 (required), this
#     script's path/sha256/version, Ruby and Rails versions, the env inputs
#     above, the database configuration file Rails loaded with its sha256
#     (required: a configuration from DATABASE_URL alone aborts the run), the
#     sha256 of every other configuration file named, and the NAMES (never
#     the values) of every withheld input and of any set `*DATABASE_URL`
#     variable, which Rails merges in;
#   * NEVER: database names/paths, hosts, ports, usernames, passwords, URLs,
#     or any other key of the configuration hash; never a Proc or other code.
#     A value that is not plain data, a pragma whose name is credential-like,
#     and any recorded string carrying URL userinfo abort the run.
#
# Freshness is NOT guaranteed. The hashes cover the files named above. A
# database.yml rendered through ERB can read anything (ENV, other files,
# app constants); those inputs are not hashed. The env inputs recorded
# here are the ones known to matter; regenerate the manifest whenever any
# input of the resolution changes.

require "json"
require "digest"
require "pathname"

module RoundhouseDatabaseManifest
  VERSION = 1
  DEFAULT_INPUTS = %w[RAILS_ENV RACK_ENV DB_ADAPTER SCHEMA TEST_ENV_NUMBER].freeze
  SCALAR = [String, Integer, Float, TrueClass, FalseClass, NilClass, Symbol].freeze
  # Names whose values are credentials, or locate a server with them. The
  # loader (src/ingest/database_manifest.rs) refuses the same shapes.
  CREDENTIAL_NAME = /PASSWORD|PASSWD|PASSPHRASE|TOKEN|SECRET|CREDENTIAL|PRIVATE|(?:\A|_)KEY\z|(?:\A|_)URL\z|(?:\A|_)DSN\z/i
  # `scheme://user[:password]@`.
  USERINFO = %r{[a-z][a-z0-9+.\-]*://[^/?#\s]*@}i

  module_function

  def fail!(message)
    warn "rails-database-manifest: #{message}"
    exit 2
  end

  def parse_args(argv)
    args = argv.dup
    args.shift if args.first == "--"
    opts = { inputs: [], config_files: [], schema_sources: {} }
    until args.empty?
      flag = args.shift
      value = args.shift or fail!("#{flag} needs a value")
      case flag
      when "--out" then opts[:out] = value
      when "--input" then opts[:inputs] << value
      when "--config-file" then opts[:config_files] << value
      when "--source-revision" then opts[:source_revision] = value
      when "--source-tree" then opts[:source_tree] = value
      when "--schema-source"
        name, spec = value.split("=", 2)
        path, format, note = spec.to_s.split(":", 3)
        fail!("--schema-source wants NAME=PATH:FORMAT[:NOTE], got #{value.inspect}") if name.to_s.empty? || path.to_s.empty?
        fail!("--schema-source format must be ruby or sql, got #{format.inspect}") unless %w[ruby sql].include?(format)
        opts[:schema_sources][name] = { path: path, format: format, note: note }
      else
        fail!("unknown option #{flag}")
      end
    end
    fail!("--out is required") unless opts[:out]
    opts
  end

  def root
    @root ||= Pathname.new(Rails.root.to_s).realpath
  end

  # App-relative path, refusing anything outside the app root.
  def relative(path)
    full = Pathname.new(path.to_s)
    full = root.join(full) unless full.absolute?
    full = full.cleanpath
    rel = full.relative_path_from(root).to_s
    fail!("#{path} is outside the app root #{root}") if rel.start_with?("..")
    rel
  end

  def sha256_of(rel)
    full = root.join(rel)
    full.file? ? Digest::SHA256.file(full.to_s).hexdigest : nil
  end

  def credential_name?(name)
    name.to_s.match?(CREDENTIAL_NAME)
  end

  def plain!(value, what)
    case value
    when Hash
      value.each_with_object({}) do |(k, v), out|
        fail!("#{what}.#{k} has a credential-like name; refusing to record it") if credential_name?(k)
        out[k.to_s] = plain!(v, "#{what}.#{k}")
      end
    when Array
      value.map.with_index { |v, i| plain!(v, "#{what}[#{i}]") }
    when *SCALAR
      fail!("#{what} carries URL userinfo; refusing to record it") if value.to_s.match?(USERINFO)
      value.is_a?(Symbol) ? value.to_s : value
    else
      fail!("#{what} is a #{value.class}, not plain data; refusing to record it")
    end
  end

  def git(*args)
    out = IO.popen(["git", "-C", root.to_s, *args], err: File::NULL, &:read).to_s.strip
    $?.success? && !out.empty? ? out : nil
  rescue SystemCallError
    nil
  end

  def configuration(db_config, opts)
    hash = db_config.configuration_hash
    format = db_config.schema_format.to_s
    dump_path = ActiveRecord::Tasks::DatabaseTasks.schema_dump_path(db_config, db_config.schema_format)
    dump_path &&= relative(dump_path)
    declared = opts[:schema_sources].delete(db_config.name)
    source =
      if declared
        rel = relative(declared[:path])
        { "path" => rel, "format" => declared[:format], "sha256" => sha256_of(rel),
          "resolved_by" => "declared: #{declared[:note] || "--schema-source"}" }
      elsif db_config.database_tasks? && dump_path
        { "path" => dump_path, "format" => format, "sha256" => sha256_of(dump_path),
          "resolved_by" => "rails: ActiveRecord::Tasks::DatabaseTasks.schema_dump_path" }
      end
    {
      "name" => db_config.name,
      "adapter" => db_config.adapter.to_s,
      "primary" => db_config.primary? ? true : false,
      "replica" => db_config.replica? ? true : false,
      "database_tasks" => db_config.database_tasks? ? true : false,
      "schema_format" => format,
      "schema_dump" => hash.key?(:schema_dump) ? plain!(hash[:schema_dump], "#{db_config.name}.schema_dump") : nil,
      "schema_dump_path" => dump_path,
      "schema_source" => source,
      "schema_search_path" => plain!(hash[:schema_search_path], "#{db_config.name}.schema_search_path"),
      "pragmas" => plain!(hash[:pragmas] || {}, "#{db_config.name}.pragmas"),
      "migrations_paths" => Array(db_config.migrations_paths).map { |p| relative(p) },
    }
  end

  def run(argv)
    opts = parse_args(argv)
    out = Pathname.new(opts[:out])
    out = root.join(out) unless out.absolute?
    fail!("#{out} exists; refusing to overwrite") if out.exist?

    env = Rails.env.to_s
    configs = ActiveRecord::Base.configurations.configs_for(env_name: env, include_hidden: true)
    fail!("no database configurations for env #{env}") if configs.empty?
    configurations = configs.map { |c| configuration(c, opts) }
    unless opts[:schema_sources].empty?
      fail!("--schema-source names configurations this env does not have: #{opts[:schema_sources].keys.join(", ")}")
    end

    # The file Rails' `database_configuration` reads: the first existent
    # `paths["config/database"]`. Without one Rails resolves from
    # DATABASE_URL alone, which no file pins.
    loaded = Array(Rails.application.config.paths["config/database"].existent).first
    fail!("Rails loaded no database configuration file (DATABASE_URL alone?); nothing can pin it") unless loaded
    database_config = relative(loaded)
    config_files = opts[:config_files].map { |p| relative(p) }.uniq - [database_config]
    missing = config_files.reject { |p| root.join(p).file? }
    fail!("configuration files not found: #{missing.join(", ")}") unless missing.empty?

    lock = Bundler.default_lockfile rescue nil
    fail!("no Gemfile.lock; the bundle Rails resolved under cannot be pinned") unless lock && File.file?(lock.to_s)
    lock_rel = relative(lock.to_s)
    script = Pathname.new(__FILE__).realpath

    inputs = {}
    withheld = ENV.keys.grep(/DATABASE_URL\z/)
    (DEFAULT_INPUTS + opts[:inputs]).uniq.each do |name|
      value = ENV[name]
      if credential_name?(name) || value.to_s.match?(USERINFO)
        withheld << name
      else
        inputs[name] = value
      end
    end

    manifest = {
      "roundhouse_database_manifest" => VERSION,
      "env" => env,
      "inputs" => inputs,
      "configurations" => configurations,
      "provenance" => {
        "source_revision" => opts[:source_revision] || git("rev-parse", "HEAD"),
        "source_tree" => opts[:source_tree] || git("rev-parse", "HEAD^{tree}"),
        "gemfile_lock" => { "path" => lock_rel, "sha256" => sha256_of(lock_rel) },
        "extractor" => { "path" => script.to_s, "sha256" => Digest::SHA256.file(script.to_s).hexdigest, "version" => VERSION },
        "ruby_version" => "#{RUBY_VERSION} (#{RUBY_PLATFORM})",
        "rails_version" => Rails.version.to_s,
        "command" => (["bin/rails", "runner", script.to_s] + argv).join(" "),
        "database_config" => { "path" => database_config, "sha256" => sha256_of(database_config) },
        "config_files" => config_files.map { |p| { "path" => p, "sha256" => sha256_of(p) } },
        # Names only: their values are credentials or carry them (a
        # `*DATABASE_URL` is merged into the configuration by Rails).
        "withheld_inputs" => withheld.uniq.sort,
        "unhashed_inputs" => [
          "ERB in the configuration files can read ENV beyond `inputs`, other files and app constants; none of those are hashed",
          "regenerate this manifest whenever any input of the resolution changes; the hashes do not guarantee freshness",
        ],
      },
    }
    out.dirname.mkpath
    File.open(out.to_s, File::WRONLY | File::CREAT | File::EXCL) { |f| f.write(JSON.pretty_generate(manifest) + "\n") }
    puts "rails-database-manifest: wrote #{out} (#{configurations.size} configuration(s) for #{env})"
  end
end

RoundhouseDatabaseManifest.run(ARGV)
