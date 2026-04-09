mod config;
#[cfg(feature = "docker")]
mod docker;
mod fs;
mod git;
mod json;
#[cfg(feature = "k8s")]
mod k8s;
#[cfg(feature = "lxc")]
mod lxc;
mod output;
#[cfg(feature = "sqlite")]
mod sqlite;
mod value;

use std::process::ExitCode;
use std::sync::LazyLock;

use clap::{Parser, Subcommand};

/// Built at startup so optional-domain examples only appear when the
/// matching cargo feature is enabled. A `--no-default-features` build
/// would otherwise advertise commands that don't exist in the binary.
static QUICK_START: LazyLock<String> = LazyLock::new(|| {
    #[cfg_attr(
        not(any(
            feature = "k8s",
            feature = "lxc",
            feature = "docker",
            feature = "sqlite"
        )),
        allow(unused_mut)
    )]
    let mut s = String::from(
        "Quick start:
  sak fs glob '**/*.rs'                       Find all Rust files
  sak fs grep 'fn main' src/                  Search for a pattern
  sak fs read src/main.rs -n 1-20             Read lines 1-20 of a file
  sak fs cut -d: -f 1 /etc/passwd             Extract first field
  sak git status                              Show working tree status
  sak git log --oneline -n 10                 Recent commits
  sak git diff --staged                       Show staged changes
  sak git blame src/main.rs                   Line-by-line authorship
  sak json query .name data.json              Extract a JSON value
  sak json keys --types data.json             List keys with value types
  sak json flatten data.json                  Flatten to path<TAB>value
  sak json validate data.json                 Check JSON validity
  sak config query .package.name Cargo.toml   Read TOML/YAML/plist values
  sak config keys --types config.yaml         List config keys with types
  sak config flatten Info.plist               Flatten any config file
  sak config validate config.toml             Check syntax validity",
    );
    #[cfg(feature = "k8s")]
    s.push_str(
        "
  sak k8s contexts                            List kubeconfig contexts
  sak k8s kinds                               List group/version/kinds
  sak k8s get pods -n kube-system             List resources of a kind
  sak k8s images deployment/foo               Container images on a workload
  sak k8s schema deployment.apps/v1           OpenAPI v3 schema for a kind
  sak k8s restarts -A                         Pod containers with restarts
  sak k8s failing -A                          Pods not Running or Succeeded
  sak k8s pending -A                          Pods stuck in Pending",
    );
    #[cfg(feature = "lxc")]
    s.push_str(
        "
  sak lxc list                                List LXD/Incus instances
  sak lxc info my-ct                          Full metadata for an instance
  sak lxc images                              List images on the daemon",
    );
    #[cfg(feature = "docker")]
    s.push_str(
        "
  sak docker list                             List containers
  sak docker images                           List images
  sak docker info my-container                Full metadata for a container",
    );
    #[cfg(feature = "sqlite")]
    s.push_str(
        "
  sak sqlite tables app.db                    List tables in a SQLite file
  sak sqlite schema app.db                    Show database schema
  sak sqlite count users app.db               Count rows in a table
  sak sqlite query 'SELECT * FROM users' app.db   Run a read-only query
  sak sqlite info app.db                      Database-level metadata",
    );
    s
});

#[derive(Parser)]
#[command(
    name = "sak",
    version,
    about = "Swiss Army Knife for LLMs — read-only operations",
    long_about = "Swiss Army Knife for LLMs — a collection of read-only operations \
        designed for use by language models.\n\n\
        All operations are strictly read-only with no side effects. \
        Commands are organized by domain (e.g., fs for filesystem). \
        Use `sak <domain> --help` to explore available operations, \
        or `sak <domain> <command> --help` for detailed usage.",
    after_help = QUICK_START.as_str()
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Filesystem operations (read-only)
    #[command(subcommand)]
    Fs(fs::FsCommand),
    /// Git repository operations (read-only)
    #[command(subcommand)]
    Git(git::GitCommand),
    /// JSON operations (read-only)
    #[command(subcommand)]
    Json(json::JsonCommand),
    /// Config file operations — TOML, YAML, plist (read-only)
    #[command(subcommand)]
    Config(config::ConfigCommand),
    /// Kubernetes operations against a live cluster (read-only)
    #[cfg(feature = "k8s")]
    #[command(subcommand)]
    K8s(k8s::K8sCommand),
    /// LXD/Incus container operations against a live daemon (read-only)
    #[cfg(feature = "lxc")]
    #[command(subcommand)]
    Lxc(lxc::LxcCommand),
    /// Docker container operations against a live daemon (read-only)
    #[cfg(feature = "docker")]
    #[command(subcommand)]
    Docker(docker::DockerCommand),
    /// SQLite database operations (read-only)
    #[cfg(feature = "sqlite")]
    #[command(subcommand)]
    Sqlite(sqlite::SqliteCommand),
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    let result = match &cli.command {
        Command::Fs(cmd) => fs::run(cmd),
        Command::Git(cmd) => git::run(cmd),
        Command::Json(cmd) => json::run(cmd),
        Command::Config(cmd) => config::run(cmd),
        #[cfg(feature = "k8s")]
        Command::K8s(cmd) => k8s::run(cmd),
        #[cfg(feature = "lxc")]
        Command::Lxc(cmd) => lxc::run(cmd),
        #[cfg(feature = "docker")]
        Command::Docker(cmd) => docker::run(cmd),
        #[cfg(feature = "sqlite")]
        Command::Sqlite(cmd) => sqlite::run(cmd),
    };

    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("sak: error: {:#}", e);
            ExitCode::from(2)
        }
    }
}
