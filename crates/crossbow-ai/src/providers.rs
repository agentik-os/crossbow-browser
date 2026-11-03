//! Passive discovery. Finding a client is not authorization to run it, proof of
//! authentication, a model catalogue, or a guarantee of on-device inference.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    Ollama,
    LmStudio,
    LlamaCpp,
    ClaudeCode,
    Codex,
    GeminiCli,
    OpenCode,
}

impl ProviderKind {
    pub const ALL: [Self; 7] = [Self::Ollama, Self::LmStudio, Self::LlamaCpp, Self::ClaudeCode, Self::Codex, Self::GeminiCli, Self::OpenCode];

    pub fn name(self) -> &'static str {
        match self {
            Self::Ollama => "Ollama",
            Self::LmStudio => "LM Studio",
            Self::LlamaCpp => "llama.cpp",
            Self::ClaudeCode => "Claude Code",
            Self::Codex => "Codex",
            Self::GeminiCli => "Gemini CLI",
            Self::OpenCode => "OpenCode",
        }
    }

    pub fn execution_description(self) -> &'static str {
        match self {
            Self::Ollama | Self::LmStudio | Self::LlamaCpp => "Local server; model execution location must be checked separately.",
            Self::ClaudeCode | Self::Codex | Self::GeminiCli => "Installed client backed by a cloud provider; not offline inference.",
            Self::OpenCode => "Installed client; local or cloud execution depends on its configuration.",
        }
    }

    fn executable(self) -> &'static str {
        match self {
            Self::Ollama => "ollama",
            Self::LmStudio => "lms",
            Self::LlamaCpp => "llama-server",
            Self::ClaudeCode => "claude",
            Self::Codex => "codex",
            Self::GeminiCli => "gemini",
            Self::OpenCode => "opencode",
        }
    }

    fn bundle(self) -> Option<&'static str> {
        match self {
            Self::Ollama => Some("Ollama.app"),
            Self::LmStudio => Some("LM Studio.app"),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DetectedProvider {
    pub kind: ProviderKind,
    /// Canonical paths are display-only. Never execute a discovered path without
    /// separately approving and revalidating the adapter and binary identity.
    pub executable: Option<PathBuf>,
    pub application: Option<PathBuf>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Discovery {
    pub providers: Vec<DetectedProvider>,
    pub path_limit_reached: bool,
}

#[derive(Clone, Debug, Default)]
pub struct DiscoveryRoots {
    pub binaries: Vec<PathBuf>,
    pub applications: Vec<PathBuf>,
}

impl DiscoveryRoots {
    pub fn for_current_process(home: Option<&Path>) -> Self {
        let mut roots = Self::default();
        if let Some(path) = std::env::var_os("PATH") {
            roots.binaries.extend(std::env::split_paths(&path));
        }
        if let Some(home) = home.filter(|path| path.is_absolute()) {
            for relative in [".local/bin", ".cargo/bin", ".opencode/bin", ".lmstudio/bin"] {
                roots.binaries.push(home.join(relative));
            }
            #[cfg(target_os = "macos")]
            roots.applications.push(home.join("Applications"));
        }
        #[cfg(unix)]
        roots.binaries.extend(["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"].map(PathBuf::from));
        #[cfg(target_os = "macos")]
        roots.applications.push(PathBuf::from("/Applications"));
        roots
    }
}

/// Checks only fixed filenames, never walks a home directory, opens settings or
/// credentials, invokes a shell/client, starts a server, or contacts an endpoint.
/// Synchronous filesystem calls can block; the application uses the cancellable
/// form behind a process-wide worker limit and a separate UI deadline.
pub fn discover(roots: &DiscoveryRoots) -> Discovery {
    discover_cancellable(roots, || false).unwrap_or_default()
}

/// Stops between filesystem calls and discards partial results. Cancellation
/// cannot interrupt a syscall already blocked inside the operating system.
pub fn discover_cancellable(roots: &DiscoveryRoots, mut stopped: impl FnMut() -> bool) -> Option<Discovery> {
    let mut binaries = Vec::new();
    let mut path_limit_reached = roots.applications.len() > 8;
    for directory in &roots.binaries {
        if stopped() { return None; }
        // Relative and empty PATH entries would inspect the working project.
        if directory.is_absolute() && !binaries.contains(directory) {
            if binaries.len() == 64 { path_limit_reached = true; break; }
            binaries.push(directory.clone());
        }
    }
    let mut providers = Vec::new();
    for kind in ProviderKind::ALL {
        if stopped() { return None; }
        let mut executable = None;
        for directory in &binaries {
            executable = executable_at(directory, kind.executable(), &mut stopped).ok()?;
            if executable.is_some() { break; }
        }
        let mut application = None;
        if let Some(name) = kind.bundle() {
            for directory in roots.applications.iter().take(8).filter(|p| p.is_absolute()) {
                let Ok(path) = lookup(&mut stopped, || directory.join(name).canonicalize()).ok()? else { continue };
                if lookup(&mut stopped, || path.is_dir()).ok()? { application = Some(path); break; }
            }
        }
        if executable.is_some() || application.is_some() { providers.push(DetectedProvider { kind, executable, application }); }
    }
    if stopped() { None } else { Some(Discovery { providers, path_limit_reached }) }
}

fn lookup<T>(stopped: &mut impl FnMut() -> bool, read: impl FnOnce() -> T) -> Result<T, ()> {
    if stopped() { return Err(()); }
    let value = read();
    if stopped() { Err(()) } else { Ok(value) }
}

fn executable_at(directory: &Path, name: &str, stopped: &mut impl FnMut() -> bool) -> Result<Option<PathBuf>, ()> {
    #[cfg(windows)]
    let name = format!("{name}.exe");
    let Ok(path) = lookup(stopped, || directory.join(name).canonicalize())? else { return Ok(None) };
    let Ok(metadata) = lookup(stopped, || path.metadata())? else { return Ok(None) };
    if !metadata.is_file() { return Ok(None); }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 { return Ok(None); }
    }
    Ok(Some(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_binary(dir: &Path, name: &str) -> PathBuf {
        #[cfg(windows)]
        let name = format!("{name}.exe");
        let path = dir.join(name);
        std::fs::write(&path, b"this is not a runnable program\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        path
    }

    #[test]
    fn finds_fixed_names_without_execution_or_configuration_reads() {
        let root = tempfile::tempdir().unwrap();
        let binary = fixture_binary(root.path(), "claude");
        std::fs::create_dir(root.path().join("Ollama.app")).unwrap();
        // Invalid bytes are intentional: discovery must not parse this file.
        std::fs::write(root.path().join("auth.json"), [0xff, 0xfe]).unwrap();
        let result = discover(&DiscoveryRoots { binaries: vec![root.path().into()], applications: vec![root.path().into()] });
        assert_eq!(result.providers.len(), 2);
        assert_eq!(result.providers[0].kind, ProviderKind::Ollama);
        assert!(result.providers[0].executable.is_none());
        assert_eq!(result.providers[1].executable.as_ref(), Some(&binary.canonicalize().unwrap()));
        assert_eq!(result.providers[1].kind, ProviderKind::ClaudeCode);
    }

    #[test]
    fn ignores_relative_paths_directories_and_nonexecutables() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("codex")).unwrap();
        let path = fixture_binary(root.path(), "gemini");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            assert!(discover(&DiscoveryRoots { binaries: vec![root.path().into()], applications: vec![] }).providers.is_empty());
        }
        assert!(discover(&DiscoveryRoots { binaries: vec![PathBuf::from("."), PathBuf::new()], applications: vec![PathBuf::from(".")] }).providers.is_empty());
    }

    #[test]
    fn duplicate_roots_and_repeated_discovery_do_not_duplicate_providers() {
        let root = tempfile::tempdir().unwrap();
        fixture_binary(root.path(), "ollama");
        let roots = DiscoveryRoots { binaries: vec![root.path().into(); 100], applications: vec![] };
        let first = discover(&roots);
        assert_eq!(first, discover(&roots));
        assert_eq!(first.providers.len(), 1);
        assert!(!first.path_limit_reached);
    }

    #[cfg(unix)]
    #[test]
    fn canonicalizes_symlinks_but_does_not_follow_broken_links() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let target = fixture_binary(root.path(), "actual-client");
        symlink(&target, root.path().join("codex")).unwrap();
        symlink(root.path().join("absent"), root.path().join("claude")).unwrap();
        let result = discover(&DiscoveryRoots { binaries: vec![root.path().into()], applications: vec![] });
        assert_eq!(result.providers.len(), 1);
        assert_eq!(result.providers[0].executable, Some(target.canonicalize().unwrap()));
    }

    #[test]
    fn cancelled_lookup_never_starts_and_cancellation_discards_its_result() {
        use std::cell::Cell;
        let called = Cell::new(false);
        assert_eq!(lookup(&mut || true, || called.set(true)), Err(()));
        assert!(!called.get());
        let stopped = Cell::new(false);
        assert_eq!(lookup(&mut || stopped.get(), || { stopped.set(true); 42 }), Err(()));
        assert_eq!(lookup(&mut || stopped.get(), || called.set(true)), Err(()));
        assert!(!called.get());
    }

    #[test]
    fn cancellation_during_discovery_never_returns_a_partial_catalogue() {
        let root = tempfile::tempdir().unwrap();
        fixture_binary(root.path(), "ollama");
        fixture_binary(root.path(), "codex");
        std::fs::create_dir(root.path().join("LM Studio.app")).unwrap();
        let roots = DiscoveryRoots { binaries: vec![root.path().into()], applications: vec![root.path().into()] };
        let complete = discover(&roots);
        assert_eq!(complete.providers.len(), 3);
        for cancel_at in 1..50 {
            let mut checks = 0;
            let result = discover_cancellable(&roots, || { checks += 1; checks == cancel_at });
            if checks == cancel_at { assert!(result.is_none(), "cancel at {cancel_at}"); }
            else { assert_eq!(result, Some(complete.clone())); }
        }
    }

    #[test]
    fn discovery_reports_its_bound_instead_of_claiming_an_exhaustive_scan() {
        let root = tempfile::tempdir().unwrap();
        let paths = (0..65).map(|n| root.path().join(format!("bin-{n}"))).collect();
        let result = discover(&DiscoveryRoots { binaries: paths, applications: vec![] });
        assert!(result.path_limit_reached);
        assert!(result.providers.is_empty());
    }
}
