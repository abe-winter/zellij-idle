# zellij-idle

This is a zellij plugin to suspend a cloud machine when all of the terminals are idle. The reason to do this is that I'm using zellij on a big expensive cloud machine and I'd like to pause it when not actively in use to save $$$.

## how to install

```sh
rustup target add wasm32-wasip1
cargo build --release
# binary in target/wasm32-wasip1/release/zellij-idle.wasm
```

- copy that output to `~/.config/zellij/plugins/` on the dest machine
- copy the layout / config file, idle.kdl, to ~/.config/zellij/layouts on the dest
- launch zellij with the layout:

```sh
zellij -l idle
# alternatively, look into `default_layout`
```

todo:
- IAM requirements

Logs will be at `~/.local/share/zellij-idle/zellij-idle.log`.
