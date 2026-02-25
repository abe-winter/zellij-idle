# tasks

This is a list of tasks; can you mark them with a `[~]` when you start work and a `[x]` when finished? write a (very) short summary of work under the task if appropriate / non-trivial.

1. due diligence -- double check that this doesn't exist. where would it be? (zellij plugin marketplace, github). are there forum questions asking how to do this? does this exist in other contexts? (tmux?)

2. basic design questions
	a. define precisly what does 'idle' mean? No running processes other than bash?
		- is this watch-based or polled? are there perf concerns?
	b. what is necessary for a cloud VM to suspend itself? we don't want this to require a highly privileged account. is there an OS level action that the cloud host will interpret as pausing the machine? (note, we're targeting GCP at first). are there any secrets needed?
	c. zellij plugin basics. are there multiple types? (native vs wasm?). which is better for us? what API is available to detect the thing we're trying to detect? (the idle signal from 1a above). where is state + config stored?
	d. in zellij-land, are we allowed to have a background thread for polling / watching the processes? does zellij have a native cron or timer approach? (extreme bonus) is there a way to do this entirely within the zellij api?
	e. what is the UI/UX for zellij plugins? specifically, how are they installed, configured, displayed on screen? are there choices about where to show it? (on top / bottom status bars vs inside a menu tree)

3. basic plugin implementation:
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

4. basic system suspend implementation
	- tell me how you're planning to pause the cloud VM and I'll manually test the approach
	- once it works, integrate it into the plugin

5. special idle detection for claude code
	- claude code is a chat interface that the user will forget to close, but sometimes it is actually awake and doing work
	- can we distinguish between these two cases? how? subprocesses, some kind of state written by claude itself, growth of session logs, draw events on the virtual terminal?
	- if there's a good deterministic signal, add a config for it and detect it
	- if not, consider a config to sleep some processes by CPU usage
	- if that's not viable, add a config to simply ignore some processes for the purposes of idle detection

6. add logging somewhere so we can debug sleep / don't-sleep decisions. logs should at minimum:
	- say which process(es) are keeping the system awake
	- log some granular information about the claude code special case (because this is edge-case-y; I'm guessing we'll get this wrong at first and also the idle signal will change as the claude code chat UI itself evolves)

## future

tasks in this section need clarification before starting

