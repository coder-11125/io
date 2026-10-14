//! OS-level sandbox for the `bash` tool: bubblewrap on Linux, Seatbelt
//! (`sandbox-exec`) on macOS.
//!
//! Inside the sandbox the whole filesystem is read-only except the project
//! root, the temp directory and any `sandbox.writablePaths`; credential
//! directories (`~/.ssh`, `~/.aws`, `~/.gnupg`, `~/.io`, …) are hidden; and
//! the network is cut off unless `sandbox.network` is set. This backs up the
//! permission checker — it limits what an approved command can do, not just
//! whether it runs.
//!
//! When the sandbox is enabled but no backend can be found, bash refuses to
//! run (fail closed) rather than silently running unsandboxed.

use std::path::{Path, PathBuf};

/// Directories under `$HOME` that hold credentials.
const SECRET_DIRS: &[&str] = &[".ssh", ".aws", ".gnupg", ".config/gcloud", ".azure", ".io"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxPolicy {
    /// The project root: always writable.
    pub root: PathBuf,
    pub network: bool,
    /// Extra writable paths.
    pub writable: Vec<PathBuf>,
    /// Existing directories hidden from the command.
    pub hidden: Vec<PathBuf>,
}

impl SandboxPolicy {
    pub fn new(root: PathBuf, network: bool, writable: Vec<PathBuf>) -> Self {
        let hidden = dirs::home_dir()
            .map(|home| {
                SECRET_DIRS
                    .iter()
                    .map(|d| home.join(d))
                    .filter(|p| p.is_dir())
                    .collect()
            })
            .unwrap_or_default();
        Self {
            root,
            network,
            writable,
            hidden,
        }
    }

    fn writable_paths(&self) -> Vec<&Path> {
        std::iter::once(self.root.as_path())
            .chain(self.writable.iter().map(PathBuf::as_path))
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Backend {
    Bubblewrap(PathBuf),
    SandboxExec(PathBuf),
}

fn which(program: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join(program))
            .find(|p| p.is_file())
    })
}

/// The sandbox mechanism for this OS, or why there is none.
pub fn find_backend() -> Result<Backend, String> {
    if cfg!(target_os = "macos") {
        let exec = which("sandbox-exec")
            .or_else(|| Some(PathBuf::from("/usr/bin/sandbox-exec")).filter(|p| p.is_file()));
        return exec
            .map(Backend::SandboxExec)
            .ok_or_else(|| "sandbox-exec was not found".to_string());
    }
    if cfg!(target_os = "linux") {
        return which("bwrap").map(Backend::Bubblewrap).ok_or_else(|| {
            "bubblewrap (bwrap) is not installed — install it, or turn the sandbox off with \
             `sandbox.enabled = false` in settings.json"
                .to_string()
        });
    }
    Err("the bash sandbox is only available on Linux and macOS".to_string())
}

/// The program and arguments that run `shell -c script` inside the sandbox.
pub fn command_line(
    backend: &Backend,
    policy: &SandboxPolicy,
    shell: &str,
    script: &str,
    workdir: &str,
) -> (PathBuf, Vec<String>) {
    match backend {
        Backend::Bubblewrap(program) => {
            let mut args: Vec<String> = vec![
                "--ro-bind".into(),
                "/".into(),
                "/".into(),
                "--dev".into(),
                "/dev".into(),
                "--proc".into(),
                "/proc".into(),
                "--bind".into(),
                "/tmp".into(),
                "/tmp".into(),
            ];
            for dir in &policy.hidden {
                args.extend(["--tmpfs".into(), dir.display().to_string()]);
            }
            for path in policy.writable_paths() {
                let p = path.display().to_string();
                args.extend(["--bind".into(), p.clone(), p]);
            }
            args.extend([
                "--unshare-pid".into(),
                "--unshare-ipc".into(),
                "--unshare-uts".into(),
                "--die-with-parent".into(),
            ]);
            if !policy.network {
                args.push("--unshare-net".into());
            }
            args.extend([
                "--chdir".into(),
                workdir.into(),
                "--".into(),
                shell.into(),
                "-c".into(),
                script.into(),
            ]);
            (program.clone(), args)
        }
        Backend::SandboxExec(program) => (
            program.clone(),
            vec![
                "-p".into(),
                seatbelt_profile(policy),
                shell.into(),
                "-c".into(),
                script.into(),
            ],
        ),
    }
}

fn sbpl_quote(path: &Path) -> String {
    let s = path.display().to_string();
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// A Seatbelt profile: later rules win, so deny first and allow what's needed.
fn seatbelt_profile(policy: &SandboxPolicy) -> String {
    let mut p = String::from("(version 1)\n(allow default)\n(deny file-write*)\n");
    let mut allow_write: Vec<String> = policy
        .writable_paths()
        .iter()
        .map(|d| {
            let canonical = std::fs::canonicalize(d).unwrap_or_else(|_| d.to_path_buf());
            format!("(subpath {})", sbpl_quote(&canonical))
        })
        .collect();
    for tmp in ["/private/tmp", "/private/var/folders", "/tmp"] {
        allow_write.push(format!("(subpath {})", sbpl_quote(Path::new(tmp))));
    }
    allow_write.push("(literal \"/dev/null\")".to_string());
    allow_write.push("(regex #\"^/dev/(tty|ttys[0-9]+|fd/[0-9]+)$\")".to_string());
    p.push_str(&format!("(allow file-write* {})\n", allow_write.join(" ")));
    for dir in &policy.hidden {
        p.push_str(&format!(
            "(deny file-read* file-write* (subpath {}))\n",
            sbpl_quote(dir)
        ));
    }
    if !policy.network {
        p.push_str("(deny network*)\n");
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(network: bool) -> SandboxPolicy {
        SandboxPolicy {
            root: PathBuf::from("/work/proj"),
            network,
            writable: vec![PathBuf::from("/work/cache")],
            hidden: vec![PathBuf::from("/home/u/.ssh")],
        }
    }

    fn bwrap() -> Backend {
        Backend::Bubblewrap(PathBuf::from("/usr/bin/bwrap"))
    }

    fn has_pair(args: &[String], flag: &str, a: &str, b: &str) -> bool {
        args.windows(3)
            .any(|w| w[0] == flag && w[1] == a && w[2] == b)
    }

    #[test]
    fn bubblewrap_mounts_everything_read_only_except_the_workspace() {
        let (prog, args) =
            command_line(&bwrap(), &policy(false), "/bin/sh", "echo hi", "/work/proj");
        assert_eq!(prog, PathBuf::from("/usr/bin/bwrap"));
        assert!(has_pair(&args, "--ro-bind", "/", "/"));
        assert!(has_pair(&args, "--bind", "/work/proj", "/work/proj"));
        assert!(has_pair(&args, "--bind", "/work/cache", "/work/cache"));
        assert!(has_pair(&args, "--bind", "/tmp", "/tmp"));
        assert!(args.windows(2).any(|w| w == ["--tmpfs", "/home/u/.ssh"]));
        assert!(args.windows(2).any(|w| w == ["--chdir", "/work/proj"]));
        assert!(args.contains(&"--die-with-parent".to_string()));
        // The command is last, after `--`.
        let tail = &args[args.len() - 4..];
        assert_eq!(tail, ["--", "/bin/sh", "-c", "echo hi"]);
    }

    #[test]
    fn the_network_is_cut_off_unless_allowed() {
        let off = command_line(&bwrap(), &policy(false), "sh", "x", ".").1;
        assert!(off.contains(&"--unshare-net".to_string()));
        let on = command_line(&bwrap(), &policy(true), "sh", "x", ".").1;
        assert!(!on.contains(&"--unshare-net".to_string()));
    }

    #[test]
    fn the_script_is_passed_as_one_argument() {
        let script = "echo 'a b'; rm -rf \"$X\"";
        let (_, args) = command_line(&bwrap(), &policy(true), "sh", script, ".");
        assert_eq!(args.last().unwrap(), script);
    }

    #[test]
    fn seatbelt_profiles_deny_writes_then_allow_the_workspace() {
        let backend = Backend::SandboxExec(PathBuf::from("/usr/bin/sandbox-exec"));
        let (prog, args) = command_line(&backend, &policy(false), "/bin/sh", "ls", "/work/proj");
        assert_eq!(prog, PathBuf::from("/usr/bin/sandbox-exec"));
        assert_eq!(args[0], "-p");
        assert_eq!(&args[2..], ["/bin/sh", "-c", "ls"]);
        let profile = &args[1];
        let deny = profile.find("(deny file-write*)").unwrap();
        let allow = profile.find("(allow file-write*").unwrap();
        assert!(deny < allow, "later rules win, so the allow must come last");
        assert!(profile.contains("(subpath \"/work/proj\")"));
        assert!(profile.contains("(deny file-read* file-write* (subpath \"/home/u/.ssh\"))"));
        assert!(profile.contains("(deny network*)"));
        assert!(!seatbelt_profile(&policy(true)).contains("(deny network*)"));
    }

    #[test]
    fn seatbelt_paths_are_quoted() {
        assert_eq!(
            sbpl_quote(Path::new("/a \"b\"\\c")),
            "\"/a \\\"b\\\"\\\\c\""
        );
    }

    #[test]
    fn hidden_directories_are_only_those_that_exist() {
        let p = SandboxPolicy::new(PathBuf::from("/x"), false, vec![]);
        assert!(p.hidden.iter().all(|d| d.is_dir()));
    }

    /// Runs the real thing when bubblewrap works here (it needs user
    /// namespaces, which many containers forbid), and is skipped otherwise.
    #[test]
    fn bubblewrap_really_confines_writes_and_network() {
        let Ok(backend @ Backend::Bubblewrap(_)) = find_backend() else {
            eprintln!("skipped: bwrap not installed");
            return;
        };
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let root = std::env::temp_dir().join(format!("io_bwrap_{}_{nanos}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let outside = PathBuf::from(format!("/var/tmp/io_bwrap_outside_{nanos}"));
        let policy = SandboxPolicy::new(root.clone(), false, vec![]);
        let run = |script: &str| {
            let (prog, args) =
                command_line(&backend, &policy, "/bin/sh", script, root.to_str().unwrap());
            std::process::Command::new(prog)
                .args(args)
                .output()
                .unwrap()
        };
        let probe = run("echo ok");
        if !probe.status.success() {
            eprintln!("skipped: bwrap cannot create namespaces here");
            std::fs::remove_dir_all(&root).ok();
            return;
        }
        assert!(run(&format!("touch '{}/inside'", root.display()))
            .status
            .success());
        assert!(root.join("inside").exists());
        assert!(!run(&format!("touch '{}'", outside.display()))
            .status
            .success());
        assert!(!outside.exists());
        std::fs::remove_dir_all(&root).ok();
    }
}
