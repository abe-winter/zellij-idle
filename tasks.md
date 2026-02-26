# tasks

This is a list of tasks; can you mark them with a `[x]` when finished? write a (very) short summary of work under the task if appropriate / non-trivial.

1. [x] due diligence -- double check that this doesn't exist. where would it be? (zellij plugin marketplace, github). are there forum questions asking how to do this? does this exist in other contexts? (tmux?)

	**Finding: This does not exist. Nothing to build on, but there is useful prior art.**

	- **Zellij plugin ecosystem**: Checked [awesome-zellij](https://github.com/zellij-org/awesome-zellij), the plugin manager, GitHub issues/discussions, and web searches. No plugin exists for idle detection + VM suspend. The closest plugin is [zellij-autolock](https://github.com/fresh2dev/zellij-autolock), which detects running processes in panes but only toggles locked/normal mode. Also notable: [claude-code-zellij-status](https://github.com/thoo/claude-code-zellij-status) monitors Claude Code activity in zellij panes (relevant to task #5).
	- **tmux ecosystem**: No equivalent plugin. [tmux-suspend](https://github.com/MunifTanjim/tmux-suspend) is about suspending keybindings for nested sessions, not VM suspend.
	- **Forum/community interest**: Found [zellij issue #509](https://github.com/zellij-org/zellij/issues/509) about CPU usage when idle (fixed via async IO), but no one has asked for idle-triggered VM suspend.
	- **OS-level prior art** (not zellij-specific):
		- [GCE auto-suspend gist](https://gist.github.com/ozaki-r/b9df931e33e119df3a2626e418873bd9): shell script polling `/proc/loadavg` for `0.00 0.00 0.00`, then calls `gcloud compute instances suspend`. Simple but coarse — doesn't know about terminal semantics.
		- [autosuspend](https://github.com/languitar/autosuspend): Python daemon, condition-based system suspend. More sophisticated but still OS-level, no terminal awareness.
		- [GCP Workstations idle timeout](https://oneuptime.com/blog/post/2026-02-17-how-to-configure-idle-timeout-and-auto-stop-policies-to-reduce-google-cloud-workstation-costs/view): built-in idle timeout for Cloud Workstations (not regular GCE VMs).
	- **Conclusion**: Building this as a zellij plugin is novel. The zellij plugin API has the pieces we need (Timer events for polling, RunCommand for executing `gcloud`, pane/process introspection). The OS-level gists validate the `gcloud compute instances suspend` approach for the actual suspension.

2. [x] basic design questions
	a. define precisly what does 'idle' mean? No running processes other than bash?
		- is this watch-based or polled? are there perf concerns?
	b. what is necessary for a cloud VM to suspend itself? we don't want this to require a highly privileged account. is there an OS level action that the cloud host will interpret as pausing the machine? (note, we're targeting GCP at first). are there any secrets needed?
	c. zellij plugin basics. are there multiple types? (native vs wasm?). which is better for us? what API is available to detect the thing we're trying to detect? (the idle signal from 1a above). where is state + config stored?
	d. in zellij-land, are we allowed to have a background thread for polling / watching the processes? does zellij have a native cron or timer approach? (extreme bonus) is there a way to do this entirely within the zellij api?
	e. what is the UI/UX for zellij plugins? specifically, how are they installed, configured, displayed on screen? are there choices about where to show it? (on top / bottom status bars vs inside a menu tree)

	### 2a. Defining "idle"

	**Definition: A pane is idle when its foreground process is just the shell (bash/zsh/fish).** Any non-shell foreground process = not idle.

	**Detection mechanism:** Check each PTY's foreground process group via `/proc/<pid>/stat`. If the shell's PGID == the terminal's TPGID, the shell is at its prompt. This is the same mechanism tmux uses for `pane_current_command` and bash uses for `TMOUT`.

	**Must be polled, not purely event-driven.** Zellij fires `PaneUpdate` on focus/title/geometry changes, but NOT when a foreground process starts or stops inside a pane. So we poll via `set_timeout` + `run_command` (calling `ps` or reading `/proc`). A 5-10 second poll interval is fine — `ps` takes ~1-5ms and our idle threshold is measured in minutes.

	**Hybrid optimization:** Use `InputReceived` events to instantly reset the idle timer (user is typing), and use polling to confirm idle state before triggering countdown.

	**Edge cases (conservative — any running process blocks suspend):**
	| Scenario | Idle? | Why |
	|---|---|---|
	| Shell at prompt | YES | Definition of idle |
	| vim/nvim sitting open | NO | Foreground process, even if user walked away |
	| `sleep 3600` | NO | Foreground process |
	| `tail -f` with no output | NO | Foreground process |
	| Compilation running | NO | Foreground process |
	| Background job (`sleep &`) + shell prompt | YES | Shell is at prompt; bg jobs don't count |
	| Ctrl+Z suspended process | YES | Shell reclaims foreground |
	| Claude Code | NO | Always looks active — needs special handling (task #5) |

	### 2b. Cloud VM self-suspend

	**Use `gcloud compute instances suspend`.** This preserves full RAM state (like closing a laptop lid). Resume is fast — processes, file descriptors, editor state all survive. `stop` is the fallback (clears RAM, requires full reboot).

	**No secrets needed.** The VM's metadata server at `metadata.google.internal` provides OAuth2 tokens for the attached service account. No key files, no `gcloud auth login`.

	**Minimal permissions setup:**
	1. Create a custom IAM role with just `compute.instances.suspend` permission
	2. Create a dedicated service account, bind the role at the instance level (not project)
	3. Attach the service account to the VM with `cloud-platform` scope

	Self-suspend script (the plugin will do this via `run_command`):
	```bash
	VM_NAME=$(curl -s "http://metadata.google.internal/computeMetadata/v1/instance/name" -H "Metadata-Flavor: Google")
	VM_ZONE=$(curl -s "http://metadata.google.internal/computeMetadata/v1/instance/zone" -H "Metadata-Flavor: Google" | cut -d '/' -f 4)
	VM_PROJECT=$(curl -s "http://metadata.google.internal/computeMetadata/v1/project/project-id" -H "Metadata-Flavor: Google")
	gcloud compute instances suspend "$VM_NAME" --zone="$VM_ZONE" --project="$VM_PROJECT" --quiet
	```

	**OS-level `systemctl suspend` does NOT work** — GCP ignores in-guest ACPI sleep signals. Must go through `gcloud` or the API.

	**Key caveats:** Suspend doesn't work on E2 instances, GPUs, or VMs >208GB RAM (use `stop` as fallback). Network connections (SSH) break on resume — user must reconnect, but zellij session survives intact. Max 60 days suspended before GCP auto-terminates. Billing stops immediately on suspend.

	**Wake-up:** Must be external — `gcloud compute instances resume` from local machine, Cloud Console, or a Cloud Scheduler job.

	### 2c. Zellij plugin basics

	**Only WASM plugins** — no native plugin mode. All plugins compile to WebAssembly, written in Rust using the `zellij-tile` crate.

	**Idle detection API:** `PaneInfo` (from `PaneUpdate` event) has `title`, `terminal_command`, `is_plugin`, `exited` — but **no PID or running command** for regular terminal panes. Open PRs [#3765](https://github.com/zellij-org/zellij/pull/3765)/[#3800](https://github.com/zellij-org/zellij/pull/3800) would add PIDs but aren't merged. Workaround: use `run_command` to call `ps` and inspect `/proc` on the host.

	**Config:** Passed as key-value pairs in layout KDL files or via `--configuration` CLI flag. No built-in persistent state store — write to filesystem if needed (`FullHdAccess` permission).

	**Permissions needed:** `ReadApplicationState` (subscribe to events), `RunCommands` (run `ps`, `gcloud`), `ChangeApplicationState` (open floating countdown pane).

	### 2d. Timers and background work

	**Timers: Yes.** `set_timeout(seconds)` fires a `Timer` event. Chain calls for a recurring loop — this is the canonical polling pattern.

	**Background threads: Yes, via "Plugin Workers."** Register workers with `register_worker!` macro, communicate via `post_message_to`/`post_message_to_plugin`. Workers don't block rendering. Useful for parsing `ps` output but likely overkill for this use case.

	**Host commands: Yes.** `run_command(&["ps", ...], context)` runs async on the host, results come back via `RunCommandResult` event.

	**Not possible entirely within zellij API** — must shell out to `ps`/`/proc` for process state since `PaneInfo` lacks PIDs.

	### 2e. Plugin UI/UX

	**Installation:** Plugin Manager (`Ctrl+O` then `P`), layout files, CLI (`zellij plugin -- file:plugin.wasm`), `load_plugins` in config, or keybinding.

	**Display options:**
	| Method | Display |
	|---|---|
	| `size=1 borderless=true` in layout | Status bar strip (top/bottom) |
	| Regular layout pane | Tiled pane |
	| `-f` flag or `floating true` | Floating pane |
	| `load_plugins` in config | Background (invisible) |

	**Visibility control:** `hide_self()` / `show_self()` toggle visibility at runtime. `set_selectable(false)` makes it non-interactive (like built-in status bar).

	**Recommended architecture for this plugin:**
	- **Status bar pane** (`size=1 borderless=true`) showing idle/active indicator + countdown
	- When countdown starts, optionally **pop up a floating pane** via a second plugin instance (communicate via `pipe_message_to_plugin`) or just show countdown in the status bar itself
	- Two instances can be coordinated using `zellij:OWN_URL` to launch another copy of the same plugin

3. [x] basic plugin implementation:
	- start by making a stub that is theoretically correct
	- I will manually try to install it to verify
	- add a UI that shows idle / active somewhere in the zellij interface
	- add the idle signal implementation and wire it to the UX
	- add configs
		- t1: idle time before showing countdown
		- t2: countdown time
	- add a countdown to the UI once idle for more than t1
		- if easy, do this by popping up a floating window
		- otherwise do it some other way
	- I'll test at the end of this task

4. [x] if simple, place the UI in the existing top status bar rather than adding an entire new line to the UI

	Plugin now shares the top row with tab-bar via `split_direction="vertical"` (24-col fixed width on the right). Render is compact: just process names (blue), `IDLE 45s` (green), `SUSPEND 42s` (yellow), `SUSPEND!` (red).

5. [x] basic system suspend implementation: integrate your planned gcloud instance suspend / stop command into the plugin

	Added `SUSPEND_SCRIPT` that fetches VM metadata and runs `gcloud compute instances suspend` with stop fallback. Fires via `trigger_suspend()` when countdown hits zero. Config: `suspend_action` = "suspend" (default), "stop", or "none". Commands are context-tagged to distinguish from idle check results.

6. [x] special idle detection for claude code
	- claude code is a chat interface that the user will forget to close, but sometimes it is actually awake and doing work
	- can we distinguish between these two cases? how? subprocesses, some kind of state written by claude itself, growth of session logs, draw events on the virtual terminal?
	- if there's a good deterministic signal, add a config for it and detect it
	- if not, consider a config to sleep some processes by CPU usage
	- if that's not viable, add a config to simply ignore some processes for the purposes of idle detection

7. [x] add logging somewhere so we can debug sleep / don't-sleep decisions. logs should at minimum:
	- say which process(es) are keeping the system awake
	- log some granular information about the claude code special case (because this is edge-case-y; I'm guessing we'll get this wrong at first and also the idle signal will change as the claude code chat UI itself evolves)

	Persistent log at `~/.local/share/zellij-idle/zellij-idle.log` (survives reboot/stop). Also echoes to zellij's stderr log. Logs: config on load, every poll result with per-pane breakdown (pid, fg process, claude annotations), state transitions (IDLE/ACTIVE/COUNTDOWN), input resets, suspend command results. Claude Code panes show `(claude-idle)` or `(claude-working)` in the poll output. Timestamps added by bash on flush.

8. walk me through testing your suspend/stop approach (not doing this inline with task 5 because my dev box is busy and can't be restarted right now)

9. improve Claude Code "thinking" detection — the current child-process heuristic only detects tool execution (bash, file writes spawn children). When Claude Code is thinking/streaming an API response, it's just the main node process with no children, so it's incorrectly classified as idle. Possible signals: `/proc/<pid>/io` read_bytes delta (network activity from API streaming), CPU usage from `/proc/<pid>/stat` utime+stime delta, or checking for open TCP connections to anthropic API endpoints. This is safe-ish to get wrong (suspending mid-thought will resume and retry) but would be more correct.

10. [x] when the gcloud command is not present, can you show the text `!gcloud` in red in the UI?

	Runs `which gcloud` on load (when suspend_action != "none"). If missing, sets `gcloud_missing` flag and render shows `!gcloud` in red bold, overriding normal status display.

11. [x] github workflows to:
	- build the wasm file on push
	- attach a zip to the release that has the wasm and layout in a foldered tarball so you can just unpack it in ~/.config/zellij and get the locations specified in README.md

	Two workflows: `build.yml` (on push/PR to main, builds + uploads artifact) and `release.yml` (on `v*` tag, builds + creates GitHub release with `.wasm` and tarball containing `plugins/zellij-idle.wasm` + `layouts/idle.kdl` — unpack into `~/.config/zellij/`).

12. notify the user on slack when suspending

## future

tasks in this section need clarification before starting

