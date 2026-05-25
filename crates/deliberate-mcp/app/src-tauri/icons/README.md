# App icons

The four PNGs here are 1x1 transparent placeholders generated at scaffold
time. They satisfy Tauri's proc-macro validation so `cargo check` and
`tauri dev` work out of the box.

Replace them with real icons before shipping a build:

```
npx @tauri-apps/cli icon path/to/source.png \
  --output crates/app-tauri/icons
```

`tauri icon` also emits the platform-specific `.icns` / `.ico` files that
`tauri build` (but not `tauri dev`) needs.
