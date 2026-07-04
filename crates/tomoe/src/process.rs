//! Declarative process management (`tomoe.process`, ShojiWM-shape).
//!
//! Lua declares a manifest — `once`/`service` entries keyed by id — and this
//! module owns the actual process lifecycle, reconciling running children
//! against the manifest after every Lua entry that changed it. The manifest
//! design (rather than a raw `exec()`) is what makes hot reload safe: a fresh
//! config VM re-declares its desired state and the diff decides what to
//! keep, restart, or stop. Fire-and-forget spawns are not diffed.
//!
//! Supervision is polling, not SIGCHLD: a 1 Hz timer (`Tomoe::
//! ensure_process_timer`) calls [`ProcessManager::tick`], which reaps every
//! child and restarts services per policy. The tick period doubles as the
//! restart rate limit, so a crash-looping service respawns at most once per
//! second. The timer only exists while there is something to supervise —
//! an idle session takes no wakeups from this module.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};

use tracing::{info, warn};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Launch {
    /// `/bin/sh -c <command>` — for pipes, redirection, `$VAR` expansion.
    Shell(String),
    /// exec'd directly, no shell involved. Never empty (parse-side checked).
    Argv(Vec<String>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessSpec {
    pub launch: Launch,
    /// Relative paths resolve against the config file's directory.
    pub cwd: Option<PathBuf>,
    /// BTreeMap so equal configs compare equal regardless of Lua table
    /// iteration order — spec equality is what `keep_if_unchanged` keys on.
    pub env: BTreeMap<String, String>,
}

/// `once` entries: when to run again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RunPolicy {
    /// At most once per compositor session; reloads never re-run it.
    #[default]
    OncePerSession,
    /// Once per config generation: re-runs on every config (re)load.
    OncePerConfigVersion,
}

/// `service` entries: what to do when the process exits on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RestartPolicy {
    Never,
    OnFailure,
    #[default]
    OnExit,
}

/// `service` entries: what a config reload does to a running process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReloadPolicy {
    /// Keep the running process if its declaration is byte-identical.
    #[default]
    KeepIfUnchanged,
    /// Restart on every new config generation even if unchanged.
    AlwaysRestart,
}

/// One manifest entry, as declared from Lua.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessDecl {
    Once {
        spec: ProcessSpec,
        run: RunPolicy,
    },
    Service {
        spec: ProcessSpec,
        restart: RestartPolicy,
        reload: ReloadPolicy,
    },
}

struct Service {
    spec: ProcessSpec,
    restart: RestartPolicy,
    reload: ReloadPolicy,
    child: Child,
    started_generation: u64,
}

#[derive(Default)]
pub struct ProcessManager {
    services: HashMap<String, Service>,
    /// once id → config generation it last ran at.
    once_runs: HashMap<String, u64>,
    /// Services that exited and stay stopped (policy said no restart, or the
    /// respawn failed) until the recorded generation is superseded — so a
    /// config reload is the retry trigger, not every reconcile.
    suppressed: HashMap<String, u64>,
    /// Fire-and-forget children (`spawn`, `once`) awaiting reap.
    detached: Vec<Child>,
    /// Config generation, bumped once per successful config (re)load.
    generation: u64,
    /// Directory of the loaded config file, for relative `cwd` values.
    config_dir: Option<PathBuf>,
}

impl ProcessManager {
    /// A new config generation begins (successful config load or reload).
    pub fn begin_generation(&mut self, config_path: Option<&Path>) {
        self.generation += 1;
        self.config_dir = config_path.and_then(|p| p.parent().map(PathBuf::from));
    }

    /// True while any child needs polling (the supervision timer's lifetime).
    pub fn needs_supervision(&self) -> bool {
        !self.services.is_empty() || !self.detached.is_empty()
    }

    /// Fire-and-forget spawn; the child is reaped by `tick` when it exits.
    pub fn spawn_detached(&mut self, spec: &ProcessSpec) {
        match self.spawn(spec) {
            Ok(child) => self.detached.push(child),
            Err(err) => warn!("error spawning {:?}: {err}", spec.launch),
        }
    }

    /// Drive running state to match the manifest: stop services that
    /// disappeared or changed, start missing ones, run due `once` entries.
    pub fn reconcile(&mut self, desired: &HashMap<String, ProcessDecl>) {
        let generation = self.generation;

        // Stop what no longer matches. A changed declaration (spec *or*
        // policies) replaces the process; `always_restart` also replaces an
        // unchanged one once per generation.
        let running: Vec<String> = self.services.keys().cloned().collect();
        for id in running {
            let keep = match desired.get(&id) {
                Some(ProcessDecl::Service {
                    spec,
                    restart,
                    reload,
                }) => {
                    let svc = &self.services[&id];
                    svc.spec == *spec
                        && svc.restart == *restart
                        && svc.reload == *reload
                        && !(*reload == ReloadPolicy::AlwaysRestart
                            && svc.started_generation != generation)
                }
                _ => false,
            };
            if !keep {
                if let Some(mut svc) = self.services.remove(&id) {
                    info!("stopping service {id:?}");
                    kill(&mut svc.child);
                }
                self.suppressed.remove(&id);
            }
        }

        for (id, decl) in desired {
            match decl {
                ProcessDecl::Once { spec, run } => {
                    let due = match (run, self.once_runs.get(id)) {
                        (_, None) => true,
                        (RunPolicy::OncePerSession, Some(_)) => false,
                        (RunPolicy::OncePerConfigVersion, Some(gen)) => *gen != generation,
                    };
                    if !due {
                        continue;
                    }
                    match self.spawn(spec) {
                        Ok(child) => {
                            info!("ran once process {id:?}");
                            self.detached.push(child);
                            self.once_runs.insert(id.clone(), generation);
                        }
                        Err(err) => warn!("error running once process {id:?}: {err}"),
                    }
                }
                ProcessDecl::Service {
                    spec,
                    restart,
                    reload,
                } => {
                    if self.services.contains_key(id)
                        || self.suppressed.get(id) == Some(&generation)
                    {
                        continue;
                    }
                    match self.spawn(spec) {
                        Ok(child) => {
                            info!("started service {id:?} (pid {})", child.id());
                            self.suppressed.remove(id);
                            self.services.insert(
                                id.clone(),
                                Service {
                                    spec: spec.clone(),
                                    restart: *restart,
                                    reload: *reload,
                                    child,
                                    started_generation: generation,
                                },
                            );
                        }
                        Err(err) => {
                            warn!("error starting service {id:?}: {err}");
                            self.suppressed.insert(id.clone(), generation);
                        }
                    }
                }
            }
        }
    }

    /// Supervision-timer body: reap exited children, restart services per
    /// policy. Returns false when nothing is left to supervise, so the
    /// caller can drop the timer.
    pub fn tick(&mut self) -> bool {
        self.detached
            .retain_mut(|child| matches!(child.try_wait(), Ok(None)));

        let ids: Vec<String> = self.services.keys().cloned().collect();
        for id in ids {
            let status = match self.services.get_mut(&id).unwrap().child.try_wait() {
                Ok(status) => status,
                Err(err) => {
                    warn!("error polling service {id:?}: {err}");
                    None
                }
            };
            let Some(status) = status else { continue };
            let svc = self.services.remove(&id).unwrap();
            if !should_restart(svc.restart, status) {
                info!("service {id:?} exited ({status}); staying stopped");
                self.suppressed.insert(id, self.generation);
                continue;
            }
            info!("service {id:?} exited ({status}); restarting");
            match self.spawn(&svc.spec) {
                Ok(child) => {
                    self.services.insert(
                        id,
                        Service {
                            child,
                            started_generation: self.generation,
                            ..svc
                        },
                    );
                }
                Err(err) => {
                    warn!("error restarting service {id:?}: {err}");
                    self.suppressed.insert(id, self.generation);
                }
            }
        }
        self.needs_supervision()
    }

    /// Compositor shutdown: stop every managed service. Fire-and-forget
    /// children are not managed and are left alone (init reaps them).
    pub fn shutdown(&mut self) {
        for (id, mut svc) in self.services.drain() {
            info!("stopping service {id:?}");
            kill(&mut svc.child);
        }
    }

    fn spawn(&self, spec: &ProcessSpec) -> std::io::Result<Child> {
        let mut cmd = match &spec.launch {
            Launch::Shell(sh) => {
                let mut cmd = Command::new("/bin/sh");
                cmd.arg("-c").arg(sh);
                cmd
            }
            Launch::Argv(argv) => {
                let Some((program, args)) = argv.split_first() else {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "empty command",
                    ));
                };
                let mut cmd = Command::new(program);
                cmd.args(args);
                cmd
            }
        };
        if let Some(cwd) = &spec.cwd {
            match (cwd.is_relative(), &self.config_dir) {
                (true, Some(dir)) => cmd.current_dir(dir.join(cwd)),
                _ => cmd.current_dir(cwd),
            };
        }
        cmd.envs(&spec.env);
        // stdout/stderr stay inherited so service output lands in the
        // compositor's log (the session journal on a TTY session).
        cmd.stdin(Stdio::null());
        cmd.spawn()
    }
}

fn should_restart(policy: RestartPolicy, status: ExitStatus) -> bool {
    match policy {
        RestartPolicy::Never => false,
        RestartPolicy::OnExit => true,
        RestartPolicy::OnFailure => !status.success(),
    }
}

fn kill(child: &mut Child) {
    // Already-exited children just get reaped; anything else is killed. No
    // process-group kill: a `shell` service that forks should exec.
    match child.try_wait() {
        Ok(Some(_)) => {}
        _ => {
            if let Err(err) = child.kill() {
                warn!("error killing child {}: {err}", child.id());
            }
            let _ = child.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shell(cmd: &str) -> ProcessSpec {
        ProcessSpec {
            launch: Launch::Shell(cmd.to_string()),
            cwd: None,
            env: BTreeMap::new(),
        }
    }

    fn service(cmd: &str) -> ProcessDecl {
        ProcessDecl::Service {
            spec: shell(cmd),
            restart: RestartPolicy::OnExit,
            reload: ReloadPolicy::KeepIfUnchanged,
        }
    }

    #[test]
    fn restart_policies() {
        use std::os::unix::process::ExitStatusExt;
        let ok = ExitStatus::from_raw(0);
        let fail = ExitStatus::from_raw(1 << 8);
        assert!(!should_restart(RestartPolicy::Never, ok));
        assert!(!should_restart(RestartPolicy::Never, fail));
        assert!(should_restart(RestartPolicy::OnExit, ok));
        assert!(should_restart(RestartPolicy::OnExit, fail));
        assert!(!should_restart(RestartPolicy::OnFailure, ok));
        assert!(should_restart(RestartPolicy::OnFailure, fail));
    }

    #[test]
    fn reconcile_services() {
        let mut mgr = ProcessManager::default();
        mgr.begin_generation(None);

        let mut manifest = HashMap::new();
        manifest.insert("svc".to_string(), service("sleep 30"));
        mgr.reconcile(&manifest);
        assert!(mgr.services.contains_key("svc"));
        assert!(mgr.needs_supervision());
        let pid = mgr.services["svc"].child.id();

        // Unchanged spec: the same process keeps running.
        mgr.reconcile(&manifest);
        assert_eq!(mgr.services["svc"].child.id(), pid);

        // Changed spec: replaced.
        manifest.insert("svc".to_string(), service("sleep 60"));
        mgr.reconcile(&manifest);
        assert_ne!(mgr.services["svc"].child.id(), pid);

        // Removed from the manifest: stopped.
        manifest.clear();
        mgr.reconcile(&manifest);
        assert!(mgr.services.is_empty());
    }

    #[test]
    fn reconcile_once_policies() {
        let mut mgr = ProcessManager::default();
        mgr.begin_generation(None);

        let mut manifest = HashMap::new();
        manifest.insert(
            "session".to_string(),
            ProcessDecl::Once {
                spec: shell("true"),
                run: RunPolicy::OncePerSession,
            },
        );
        manifest.insert(
            "config".to_string(),
            ProcessDecl::Once {
                spec: shell("true"),
                run: RunPolicy::OncePerConfigVersion,
            },
        );

        mgr.reconcile(&manifest);
        assert_eq!(mgr.detached.len(), 2);

        // Same generation: neither runs again.
        mgr.reconcile(&manifest);
        assert_eq!(mgr.detached.len(), 2);

        // New generation (config reload): only once_per_config_version re-runs.
        mgr.begin_generation(None);
        mgr.reconcile(&manifest);
        assert_eq!(mgr.detached.len(), 3);

        for mut child in mgr.detached.drain(..) {
            let _ = child.wait();
        }
    }
}
