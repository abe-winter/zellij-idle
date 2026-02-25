use std::collections::BTreeMap;
use zellij_tile::prelude::*;

const POLL_INTERVAL_SECS: f64 = 5.0;
const DEFAULT_IDLE_TIMEOUT_SECS: f64 = 300.0;
const DEFAULT_COUNTDOWN_SECS: f64 = 60.0;

// Inline bash script for idle detection.
// Finds direct children of zellij, checks /proc/<pid>/stat to determine
// if the shell is the foreground process (idle) or something else is running (active).
// Skips processes without a controlling terminal (tty_nr == 0).
// $1 = zellij PID, passed as positional arg to bash -c.
const IDLE_CHECK_SCRIPT: &str = r#"for child in $(pgrep -P "$1"); do
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
    fg_comm=$(cat /proc/$tpgid/comm 2>/dev/null || echo "unknown")
    echo "active:$child:$fg_comm"
  fi
done"#;

#[derive(Default)]
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

    // Config (from layout.kdl)
    idle_timeout_secs: f64,
    countdown_secs: f64,
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
                        }
                    } else if self.is_idle && self.idle_elapsed_secs >= self.idle_timeout_secs {
                        self.countdown_active = true;
                        self.countdown_remaining = self.countdown_secs;
                    }

                    self.run_idle_check();
                } else {
                    self.loaded = true;
                }
                set_timeout(POLL_INTERVAL_SECS);
                true
            }
            Event::PermissionRequestResult(_) => true,
            Event::RunCommandResult(_exit_code, stdout, _stderr, _context) => {
                self.parse_idle_check_output(&stdout);
                true
            }
            Event::InputReceived => {
                self.last_activity_poll_count = self.poll_count;
                self.idle_elapsed_secs = 0.0;
                self.is_idle = false;
                self.countdown_active = false;
                self.countdown_remaining = 0.0;
                self.suspend_triggered = false;
                true
            }
            _ => false,
        }
    }

    fn render(&mut self, _rows: usize, cols: usize) {
        if !self.loaded {
            print!("zellij-idle: loading...");
            return;
        }

        if self.suspend_triggered {
            let msg = " SUSPENDING NOW... ";
            let padding = cols.saturating_sub(msg.len());
            let left = padding / 2;
            let right = padding - left;
            print!(
                "\x1b[41;97;1m{}{}{}\x1b[0m",
                " ".repeat(left),
                msg,
                " ".repeat(right)
            );
        } else if self.countdown_active {
            let remaining = self.countdown_remaining.max(0.0) as u64;
            let msg = format!(" SUSPENDING in {}s -- press any key to cancel ", remaining);
            let padding = cols.saturating_sub(msg.len());
            let left = padding / 2;
            let right = padding - left;
            print!(
                "\x1b[43;30;1m{}{}{}\x1b[0m",
                " ".repeat(left),
                msg,
                " ".repeat(right)
            );
        } else if self.is_idle {
            let countdown_eta = (self.idle_timeout_secs - self.idle_elapsed_secs).max(0.0) as u64;
            let mins = countdown_eta / 60;
            let secs = countdown_eta % 60;
            let eta_str = if mins > 0 {
                format!("{}m{:02}s", mins, secs)
            } else {
                format!("{}s", secs)
            };
            let msg = format!(
                " IDLE {}s | suspend in {} ",
                self.idle_elapsed_secs as u64, eta_str
            );
            let padding = cols.saturating_sub(msg.len());
            print!("\x1b[32m{}{}\x1b[0m", msg, " ".repeat(padding));
        } else {
            let procs = if self.active_processes.is_empty() {
                "...".to_string()
            } else {
                self.active_processes.join(", ")
            };
            let msg = format!(" ACTIVE: {} ", procs);
            let padding = cols.saturating_sub(msg.len());
            print!("\x1b[34m{}{}\x1b[0m", msg, " ".repeat(padding));
        }
    }
}

impl State {
    fn run_idle_check(&self) {
        let pid_str = self.zellij_pid.to_string();
        run_command(
            &["bash", "-c", IDLE_CHECK_SCRIPT, "_", &pid_str],
            BTreeMap::new(),
        );
    }

    fn parse_idle_check_output(&mut self, stdout: &[u8]) {
        let output = String::from_utf8_lossy(stdout);
        let mut active_count = 0;
        let mut active_procs = Vec::new();
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
                if !proc_name.is_empty() && proc_name != "unknown" {
                    active_procs.push(proc_name.to_string());
                }
            }
        }

        self.active_pane_count = active_count;
        self.active_processes = active_procs;

        if active_count == 0 && total_panes > 0 {
            if !self.is_idle {
                self.is_idle = true;
            }
        } else if active_count > 0 {
            self.is_idle = false;
            self.idle_elapsed_secs = 0.0;
            self.last_activity_poll_count = self.poll_count;
            self.countdown_active = false;
        }
        // If total_panes == 0, keep current state (startup or no terminal panes yet)
    }
}
