/// Semantic safety classification for shell commands.
///
/// `analyze_command` returns the worst-case `SafetyVerdict` across all
/// pipeline/sequence segments in a compound command.  The sandbox uses this
/// to auto-allow commands that are provably read-only without requiring the
/// user to enumerate every safe command in their allowlist.
///
/// Guard: auto-allow is suppressed for commands that contain `$`, backtick,
/// or `(` (shell expansion operators) — those require the full runtime to
/// evaluate safely, so they fall back to the normal prompt flow.

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SafetyLevel {
    /// Read-only or routinely harmless — agent can proceed without prompting.
    Safe,
    /// Modifies state but changes are scoped or reversible — prompt the user.
    Caution,
    /// Irreversible data loss or system integrity risk — always prompt.
    Destructive,
}

#[derive(Debug, Clone)]
pub struct SafetyVerdict {
    pub level: SafetyLevel,
    pub reason: &'static str,
}

impl SafetyVerdict {
    pub fn label(&self) -> &'static str {
        match self.level {
            SafetyLevel::Safe => "safe",
            SafetyLevel::Caution => "caution",
            SafetyLevel::Destructive => "destructive",
        }
    }

    fn safe(reason: &'static str) -> Self {
        Self { level: SafetyLevel::Safe, reason }
    }
    fn caution(reason: &'static str) -> Self {
        Self { level: SafetyLevel::Caution, reason }
    }
    fn destructive(reason: &'static str) -> Self {
        Self { level: SafetyLevel::Destructive, reason }
    }
}

/// Commands whose heads are definitively read-only with no persistent side effects.
const ALWAYS_SAFE: &[&str] = &[
    // directory listing
    "ls", "dir", "vdir",
    // file reading
    "cat", "tac", "less", "more", "head", "tail", "bat", "strings",
    "hexdump", "xxd", "od",
    // text processing (all read stdin/files, write only to stdout)
    "grep", "egrep", "fgrep", "rg", "ag", "ack",
    "sort", "uniq", "wc", "cut", "tr", "nl", "fold", "fmt",
    "awk", "column", "paste", "join", "comm", "diff", "cmp",
    // system info
    "pwd", "whoami", "id", "hostname", "uname", "arch",
    "date", "cal", "uptime",
    "ps", "pgrep", "pidof",
    "df", "du", "free", "nproc",
    // environment inspection
    "env", "printenv",
    // file metadata
    "file", "stat", "readlink", "realpath",
    // hashing / verification
    "md5sum", "sha1sum", "sha256sum", "sha512sum", "md5", "shasum", "cksum",
    // path / command lookup
    "which", "type",
    // output primitives
    "echo", "printf", "true", "false",
    // data format tools
    "jq", "yq", "base64",
    // help / documentation
    "man", "info",
    // process inspection (list-only modes)
    "lsof",
    // test builtins
    "test", "[",
];

/// Commands that always risk irreversible data loss or system integrity damage.
const ALWAYS_DESTRUCTIVE: &[&str] = &[
    "dd", "mkfs", "fdisk", "parted", "gdisk", "sgdisk",
    "wipefs", "blkdiscard", "shred", "wipe", "srm",
];

/// Commands that modify state but are typically reversible or project-scoped.
/// Build-tool CLIs (npm, pip, go, …) are NOT listed here — they are handled
/// by `analyze_build_tool` / `analyze_go` so individual subcommands like
/// `npm test` or `go build` can be auto-allowed.
const ALWAYS_CAUTION: &[&str] = &[
    "mv", "cp", "mkdir", "touch", "ln", "install",
    "chmod", "chown", "chgrp",
    "kill", "killall", "pkill",
    // make/cmake/ninja: Makefile rules are opaque — cannot classify statically.
    "make", "cmake", "ninja",
    "apt", "apt-get", "brew", "yum", "dnf", "pacman", "snap",
    "systemctl", "service", "launchctl",
    "crontab", "at",
    "ssh-keygen",
    "openssl",
    "tar", "zip", "unzip", "gzip", "gunzip", "bzip2", "xz",
    "patch",
    "rsync",
    "docker", "podman", "kubectl",
    "terraform", "ansible",
    "truncate",
    "tee",
    "python", "python3", "ruby", "node", "perl",
];

// ── Ecosystem-agnostic build-tool classification ──────────────────────────────
//
// Many CLIs share the same subcommand vocabulary: "test" is always read-ish,
// "publish" always risks external state.  We classify by subcommand first so
// `npm test`, `go test`, `mvn test`, `cargo test` are all auto-allowed, while
// `npm publish` or `pip install` still prompt.

/// Subcommands that are safe across build/package managers — they compile,
/// run tests, lint, or report without mutating the broader environment.
const BUILD_SAFE: &[&str] = &[
    "test", "tests",
    "build",
    "check",
    "lint", "fmt", "format",
    "doc", "docs",
    "bench", "benchmark",
    "audit",
    "verify", "validate",
    "compile",
    "typecheck", "type-check",
    "watch",
    // read-only introspection
    "list", "ls", "show", "info", "outdated", "tree",
    "freeze", "why", "explain", "search",
    "query", "inspect",
    "vet",   // go vet
    "env",   // go env, cargo env
    "version",
];

/// Subcommands that install, remove, or publish packages — always caution.
const BUILD_CAUTION: &[&str] = &[
    "install", "add", "i",
    "uninstall", "remove", "rm", "r", "delete",
    "update", "upgrade", "up",
    "publish", "pack", "release",
    "deploy",
    "get",   // go get
    "tidy",  // go mod tidy (rewrites go.sum)
    "link", "unlink",
];

/// Classify a generic build-tool invocation (npm, yarn, pnpm, bun, pip, pip3,
/// pip2, mvn, gradle, gradlew, mvnw, deno, …) by its first subcommand.
/// `npm run <script>` unwraps one level so `npm run test` → Safe.
fn analyze_build_tool(args: &[String]) -> SafetyVerdict {
    let sub = args.first().map(String::as_str).unwrap_or("");
    // npm/yarn/pnpm/bun: `run <script>` — treat the script name as the subcommand.
    let effective = if matches!(sub, "run" | "run-script" | "exec") {
        args.get(1).map(String::as_str).unwrap_or(sub)
    } else {
        sub
    };
    if BUILD_SAFE.contains(&effective) {
        SafetyVerdict::safe("build-tool read/test operation")
    } else if BUILD_CAUTION.contains(&effective) {
        SafetyVerdict::caution("build-tool modifying operation")
    } else {
        SafetyVerdict::caution("build-tool command")
    }
}

/// Go-specific analysis — `go build`/`go test`/`go vet`/`go fmt` are safe;
/// `go install`/`go get` modify the module cache.
fn analyze_go(args: &[String]) -> SafetyVerdict {
    let sub = args.first().map(String::as_str).unwrap_or("");
    // `go mod <sub>` — check the mod subcommand.
    if sub == "mod" {
        let mod_sub = args.get(1).map(String::as_str).unwrap_or("");
        return if matches!(mod_sub, "download" | "verify" | "graph" | "why") {
            SafetyVerdict::safe("go mod read operation")
        } else {
            SafetyVerdict::caution("go mod modifying operation")
        };
    }
    // Delegate to the shared classifier — BUILD_SAFE covers build/test/vet/fmt/doc/env.
    analyze_build_tool(args)
}

/// pip-specific analysis — `pip list`/`pip show`/`pip freeze` are read-only;
/// `pip install`/`pip uninstall` modify the environment.
fn analyze_pip(args: &[String]) -> SafetyVerdict {
    // Delegate to the shared classifier — BUILD_SAFE covers list/show/freeze/check.
    analyze_build_tool(args)
}

/// `git` read-only subcommands — safe to auto-allow.
const GIT_SAFE: &[&str] = &[
    "status", "log", "diff", "show", "branch", "tag", "remote",
    "describe", "rev-parse", "rev-list", "ls-files", "ls-tree",
    "shortlog", "blame", "annotate", "config", "fetch", "stash",
    "bisect", "notes",
];

/// `cargo` subcommands that are safe in a development context.
const CARGO_SAFE: &[&str] = &[
    "check", "build", "test", "clippy", "fmt", "doc", "bench",
    "tree", "metadata", "pkgid", "locate-project", "verify-project",
    "run",
];

/// Strip path prefix so `/bin/rm` and `../rm` both yield `rm`.
fn basename(s: &str) -> &str {
    s.rfind('/').map_or(s, |i| &s[i + 1..])
}

/// Split a compound command into (head, args) segments.
/// Splits at `;`, `|`, `&`, and newline — not at spaces — so arguments
/// stay attached to their command head for flag analysis.
fn parse_segments(command: &str) -> Vec<(String, Vec<String>)> {
    command
        .split([';', '|', '&', '\n'])
        .filter_map(|seg| {
            let mut it = seg.split_whitespace();
            // Skip leading env assignments: FOO=bar CMD → CMD is the head.
            let mut raw_head = it.next()?;
            while raw_head.contains('=') {
                raw_head = it.next()?;
            }
            // Strip subshell markers that survived splitting.
            let raw_head = raw_head
                .trim_start_matches("$(")
                .trim_start_matches('(');
            // Normalise: remove backslash escapes, take basename.
            let unescaped: String = raw_head.chars().filter(|&c| c != '\\').collect();
            let head = basename(&unescaped).to_string();
            if head.is_empty() {
                return None;
            }
            let args: Vec<String> = it.map(|s| s.to_string()).collect();
            Some((head, args))
        })
        .collect()
}

// ── Per-command analysers ─────────────────────────────────────────────────────

fn analyze_rm(args: &[String]) -> SafetyVerdict {
    let short_flags: String = args
        .iter()
        .filter(|a| a.starts_with('-') && !a.starts_with("--"))
        .flat_map(|a| a.chars().skip(1))
        .collect();

    let recursive = short_flags.contains('r')
        || short_flags.contains('R')
        || args.iter().any(|a| a == "--recursive");

    // Targets that imply whole-system deletion.
    const CRITICAL: &[&str] = &["/", "~", "/*", "~/", "/etc", "/usr", "/bin", "/boot", "/sys", "/dev"];
    let critical_target = args
        .iter()
        .filter(|a| !a.starts_with('-'))
        .any(|a| CRITICAL.iter().any(|&c| a == c || a.starts_with(c)));

    if recursive || critical_target {
        SafetyVerdict::destructive("rm -r can permanently delete entire directory trees")
    } else if short_flags.contains('f') || args.iter().any(|a| a == "--force") {
        SafetyVerdict::caution("rm -f deletes without confirmation")
    } else {
        SafetyVerdict::caution("rm deletes files")
    }
}

fn analyze_find(args: &[String]) -> SafetyVerdict {
    let has_delete = args.iter().any(|a| a == "-delete");
    let exec_pos = args.iter().position(|a| a == "-exec" || a == "-execdir");
    let has_exec_rm = exec_pos.is_some_and(|i| {
        args.get(i + 1)
            .map(|cmd| matches!(basename(cmd), "rm" | "shred" | "dd" | "unlink"))
            .unwrap_or(false)
    });
    if has_delete || has_exec_rm {
        SafetyVerdict::destructive("find with -delete or -exec rm removes files permanently")
    } else {
        SafetyVerdict::safe("find without -delete is read-only")
    }
}

fn analyze_git(args: &[String]) -> SafetyVerdict {
    let sub = args.first().map(String::as_str).unwrap_or("");
    let rest: Vec<&str> = args.iter().skip(1).map(String::as_str).collect();

    // Destructive operations first.
    match sub {
        "clean" => {
            return SafetyVerdict::destructive("git clean permanently removes untracked files");
        }
        "reset" if rest.contains(&"--hard") || rest.contains(&"--merge") || rest.contains(&"--keep") => {
            return SafetyVerdict::destructive("git reset --hard discards uncommitted changes");
        }
        "push" if rest.contains(&"--force") || rest.contains(&"-f") => {
            return SafetyVerdict::destructive("git push --force rewrites remote history");
        }
        _ => {}
    }

    if GIT_SAFE.contains(&sub) {
        SafetyVerdict::safe("read-only git operation")
    } else {
        SafetyVerdict::caution("git write operation")
    }
}

fn analyze_cargo(args: &[String]) -> SafetyVerdict {
    let sub = args.first().map(String::as_str).unwrap_or("");
    if CARGO_SAFE.contains(&sub) {
        SafetyVerdict::safe("routine cargo development command")
    } else if matches!(sub, "install" | "uninstall" | "publish" | "yank") {
        SafetyVerdict::caution("cargo modifies the global package store or registry")
    } else {
        SafetyVerdict::caution("cargo command")
    }
}

fn analyze_sed(args: &[String]) -> SafetyVerdict {
    let in_place = args
        .iter()
        .any(|a| a == "-i" || a.starts_with("-i'") || a.starts_with("-i\"") || a == "--in-place");
    if in_place {
        SafetyVerdict::caution("sed -i modifies files in place")
    } else {
        SafetyVerdict::safe("sed without -i only writes to stdout")
    }
}

fn analyze_chmod(args: &[String]) -> SafetyVerdict {
    const SYSTEM: &[&str] = &["/", "/etc", "/usr", "/bin", "/boot", "/sys"];
    let system_target = args
        .iter()
        .filter(|a| !a.starts_with('-'))
        .skip(1) // first non-flag arg is the mode, second+ are paths
        .any(|a| SYSTEM.iter().any(|&s| a == s || a.starts_with(s)));
    if system_target {
        SafetyVerdict::destructive("chmod on system paths can break the OS")
    } else {
        SafetyVerdict::caution("chmod changes file permissions")
    }
}

fn analyze_segment(head: &str, args: &[String]) -> SafetyVerdict {
    match head {
        // ── Per-command analysers ─────────────────────────────────────────────
        "rm" => analyze_rm(args),
        "find" => analyze_find(args),
        "git" => analyze_git(args),
        "cargo" => analyze_cargo(args),
        "sed" => analyze_sed(args),
        "chmod" => analyze_chmod(args),

        // ── Ecosystem-agnostic build tools ────────────────────────────────────
        // Node / JS
        "npm" | "yarn" | "pnpm" | "bun" | "npx" | "bunx" => analyze_build_tool(args),
        // Python
        "pip" | "pip3" | "pip2" | "uv" | "poetry" | "pdm" | "pipenv" => analyze_pip(args),
        // Go
        "go" => analyze_go(args),
        // JVM
        "mvn" | "mvnw" | "gradle" | "gradlew" => analyze_build_tool(args),
        // Ruby
        "bundle" | "bundler" => analyze_build_tool(args),
        // .NET
        "dotnet" => analyze_build_tool(args),
        // Rust (non-cargo frontends)
        "rustup" => analyze_build_tool(args),
        // PHP
        "composer" => analyze_build_tool(args),
        // Swift / Xcode
        "swift" | "xcodebuild" => analyze_build_tool(args),

        // ── mkfs.* variants ───────────────────────────────────────────────────
        _ if head.starts_with("mkfs") => SafetyVerdict::destructive("mkfs variants format filesystems"),

        // ── Static tables ──────────────────────────────────────────────────────
        _ if ALWAYS_SAFE.contains(&head) => SafetyVerdict::safe("read-only command"),
        _ if ALWAYS_DESTRUCTIVE.contains(&head) => SafetyVerdict::destructive("always-destructive command"),
        _ if ALWAYS_CAUTION.contains(&head) => SafetyVerdict::caution("modifying command"),
        _ => SafetyVerdict::caution("unknown command — defaulting to caution"),
    }
}

/// Analyse `command` and return the worst-case `SafetyVerdict` across all
/// pipeline and sequence segments.
///
/// Callers should additionally check `is_expansion_free` before using this
/// verdict to auto-allow — commands with `$`, backtick, or `(` require
/// runtime evaluation that static analysis cannot safely classify.
pub fn analyze_command(command: &str) -> SafetyVerdict {
    let segments = parse_segments(command);
    if segments.is_empty() {
        return SafetyVerdict::safe("empty command");
    }
    segments
        .into_iter()
        .map(|(head, args)| analyze_segment(&head, &args))
        .max_by_key(|v| v.level)
        .unwrap()
}

/// Returns `true` if the command string contains no shell expansion operators
/// (`$`, backtick, `(`).  Only simple commands without expansions can be
/// safely auto-allowed based on static analysis alone.
pub fn is_expansion_free(command: &str) -> bool {
    !command.contains('$') && !command.contains('`') && !command.contains('(')
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn level(cmd: &str) -> SafetyLevel {
        analyze_command(cmd).level
    }

    // ── Always-safe commands ──────────────────────────────────────────────────

    #[test]
    fn ls_is_safe() {
        assert_eq!(level("ls -la"), SafetyLevel::Safe);
        assert_eq!(level("ls --color=auto /tmp"), SafetyLevel::Safe);
    }

    #[test]
    fn cat_is_safe() {
        assert_eq!(level("cat /etc/hosts"), SafetyLevel::Safe);
    }

    #[test]
    fn echo_is_safe() {
        assert_eq!(level("echo hello world"), SafetyLevel::Safe);
    }

    #[test]
    fn grep_is_safe() {
        assert_eq!(level("grep -r TODO ."), SafetyLevel::Safe);
    }

    #[test]
    fn ps_is_safe() {
        assert_eq!(level("ps aux"), SafetyLevel::Safe);
    }

    #[test]
    fn git_status_is_safe() {
        assert_eq!(level("git status"), SafetyLevel::Safe);
    }

    #[test]
    fn git_log_is_safe() {
        assert_eq!(level("git log --oneline -20"), SafetyLevel::Safe);
    }

    #[test]
    fn git_diff_is_safe() {
        assert_eq!(level("git diff HEAD~1"), SafetyLevel::Safe);
    }

    #[test]
    fn cargo_check_is_safe() {
        assert_eq!(level("cargo check"), SafetyLevel::Safe);
    }

    #[test]
    fn cargo_test_is_safe() {
        assert_eq!(level("cargo test -- --nocapture"), SafetyLevel::Safe);
    }

    #[test]
    fn cargo_clippy_is_safe() {
        assert_eq!(level("cargo clippy -- -D warnings"), SafetyLevel::Safe);
    }

    #[test]
    fn find_without_delete_is_safe() {
        assert_eq!(level("find . -name '*.rs'"), SafetyLevel::Safe);
        assert_eq!(level("find /tmp -type f -mtime +7"), SafetyLevel::Safe);
    }

    #[test]
    fn sed_without_i_is_safe() {
        assert_eq!(level("sed 's/foo/bar/g' file.txt"), SafetyLevel::Safe);
    }

    // ── Caution commands ──────────────────────────────────────────────────────

    #[test]
    fn rm_without_r_is_caution() {
        assert_eq!(level("rm single_file.txt"), SafetyLevel::Caution);
        assert_eq!(level("rm -f file.txt"), SafetyLevel::Caution);
    }

    #[test]
    fn mv_is_caution() {
        assert_eq!(level("mv src.txt dst.txt"), SafetyLevel::Caution);
    }

    #[test]
    fn cp_is_caution() {
        assert_eq!(level("cp -r src/ dst/"), SafetyLevel::Caution);
    }

    #[test]
    fn git_commit_is_caution() {
        assert_eq!(level("git commit -m 'feat: add thing'"), SafetyLevel::Caution);
    }

    #[test]
    fn git_push_without_force_is_caution() {
        assert_eq!(level("git push origin main"), SafetyLevel::Caution);
    }

    #[test]
    fn cargo_install_is_caution() {
        assert_eq!(level("cargo install ripgrep"), SafetyLevel::Caution);
    }

    #[test]
    fn sed_with_i_is_caution() {
        assert_eq!(level("sed -i 's/foo/bar/g' file.txt"), SafetyLevel::Caution);
    }

    // ── Destructive commands ──────────────────────────────────────────────────

    #[test]
    fn rm_rf_is_destructive() {
        assert_eq!(level("rm -rf /tmp/x"), SafetyLevel::Destructive);
        assert_eq!(level("rm -fr build/"), SafetyLevel::Destructive);
        assert_eq!(level("rm -r some_dir"), SafetyLevel::Destructive);
    }

    #[test]
    fn rm_root_is_destructive() {
        assert_eq!(level("rm -f /"), SafetyLevel::Destructive);
        assert_eq!(level("rm /etc/passwd"), SafetyLevel::Destructive);
    }

    #[test]
    fn dd_is_destructive() {
        assert_eq!(level("dd if=/dev/zero of=/dev/sda"), SafetyLevel::Destructive);
    }

    #[test]
    fn mkfs_is_destructive() {
        assert_eq!(level("mkfs.ext4 /dev/sdb1"), SafetyLevel::Destructive);
    }

    #[test]
    fn shred_is_destructive() {
        assert_eq!(level("shred -u secret.txt"), SafetyLevel::Destructive);
    }

    #[test]
    fn find_with_delete_is_destructive() {
        assert_eq!(level("find . -name '*.tmp' -delete"), SafetyLevel::Destructive);
    }

    #[test]
    fn find_with_exec_rm_is_destructive() {
        assert_eq!(level("find . -type f -exec rm {} \\;"), SafetyLevel::Destructive);
    }

    #[test]
    fn git_clean_is_destructive() {
        assert_eq!(level("git clean -fd"), SafetyLevel::Destructive);
    }

    #[test]
    fn git_reset_hard_is_destructive() {
        assert_eq!(level("git reset --hard HEAD"), SafetyLevel::Destructive);
    }

    #[test]
    fn git_push_force_is_destructive() {
        assert_eq!(level("git push --force origin main"), SafetyLevel::Destructive);
        assert_eq!(level("git push -f"), SafetyLevel::Destructive);
    }

    #[test]
    fn fdisk_is_destructive() {
        assert_eq!(level("fdisk /dev/sda"), SafetyLevel::Destructive);
    }

    // ── Compound commands: worst level wins ───────────────────────────────────

    #[test]
    fn pipeline_safe_then_destructive_is_destructive() {
        assert_eq!(level("ls -la | rm -rf /tmp"), SafetyLevel::Destructive);
    }

    #[test]
    fn semicolon_safe_and_caution_is_caution() {
        assert_eq!(level("echo done; mv a b"), SafetyLevel::Caution);
    }

    #[test]
    fn all_safe_pipeline_is_safe() {
        assert_eq!(level("cat file.txt | grep TODO | wc -l"), SafetyLevel::Safe);
    }

    #[test]
    fn git_status_then_commit_is_caution() {
        assert_eq!(level("git status && git commit -m 'x'"), SafetyLevel::Caution);
    }

    // ── Expansion-free guard ──────────────────────────────────────────────────

    #[test]
    fn simple_commands_are_expansion_free() {
        assert!(is_expansion_free("ls -la"));
        assert!(is_expansion_free("git status"));
        assert!(is_expansion_free("cargo test"));
    }

    #[test]
    fn commands_with_dollar_are_not_expansion_free() {
        assert!(!is_expansion_free("echo $HOME"));
        assert!(!is_expansion_free("ls $(pwd)"));
    }

    #[test]
    fn commands_with_backtick_are_not_expansion_free() {
        assert!(!is_expansion_free("echo `whoami`"));
    }

    #[test]
    fn commands_with_parens_are_not_expansion_free() {
        assert!(!is_expansion_free("(ls -la)"));
    }

    // ── Ecosystem-agnostic build tools ────────────────────────────────────────

    #[test]
    fn npm_test_is_safe() {
        assert_eq!(level("npm test"), SafetyLevel::Safe);
        assert_eq!(level("npm run test"), SafetyLevel::Safe);
        assert_eq!(level("npm run lint"), SafetyLevel::Safe);
        assert_eq!(level("npm run build"), SafetyLevel::Safe);
    }

    #[test]
    fn npm_install_is_caution() {
        assert_eq!(level("npm install"), SafetyLevel::Caution);
        assert_eq!(level("npm install lodash"), SafetyLevel::Caution);
        assert_eq!(level("npm publish"), SafetyLevel::Caution);
        assert_eq!(level("npm uninstall pkg"), SafetyLevel::Caution);
    }

    #[test]
    fn yarn_test_is_safe() {
        assert_eq!(level("yarn test"), SafetyLevel::Safe);
        assert_eq!(level("yarn build"), SafetyLevel::Safe);
        assert_eq!(level("yarn lint"), SafetyLevel::Safe);
    }

    #[test]
    fn yarn_install_is_caution() {
        assert_eq!(level("yarn install"), SafetyLevel::Caution);
        assert_eq!(level("yarn add lodash"), SafetyLevel::Caution);
        assert_eq!(level("yarn publish"), SafetyLevel::Caution);
    }

    #[test]
    fn pnpm_test_is_safe() {
        assert_eq!(level("pnpm test"), SafetyLevel::Safe);
        assert_eq!(level("pnpm run build"), SafetyLevel::Safe);
    }

    #[test]
    fn pnpm_install_is_caution() {
        assert_eq!(level("pnpm install"), SafetyLevel::Caution);
        assert_eq!(level("pnpm add pkg"), SafetyLevel::Caution);
    }

    #[test]
    fn go_test_is_safe() {
        assert_eq!(level("go test ./..."), SafetyLevel::Safe);
        assert_eq!(level("go build ./..."), SafetyLevel::Safe);
        assert_eq!(level("go vet ./..."), SafetyLevel::Safe);
        assert_eq!(level("go fmt ./..."), SafetyLevel::Safe);
        assert_eq!(level("go env"), SafetyLevel::Safe);
        assert_eq!(level("go doc fmt"), SafetyLevel::Safe);
    }

    #[test]
    fn go_install_is_caution() {
        assert_eq!(level("go install ./..."), SafetyLevel::Caution);
        assert_eq!(level("go get github.com/foo/bar"), SafetyLevel::Caution);
    }

    #[test]
    fn go_mod_read_is_safe() {
        assert_eq!(level("go mod download"), SafetyLevel::Safe);
        assert_eq!(level("go mod verify"), SafetyLevel::Safe);
        assert_eq!(level("go mod graph"), SafetyLevel::Safe);
        assert_eq!(level("go mod why pkg"), SafetyLevel::Safe);
    }

    #[test]
    fn go_mod_write_is_caution() {
        assert_eq!(level("go mod tidy"), SafetyLevel::Caution);
        assert_eq!(level("go mod edit"), SafetyLevel::Caution);
    }

    #[test]
    fn pip_list_is_safe() {
        assert_eq!(level("pip list"), SafetyLevel::Safe);
        assert_eq!(level("pip show requests"), SafetyLevel::Safe);
        assert_eq!(level("pip freeze"), SafetyLevel::Safe);
        assert_eq!(level("pip3 check"), SafetyLevel::Safe);
    }

    #[test]
    fn pip_install_is_caution() {
        assert_eq!(level("pip install requests"), SafetyLevel::Caution);
        assert_eq!(level("pip3 uninstall requests"), SafetyLevel::Caution);
        assert_eq!(level("pip install -r requirements.txt"), SafetyLevel::Caution);
    }

    #[test]
    fn mvn_test_is_safe() {
        assert_eq!(level("mvn test"), SafetyLevel::Safe);
        assert_eq!(level("mvn compile"), SafetyLevel::Safe);
        assert_eq!(level("mvn verify"), SafetyLevel::Safe);
        assert_eq!(level("./mvnw test"), SafetyLevel::Safe);
    }

    #[test]
    fn mvn_install_is_caution() {
        assert_eq!(level("mvn install"), SafetyLevel::Caution);
        assert_eq!(level("mvn deploy"), SafetyLevel::Caution);
    }

    #[test]
    fn gradle_test_is_safe() {
        assert_eq!(level("gradle test"), SafetyLevel::Safe);
        assert_eq!(level("./gradlew build"), SafetyLevel::Safe);
        assert_eq!(level("./gradlew check"), SafetyLevel::Safe);
    }

    #[test]
    fn dotnet_test_is_safe() {
        assert_eq!(level("dotnet test"), SafetyLevel::Safe);
        assert_eq!(level("dotnet build"), SafetyLevel::Safe);
        assert_eq!(level("dotnet lint"), SafetyLevel::Safe);
    }

    #[test]
    fn dotnet_publish_is_caution() {
        assert_eq!(level("dotnet publish"), SafetyLevel::Caution);
        assert_eq!(level("dotnet add package Newtonsoft.Json"), SafetyLevel::Caution);
    }

    #[test]
    fn bun_test_is_safe() {
        assert_eq!(level("bun test"), SafetyLevel::Safe);
        assert_eq!(level("bun build src/index.ts"), SafetyLevel::Safe);
    }

    #[test]
    fn bun_install_is_caution() {
        assert_eq!(level("bun install"), SafetyLevel::Caution);
        assert_eq!(level("bun add lodash"), SafetyLevel::Caution);
    }

    #[test]
    fn npm_run_deploy_is_caution() {
        // npm run scripts that deploy/release are caution even via run.
        assert_eq!(level("npm run deploy"), SafetyLevel::Caution);
        assert_eq!(level("npm run release"), SafetyLevel::Caution);
        assert_eq!(level("npm run publish"), SafetyLevel::Caution);
    }

    // ── Edge cases ────────────────────────────────────────────────────────────

    #[test]
    fn path_prefix_stripped_for_classification() {
        // /bin/rm treated same as rm
        assert_eq!(level("/bin/rm -rf /tmp"), SafetyLevel::Destructive);
        // /usr/bin/ls treated same as ls
        assert_eq!(level("/usr/bin/ls -la"), SafetyLevel::Safe);
    }

    #[test]
    fn env_assignment_skipped() {
        assert_eq!(level("PAGER=cat git log"), SafetyLevel::Safe);
        assert_eq!(level("FOO=bar rm -rf /"), SafetyLevel::Destructive);
    }

    #[test]
    fn empty_command_is_safe() {
        assert_eq!(level(""), SafetyLevel::Safe);
        assert_eq!(level("   "), SafetyLevel::Safe);
    }

    #[test]
    fn unknown_command_defaults_to_caution() {
        assert_eq!(level("myweirdtool --flag"), SafetyLevel::Caution);
    }
}
