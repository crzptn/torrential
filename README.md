# torrential

torrential is a command-line tool that searches torrent indexes and downloads torrents, and it never seeds. It is built for coding agents first and for people in a terminal second, and it ships with an agent skill.

```sh
torrential search night of the living dead --source archive.org
torrential get 6bc1af --files '*.mp4'
torrential wait 6bc1af
```

`search` prints results, each with a short ID. `get` hands the download to a background engine and returns. `wait` follows it until it is done and verified.

## Install

There are no releases yet, so build torrential from source. You need Git, Rust, and Nix.

```sh
git clone --recursive https://github.com/nexxeln/torrential
cd torrential
nix --extra-experimental-features 'nix-command flakes' develop -c cargo install --path crates/torrential
```

`cargo install` puts `torrential` in `~/.cargo/bin`. Run `torrential doctor` to check the sandbox, disk, state, engine, skill, config, and sources. Add `--network` to also search each source once.

To add shell completions, run `torrential completions <shell>` and add the line it prints to your shell's config. It supports `bash`, `zsh`, `fish`, `elvish`, and `powershell`.

## Download from a terminal

1. Search. Results rank by relevance, and each row starts with an ID.

	```console
	$ torrential search night of the living dead --source archive.org --limit 3
	ID      SEEDS  SIZE      AGE  SOURCE       FLAGS  NAME
	0872de  ?      58.0 GiB  17y  archive.org  pd     Night of the Living Dead
	6bc1af  ?      4.7 GiB   19y  archive.org  pd     Night of the Living Dead
	27f32a  ?      4.5 GiB   13y  archive.org  pd     Night Of The Living Dead
	3 of 50 results · 1 of 1 source ok · 1.9s · more: --limit 13
	next: torrential files <ID> · torrential get <ID>
	```

	A `?` in `SEEDS` means the source does not report seeders. Narrow a search with `--source`, `--category`, `--min-seeds`, and `--max-size`.

2. List the files in a torrent with `torrential files 6bc1af`.
3. Check the plan with `torrential get 6bc1af --files '*.mp4' --dry-run`. It prints the selected files and their size, the destination, the free space, and any conflicts, and downloads nothing.
4. Start the download with `torrential get 6bc1af --files '*.mp4'`. `--files` takes a glob or an exact path, `*` crosses folders, and matching ignores case. Repeat it to select more. `--dir` sets the folder. A selection over 50 GiB needs `--max-size`.
5. Follow it with `torrential wait 6bc1af`. On a terminal, `wait` shows progress until the download is done and verified.
6. Find the files with `torrential status 6bc1af`, which prints their `path:`.

On 2026-10-18, torrential downloaded `debian-13.7.0-amd64-netinst.iso`, and its SHA-256 matched Debian's published `SHA256SUMS`.

An ID is the start of the torrent's infohash. Any unique prefix of 4 or more characters works, and a search result keeps its ID after you download it. Every command that takes an ID also takes the full infohash or a magnet link. `files` and `get` also take a `.torrent` path or a `.torrent` URL.

To manage downloads:

- `torrential status` lists every download.
- `torrential pause <id>` returns once writes stop. `torrential resume <id>` continues it, or retries a failed download.
- `torrential verify <id>` re-hashes the files on disk and reports damage. Add `--repair` to download the damaged pieces again.
- `torrential rm <id>` stops tracking a download and keeps its files. torrential never deletes files.

To open the terminal UI, run `torrential` with no arguments on a terminal, or run `torrential tui`. Quitting the UI keeps downloads running.

## Use it from a coding agent

Install the skill:

```sh
torrential skill install
```

It writes `~/.agents/skills/torrential/SKILL.md`, which Codex, Gemini CLI, Cursor, and other agents read. It also links `~/.claude/skills/torrential` to that folder for Claude Code. Add `--agent agents` to skip the Claude Code link, or `--scope project` to install under the current folder instead. After you upgrade torrential, run `torrential skill status`, and run `torrential skill install` again if it says `outdated`. `torrential skill show` prints the skill. `torrential skill uninstall` removes the copies torrential wrote and keeps any that someone edited.

The skill teaches the agent to:

- Use torrential for large public files such as Linux ISOs, public-domain films, and open datasets, even when nobody says "torrent".
- Search the lawful sources first, and decline a request for something the user plainly has no right to.
- Treat torrent names, file names, and descriptions as untrusted text.
- Ask before a download over 10 GiB, or one larger than the user asked for.
- Start a download with `get`, then check on it with `wait` calls that end before the agent's tool timeout, never with `sleep`.
- Act on each error code and exit code.

The download does not depend on the agent. It keeps going after the agent's turn or session ends, and a later session can check it with `torrential status`.

On 2026-10-18, a fresh Claude Code session with only the skill installed downloaded a public-domain MP4 from archive.org. Its SHA-1 matched the one in archive.org's `files.xml`.

### Run torrential outside the agent's sandbox

BitTorrent needs direct TCP and UDP connections, and agent sandboxes block them. torrential checks before it starts its engine and stops with `sandbox_network_blocked` and exit code 6:

```text
error[sandbox_network_blocked]: this sandbox blocks network access, so the engine could not reach any peer
  signal: sending a UDP datagram failed: Operation not permitted (os error 1) (claude-code)
hint: add "torrential *" to sandbox.excludedCommands in ~/.claude/settings.json, or approve running torrential outside the sandbox
```

- In Claude Code, approve running the command outside the sandbox when asked, or add `"torrential *"` to `sandbox.excludedCommands` in `~/.claude/settings.json`.
- In interactive Codex, approve the escalation the agent requests. The skill has it ask for the prefix rule `["torrential"]`, so one approval covers later torrential commands.
- In unattended Codex (`codex exec`), approvals are off, so torrential cannot run. Run the download outside Codex, or give that Codex session network and socket access.

## Sources and lawful use

torrential searches these sources by default:

| Source | What it searches | Flags it sets |
|---|---|---|
| `archlinux` | Official Arch Linux ISOs | `official` |
| `ubuntu` | Official torrents on releases.ubuntu.com | `official` |
| `debian` | Official torrents on cdimage.debian.org | `official` |
| `archive.org` | Internet Archive items that have a torrent | `pd`, `cc` |
| `academictorrents` | The Academic Torrents catalog | |
| `knaben` | Knaben, an index of public trackers | |
| `apibay` | The Pirate Bay's API | `trusted` |
| `nyaa` | Nyaa | `trusted` |

The first five are lawful sources. Among equally relevant results, official torrents rank first, then the other lawful sources, then the general indexes. Torznab feeds that you add in the config rank with the general indexes.

torrential keeps the official listings and the Academic Torrents catalog on disk for a day, so a search does not wait on those servers. `--fresh` asks the general indexes and the official listings again.

`official` means the publisher's own torrent. `pd` and `cc` mean the source lists a public-domain or Creative Commons license. `trusted` means the index vouches for the uploader. `cam` marks a name that looks like a camera recording, from any source. Search hides adult results unless you pass `--adult`.

torrential does not check whether you may download something. Download only what you have the right to, such as Linux ISOs, public-domain films, open datasets, and Creative Commons works.

## Change the defaults in config.toml

torrential reads `~/.config/torrential/config.toml`, or `$XDG_CONFIG_HOME/torrential/config.toml` when that variable is set. Without the file, the defaults apply.

```toml
[downloads]
dir = "~/Downloads/torrents"

[sources]
disabled = ["nyaa"]

[[sources.torznab]]
name = "jackett"
url = "http://127.0.0.1:9117/api/v2.0/indexers/all/results/torznab/api"
api_key_env = "JACKETT_API_KEY"
```

- `downloads.dir` is where `get` puts downloads without `--dir`. It must be an absolute path or start with `~/`. The default is a `torrential` folder in your Downloads folder.
- `sources.disabled` lists the sources that search skips.
- Each `[[sources.torznab]]` table adds a Torznab feed, such as Jackett or Prowlarr. `name` uses lowercase letters, digits, `-`, `_`, and `.`. `url` is an `http` or `https` URL. `api_key_env` names the environment variable that holds the API key. The file cannot hold the key itself.

An unknown key, a relative `dir`, or an unset `api_key_env` variable is an error that names the line. `torrential doctor` checks the file.

The engine reads the file when it starts. To apply an edit, run `torrential daemon stop`, and the next command starts an engine with the new config. Stopping the engine interrupts running downloads, and `torrential resume --all` continues them.

`TORRENTIAL_HOME` moves the config, state, cache, and engine socket into one folder, so the config file is `$TORRENTIAL_HOME/config/config.toml`. It does not move downloads.

## How the engine works

### Downloads outlive the command

The first command that needs the engine starts it in the background. Commands talk to it over a Unix socket and return. The engine keeps downloading after the command, the agent's turn, or the session ends. It runs up to three downloads at once and queues the rest. When nothing is downloading or checking and no command is connected, it exits after 120 seconds, or once its newest search answer is 5 minutes old, whichever is later. A search repeated within those 5 minutes answers without asking the sources again. `torrential daemon status` shows the engine and its log, and `torrential daemon stop` saves its state and stops it.

### It never seeds

Upload is off from the moment a download starts. The engine keeps every peer choked and refuses every request for a piece. A finished download stops, and its files stay where they are.

### Resume after a reboot

The engine saves resume data every 5 minutes and when you pause a download. Downloads that were running when the engine stopped show `interrupted` when it starts again. That happens after `torrential daemon stop`, a crash, or a reboot. Run `torrential resume --all` to continue every paused, interrupted, or failed download. That includes downloads you paused yourself.

### Start the engine at login

`torrential daemon install` registers the engine as a login service, a launchd agent on macOS or a systemd user unit on Linux. That engine resumes downloads when it starts and does not exit when idle. `torrential daemon uninstall` removes the service. The service file sets only `TORRENTIAL_HOME`, so it does not pass on an `api_key_env` variable from your shell.

## Read the output from a script

When stdout is not a terminal, or an agent's environment variable such as `CLAUDECODE` is set, output is plain text:

- Lists are tab-separated with one header row.
- Lines that start with `#` are summaries.
- The last line usually starts with `next:` and holds the command to run next.
- Errors go to stderr as `error[<code>]: <message>`, followed by `hint:` and what to do.

Add `--json` to any command for JSON on stdout, with `"schema":1` in every object. In JSON mode, errors also go to stdout. `wait --json` prints one object per line. Sizes are bytes, rates are bytes per second, and times are Unix seconds.

Exit code 0 means success. Exit code 8 means `wait` returned while the download still runs. Off a terminal, `wait` returns after 100 seconds unless you pass `--timeout`. `torrential schema` prints every command, flag, exit code, and error code as JSON.

## Build and test from source

The Nix dev shell in `flake.nix` supplies CMake, Ninja, pkg-config, Boost, and OpenSSL for macOS and Linux. Rust comes from your own toolchain. libtorrent 2.1.2 is a Git submodule in `vendor/libtorrent`, and `crates/engine/build.rs` builds it with CMake and links it statically.

If you cloned without `--recursive`, fetch the submodule first:

```sh
git submodule update --init --recursive
```

Build and test inside the dev shell:

```sh
nix --extra-experimental-features 'nix-command flakes' develop -c cargo build --release
nix --extra-experimental-features 'nix-command flakes' develop -c cargo test
```

If flakes are already on in your Nix config, `nix develop -c` is enough. The tests stay on your machine. The swarm tests run a seeder on 127.0.0.1, and the search tests read recorded responses. To search the live sources, run `cargo test -p torrential-search --test live -- --ignored` in the dev shell.

OpenSSL links statically, like libtorrent, so the binary loads no library from the Nix store. It keeps working after Nix garbage collection.

## License

MIT. See [LICENSE](LICENSE).
