use std::collections::BTreeMap;
use zellij_tile::prelude::*;

const POLL_INTERVAL_SECS: f64 = 5.0;
const DEFAULT_IDLE_TIMEOUT_SECS: f64 = 300.0;
const DEFAULT_COUNTDOWN_SECS: f64 = 60.0;
const DEFAULT_SUSPEND_ACTION: &str = "suspend";

// Inline bash script for idle detection.
// Finds direct children of zellij, checks /proc/<pid>/stat to determine
// if the shell is the foreground process (idle) or something else is running (active).
// Skips processes without a controlling terminal (tty_nr == 0).
//
// Arguments:
//   $1 = zellij PID
//   $2 = claude_code_idle_detection ("true" or "false")
//   $3 = ignore_processes (comma-separated list, e.g. "vim,nvim,less")
//
// Claude Code detection: When a foreground process is "claude" or "node" running
// Claude Code, we check if that process has children. If it does, Claude Code is
// actively working (running tools, generating code). If not, it's idle at its prompt.
//
// ignore_processes: Any foreground process whose name matches this list is treated
// as idle, allowing suspend even when those processes are running.
const IDLE_CHECK_SCRIPT: &str = r#"
ZELLIJ_PID="$1"
CLAUDE_DETECT="$2"
IGNORE_PROCS="$3"

# Build an associative array of ignored process names for fast lookup
declare -A IGNORED
if [ -n "$IGNORE_PROCS" ]; then
  IFS=',' read -ra IGNORE_ARR <<< "$IGNORE_PROCS"
  for p in "${IGNORE_ARR[@]}"; do
    p="$(echo "$p" | tr -d ' ')"
    [ -n "$p" ] && IGNORED["$p"]=1
  done
fi

# Check if a PID looks like it's running Claude Code.
is_claude_code() {
  local pid="$1"
  local comm="$2"
  if [ "$comm" = "claude" ]; then
    return 0
  fi
  if [ "$comm" = "node" ]; then
    local cmdline
    cmdline=$(tr '\0' ' ' < /proc/$pid/cmdline 2>/dev/null) || return 1
    case "$cmdline" in
      */@anthropic/claude-code/* | */claude-code/* | *" claude "*) return 0 ;;
    esac
  fi
  return 1
}

# Check if a process has any child processes
has_children() {
  local pid="$1"
  local children
  if [ -f "/proc/$pid/task/$pid/children" ]; then
    children=$(cat /proc/$pid/task/$pid/children 2>/dev/null)
  else
    children=$(pgrep -P "$pid" 2>/dev/null)
  fi
  [ -n "$(echo "$children" | tr -d '[:space:]')" ]
}

for child in $(pgrep -P "$ZELLIJ_PID"); do
  stat=$(cat /proc/$child/stat 2>/dev/null) || continue
  comm="${stat#*(}"
  comm="${comm%)*}"
  rest="${stat##*) }"
  tty_nr=$(echo "$rest" | awk '{print $5}')
  [ "$tty_nr" = "0" ] && continue
  pgrp=$(echo "$rest" | awk '{print $3}')
  tpgid=$(echo "$rest" | awk '{print $6}')
  if [ "$pgrp" = "$tpgid" ]; then
    echo "idle:$child:$comm"
  else
    fg_pid="$tpgid"
    fg_comm=$(cat /proc/$fg_pid/comm 2>/dev/null || echo "unknown")

    # Check ignore_processes list
    if [ -n "${IGNORED[$fg_comm]+x}" ]; then
      echo "idle:$child:$fg_comm(ignored)"
      continue
    fi

    # Check Claude Code idle detection
    if [ "$CLAUDE_DETECT" = "true" ] && is_claude_code "$fg_pid" "$fg_comm"; then
      if has_children "$fg_pid"; then
        echo "active:$child:$fg_comm(claude-working)"
      else
        echo "idle:$child:$fg_comm(claude-idle)"
      fi
      continue
    fi

    echo "active:$child:$fg_comm"
  fi
done
"#;

// Bash script to self-suspend or stop a GCE VM.
// Fetches instance metadata from the GCE metadata server, then tries suspend first
// and falls back to stop (for E2/GPU instances where suspend is unsupported).
// $1 = action: "suspend" or "stop".
const SUSPEND_SCRIPT: &str = r#"
VM_NAME=$(curl -sf "http://metadata.google.internal/computeMetadata/v1/instance/name" -H "Metadata-Flavor: Google") || { echo "ERROR: failed to fetch VM name from metadata server"; exit 1; }
VM_ZONE=$(curl -sf "http://metadata.google.internal/computeMetadata/v1/instance/zone" -H "Metadata-Flavor: Google" | cut -d '/' -f 4) || { echo "ERROR: failed to fetch VM zone from metadata server"; exit 1; }
VM_PROJECT=$(curl -sf "http://metadata.google.internal/computeMetadata/v1/project/project-id" -H "Metadata-Flavor: Google") || { echo "ERROR: failed to fetch project ID from metadata server"; exit 1; }

ACTION="${1:-suspend}"

if [ "$ACTION" = "stop" ]; then
  echo "Stopping $VM_NAME in $VM_ZONE ($VM_PROJECT)..."
  gcloud compute instances stop "$VM_NAME" --zone="$VM_ZONE" --project="$VM_PROJECT" --quiet
elif [ "$ACTION" = "suspend" ]; then
  echo "Suspending $VM_NAME in $VM_ZONE ($VM_PROJECT)..."
  if ! gcloud compute instances suspend "$VM_NAME" --zone="$VM_ZONE" --project="$VM_PROJECT" --quiet 2>/tmp/zellij-idle-suspend-err; then
    echo "Suspend failed, falling back to stop..."
    gcloud compute instances stop "$VM_NAME" --zone="$VM_ZONE" --project="$VM_PROJECT" --quiet
  fi
fi
"#;

struct State {
    loaded: bool,
    zellij_pid: u32,

    // Idle detection
    is_idle: bool,
    idle_elapsed_secs: f64,
    active_pane_count: usize,
    active_processes: Vec<String>,

    // Polling counters — elapsed idle time = (poll_count - last_activity_poll_count) * POLL_INTERVAL_SECS
    poll_count: u64,
    last_activity_poll_count: u64,

    // Countdown state
    countdown_active: bool,
    countdown_remaining: f64,
    suspend_triggered: bool,

    // Suspend command state
    suspend_command_sent: bool,

    // Config (from layout.kdl)
    idle_timeout_secs: f64,
    countdown_secs: f64,
    suspend_action: String,
    claude_code_idle_detection: bool,
    ignore_processes: Vec<String>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            loaded: false,
            zellij_pid: 0,
            is_idle: false,
            idle_elapsed_secs: 0.0,
            active_pane_count: 0,
            active_processes: Vec::new(),
            poll_count: 0,
            last_activity_poll_count: 0,
            countdown_active: false,
            countdown_remaining: 0.0,
            suspend_triggered: false,
            suspend_command_sent: false,
            idle_timeout_secs: 0.0,
            countdown_secs: 0.0,
            suspend_action: String::new(),
            claude_code_idle_detection: true,
            ignore_processes: Vec::new(),
        }
    }
}

register_plugin!(State);

impl ZellijPlugin for State {
    fn load(&mut self, configuration: BTreeMap<String, String>) {
        self.idle_timeout_secs = configuration
            .get("idle_timeout_secs")
            .and_then(|s| s.parse().ok())
            .unwrap_or(DEFAULT_IDLE_TIMEOUT_SECS);
        self.countdown_secs = configuration
            .get("countdown_secs")
            .and_then(|s| s.parse().ok())
            .unwrap_or(DEFAULT_COUNTDOWN_SECS);
        self.suspend_action = configuration
            .get("suspend_action")
            .cloned()
            .unwrap_or_else(|| DEFAULT_SUSPEND_ACTION.to_string());
        self.claude_code_idle_detection = configuration
            .get("claude_code_idle_detection")
            .map(|s| s.trim().eq_ignore_ascii_case("true"))
            .unwrap_or(true);
        self.ignore_processes = configuration
            .get("ignore_processes")
            .map(|s| {
                s.split(',')
                    .map(|p| p.trim().to_string())
                    .filter(|p| !p.is_empty())
                    .collect()
            })
            .unwrap_or_default();

        let ids = get_plugin_ids();
        self.zellij_pid = ids.zellij_pid;

        request_permission(&[
            PermissionType::ReadApplicationState,
            PermissionType::RunCommands,
            PermissionType::ChangeApplicationState,
        ]);

        subscribe(&[
            EventType::Timer,
            EventType::PermissionRequestResult,
            EventType::RunCommandResult,
            EventType::InputReceived,
        ]);

        eprintln!(
            "zellij-idle: loaded config: idle_timeout={}s, countdown={}s, suspend_action={}, claude_detect={}, ignore={:?}, zellij_pid={}",
            self.idle_timeout_secs, self.countdown_secs, self.suspend_action,
            self.claude_code_idle_detection, self.ignore_processes, self.zellij_pid
        );

        set_timeout(1.0);
    }

    fn update(&mut self, event: Event) -> bool {
        match event {
            Event::Timer(_) => {
                if self.loaded {
                    self.poll_count += 1;

                    // Update idle elapsed time
                    if self.is_idle {
                        self.idle_elapsed_secs = (self.poll_count - self.last_activity_poll_count)
                            as f64
                            * POLL_INTERVAL_SECS;
                    }

                    // Countdown logic
                    if self.countdown_active {
                        self.countdown_remaining -= POLL_INTERVAL_SECS;
                        if self.countdown_remaining <= 0.0 {
                            self.suspend_triggered = true;
                            self.countdown_active = false;
                            self.trigger_suspend();
                        }
                    } else if self.is_idle && self.idle_elapsed_secs >= self.idle_timeout_secs {
                        self.countdown_active = true;
                        self.countdown_remaining = self.countdown_secs;
                        eprintln!(
                            "zellij-idle: -> COUNTDOWN (idle for {}s >= threshold {}s, countdown={}s)",
                            self.idle_elapsed_secs as u64, self.idle_timeout_secs as u64, self.countdown_secs as u64
                        );
                    }

                    self.run_idle_check();
                } else {
                    self.loaded = true;
                }
                set_timeout(POLL_INTERVAL_SECS);
                true
            }
            Event::PermissionRequestResult(_) => true,
            Event::RunCommandResult(exit_code, stdout, stderr, context) => {
                match context.get("command").map(|s| s.as_str()) {
                    Some("suspend") => {
                        let out = String::from_utf8_lossy(&stdout);
                        let err = String::from_utf8_lossy(&stderr);
                        if exit_code != Some(0) {
                            eprintln!(
                                "zellij-idle: suspend command failed (exit {:?}): stdout={}, stderr={}",
                                exit_code,
                                out.trim(),
                                err.trim()
                            );
                        } else {
                            eprintln!("zellij-idle: suspend command succeeded: {}", out.trim());
                        }
                    }
                    _ => {
                        self.parse_idle_check_output(&stdout);
                    }
                }
                true
            }
            Event::InputReceived => {
                if self.countdown_active {
                    eprintln!("zellij-idle: input received, cancelling countdown");
                } else if self.is_idle {
                    eprintln!("zellij-idle: input received, resetting idle timer");
                }
                self.last_activity_poll_count = self.poll_count;
                self.idle_elapsed_secs = 0.0;
                self.is_idle = false;
                self.countdown_active = false;
                self.countdown_remaining = 0.0;
                self.suspend_triggered = false;
                self.suspend_command_sent = false;
                true
            }
            _ => false,
        }
    }

    fn render(&mut self, _rows: usize, cols: usize) {
        if !self.loaded {
            print!("loading");
            return;
        }

        if self.suspend_triggered {
            let msg = "SUSPEND!";
            let truncated = &msg[..msg.len().min(cols)];
            let padding = cols.saturating_sub(truncated.len());
            print!(
                "\x1b[41;97;1m{}{}\x1b[0m",
                truncated,
                " ".repeat(padding)
            );
        } else if self.countdown_active {
            let remaining = self.countdown_remaining.max(0.0) as u64;
            let msg = format!("SUSPEND {}s", remaining);
            let truncated = &msg[..msg.len().min(cols)];
            let padding = cols.saturating_sub(truncated.len());
            print!(
                "\x1b[43;30;1m{}{}\x1b[0m",
                truncated,
                " ".repeat(padding)
            );
        } else if self.is_idle {
            let elapsed = self.idle_elapsed_secs as u64;
            let msg = format!("IDLE {}s", elapsed);
            let truncated = &msg[..msg.len().min(cols)];
            let padding = cols.saturating_sub(truncated.len());
            print!("\x1b[32m{}{}\x1b[0m", truncated, " ".repeat(padding));
        } else {
            let procs = if self.active_processes.is_empty() {
                "...".to_string()
            } else {
                let joined = self.active_processes.join(",");
                if joined.len() > cols {
                    format!("{}+", &joined[..cols.saturating_sub(1)])
                } else {
                    joined
                }
            };
            let padding = cols.saturating_sub(procs.len());
            print!("\x1b[34m{}{}\x1b[0m", procs, " ".repeat(padding));
        }
    }
}

impl State {
    fn run_idle_check(&self) {
        let pid_str = self.zellij_pid.to_string();
        let claude_detect = if self.claude_code_idle_detection {
            "true"
        } else {
            "false"
        };
        let ignore_procs = self.ignore_processes.join(",");
        let mut context = BTreeMap::new();
        context.insert("command".to_string(), "idle_check".to_string());
        run_command(
            &[
                "bash",
                "-c",
                IDLE_CHECK_SCRIPT,
                "_",
                &pid_str,
                claude_detect,
                &ignore_procs,
            ],
            context,
        );
    }

    fn trigger_suspend(&mut self) {
        if self.suspend_command_sent {
            return;
        }
        self.suspend_command_sent = true;

        if self.suspend_action == "none" {
            eprintln!("zellij-idle: suspend_action is 'none', skipping gcloud command");
            return;
        }

        let action = match self.suspend_action.as_str() {
            "stop" => "stop",
            _ => "suspend",
        };

        let mut context = BTreeMap::new();
        context.insert("command".to_string(), "suspend".to_string());
        run_command(&["bash", "-c", SUSPEND_SCRIPT, "_", action], context);
    }

    fn parse_idle_check_output(&mut self, stdout: &[u8]) {
        let output = String::from_utf8_lossy(stdout);
        let mut active_count = 0;
        let mut active_procs = Vec::new();
        let mut idle_details = Vec::new();
        let mut active_details = Vec::new();
        let mut total_panes = 0;

        for line in output.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            total_panes += 1;

            let parts: Vec<&str> = line.splitn(3, ':').collect();
            if parts.len() < 3 {
                continue;
            }

            if parts[0] == "active" {
                active_count += 1;
                let proc_name = parts[2].trim();
                active_details.push(format!("pid={} fg={}", parts[1], proc_name));
                if !proc_name.is_empty() && proc_name != "unknown" {
                    active_procs.push(proc_name.to_string());
                }
            } else {
                idle_details.push(format!("pid={} {}", parts[1], parts[2].trim()));
            }
        }

        eprintln!(
            "zellij-idle: poll #{}: {}/{} panes active | active=[{}] idle=[{}]",
            self.poll_count,
            active_count,
            total_panes,
            active_details.join(", "),
            idle_details.join(", ")
        );

        let was_idle = self.is_idle;
        self.active_pane_count = active_count;
        self.active_processes = active_procs;

        if active_count == 0 && total_panes > 0 {
            if !self.is_idle {
                self.is_idle = true;
                eprintln!("zellij-idle: -> IDLE (all {} panes idle)", total_panes);
            }
        } else if active_count > 0 {
            if was_idle || self.countdown_active {
                eprintln!(
                    "zellij-idle: -> ACTIVE (keeping awake: {})",
                    self.active_processes.join(", ")
                );
            }
            self.is_idle = false;
            self.idle_elapsed_secs = 0.0;
            self.last_activity_poll_count = self.poll_count;
            self.countdown_active = false;
        }
        // If total_panes == 0, keep current state (startup or no terminal panes yet)
    }
}
