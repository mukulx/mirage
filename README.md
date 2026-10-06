# Mirage Launcher

A fast terminal launcher for Minecraft, built in Rust around a full-screen TUI.

> **Alpha:** under active development. Expect bugs and breaking changes.

## Features

- **Instances** with their own Minecraft version, Fabric or Quilt loader, RAM, Java path and JVM flags
- **Modrinth** mod and modpack (`.mrpack`) install, with a version picker, category filters and cached results
- **Mod updates:** a `↑` badge marks mods with a newer build, and required dependencies install automatically
- **Play in the TUI:** the game's log and your playtime live on the instance page
- **Crash hints:** wrong Java, out of memory, missing dependency or a bad mixin, in plain words
- **Export** any instance as a `.mrpack`
- **Offline friendly:** saved Modrinth results and your saved session keep working without a network
- Parallel downloads with SHA-1 verification

## Install

Prebuilt binary (Linux x86_64 and aarch64), a few seconds:

```bash
curl -fsSL https://raw.githubusercontent.com/mukulx/mirage/main/install.sh | bash
```

Or build from source:

```bash
cargo install --git https://github.com/mukulx/mirage
```

## Usage

```bash
mirage
```

Sign in from Settings (`6`, then `M`), create an instance (`5`) or install a modpack (`4`), then press `Enter` on it to play. The same actions are available as commands: run `mirage --help`.

## Keys

Press `?` in the app for the full list.

| Anywhere | |
|---|---|
| `1`-`6`, `Tab`, `←` `→` | Switch tab |
| `↑` `↓`, `j` `k` | Move |
| `?` | Show all keys |
| `Q` | Quit |

| Instances | |
|---|---|
| `Enter` | Open the instance page and launch |
| `E` | Settings: RAM, Java path, JVM flags |
| `M` / `S` | Installed mods / find mods |
| `C` / `D` / `N` | Clone / delete / new instance |
| `P` | Export to `~/Downloads/<name>.mrpack` |
| `K` | Stop the game on the instance page (twice to force) |

| Mods | |
|---|---|
| `Space` | Enable or disable |
| `U` / `Shift-U` | Update one / update all |
| `D` | Remove |
| `A` | Add mods |

| Search | |
|---|---|
| `/` | Type a query |
| `Enter` | Install the best matching version |
| `V` | Pick an exact version |
| `O` / `F` | Cycle sort / category |
| `C` | Mods: this instance only, or any loader and version |

| Settings | |
|---|---|
| `M` | Sign in |
| `T` | Game opens in a new window or in the TUI |
| `Y` | Cycle theme |
