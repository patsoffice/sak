mod cert;
mod config;
mod csv;
#[cfg(feature = "docker")]
mod docker;
mod fs;
mod gh;
mod git;
mod hook;
mod json;
#[cfg(feature = "k8s")]
mod k8s;
mod linux;
#[cfg(feature = "lxc")]
mod lxc;
mod output;
#[cfg(feature = "prom")]
mod prom;
#[cfg(feature = "sqlite")]
mod sqlite;
mod talos;
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
            feature = "sqlite",
            feature = "prom"
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
  sak json select .name,.age data.json        Project a subset of fields
  sak json flatten data.json                  Flatten to path<TAB>value
  sak json paths data.json                    List leaf paths (no values)
  sak json diff a.json b.json                 Structural diff of two JSON files
  sak json validate data.json                 Check JSON validity
  sak config query .package.name Cargo.toml   Read TOML/YAML/plist values
  sak config keys --types config.yaml         List config keys with types
  sak config flatten Info.plist               Flatten any config file
  sak config paths config.yaml                List leaf paths (no values)
  sak config diff a.toml b.yaml               Cross-format structural diff
  sak config validate config.toml             Check syntax validity
  sak csv headers data.csv                    List CSV column names and indices
  sak csv query -c name,age data.csv          Project columns and filter rows
  sak csv stats data.csv                      Summary statistics per column
  sak csv validate data.csv                   Check CSV structure / parse errors
  sak cert inspect cert.pem                   Show subject, dates, SANs, fingerprint
  sak cert expiring --days 30 *.pem           Certs expiring within a window
  sak cert from-kubeconfig ~/.kube/config     Inspect kubeconfig client certs
  sak talos certs                             Cert inventory across all Talos nodes
  sak talos read /etc/os-release              Fan-out file read across nodes
  sak talos get members --node 192.168.1.10   COSI resource from one node
  sak gh api repos/cli/cli                     GET a GitHub REST/GraphQL endpoint
  sak gh pr-list --state open                  List pull requests as TSV
  sak gh pr-view 123                            Show one PR's metadata (JSON/TSV)
  sak gh issue-list --label bug                List issues as TSV
  sak gh issue-view 123                         Show one issue's metadata (JSON/TSV)
  sak gh run-list --workflow ci.yml            List CI workflow runs as TSV
  sak gh run-view 123 --log-failed             Show a run's metadata or logs
  sak gh release-list                          List releases as TSV
  sak gh release-view v1.2.3                    Show one release's metadata (JSON/TSV)
  sak gh workflow-list                         List workflow definitions as TSV
  sak gh repo-view cli/cli                      Show repository metadata (JSON/TSV)
  sak linux cpuinfo                           Parsed /proc/cpuinfo, one row per CPU
  sak linux meminfo                           Parsed /proc/meminfo as key<TAB>value_kb
  sak linux mounts --type ext4                Mount table from /proc/self/mountinfo
  sak hook claude-code                        Pre-tool-use hook for Claude Code (reads stdin)",
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
  sak k8s pending -A                          Pods stuck in Pending
  sak k8s events -A --limit 20                Recent cluster events
  sak k8s describe deploy api -n api          Aggregated description of one resource
  sak k8s logs web-0 -n web --tail 50         Last 50 log lines from a pod",
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
    #[cfg(feature = "prom")]
    s.push_str(
        "
  sak prom alerts --url http://prom:9090      Firing+pending alerts
  sak prom alerts --all --name 'Cert.*'       Alerts filtered by name regex
  sak prom query 'up'                         Run a PromQL instant query
  sak prom query-range 'up' --since 1h        Range query over the last hour
  sak prom histogram apiserver_request_duration_seconds   Pretty-print buckets
  sak prom targets --down                     Unhealthy scrape targets
  sak prom rules --firing                     Currently-firing rules
  sak prom labels                             List all label names
  sak prom label-values namespace             Values of one label
  sak prom series 'up'                        Series matching a selector
  sak prom metadata up                        Type/help/unit for a metric
  sak prom tsdb-stats                         Top-K cardinality offenders
  sak prom flags                              Daemon command-line flags
  sak prom config                             Daemon YAML config
  sak prom am alerts                          Alertmanager active alerts
  sak prom am silences                        Alertmanager active silences",
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
    /// CSV operations (read-only)
    #[command(subcommand)]
    Csv(csv::CsvCommand),
    /// X.509 certificate inspection (read-only)
    #[command(subcommand)]
    Cert(cert::CertCommand),
    /// Talos Linux cluster operations (read-only, shells out to talosctl)
    #[command(subcommand)]
    Talos(talos::TalosCommand),
    /// GitHub CLI operations (read-only, shells out to gh)
    #[command(subcommand)]
    Gh(gh::GhCommand),
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
    /// Prometheus / Alertmanager HTTP API operations (read-only)
    #[cfg(feature = "prom")]
    #[command(subcommand)]
    Prom(prom::PromCommand),
    /// Linux /proc system-state inspection (read-only, Linux-only)
    #[command(subcommand)]
    Linux(linux::LinuxCommand),
    /// LLM-agent harness integration hooks (read-only command classification)
    #[command(subcommand)]
    Hook(hook::HookCommand),
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    let result = match &cli.command {
        Command::Fs(cmd) => fs::run(cmd),
        Command::Git(cmd) => git::run(cmd),
        Command::Json(cmd) => json::run(cmd),
        Command::Config(cmd) => config::run(cmd),
        Command::Csv(cmd) => csv::run(cmd),
        Command::Cert(cmd) => cert::run(cmd),
        Command::Talos(cmd) => talos::run(cmd),
        Command::Gh(cmd) => gh::run(cmd),
        #[cfg(feature = "k8s")]
        Command::K8s(cmd) => k8s::run(cmd),
        #[cfg(feature = "lxc")]
        Command::Lxc(cmd) => lxc::run(cmd),
        #[cfg(feature = "docker")]
        Command::Docker(cmd) => docker::run(cmd),
        #[cfg(feature = "sqlite")]
        Command::Sqlite(cmd) => sqlite::run(cmd),
        #[cfg(feature = "prom")]
        Command::Prom(cmd) => prom::run(cmd),
        Command::Linux(cmd) => linux::run(cmd),
        Command::Hook(cmd) => hook::run(cmd),
    };

    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("sak: error: {:#}", e);
            ExitCode::from(2)
        }
    }
}
