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

## License

MIT. See [LICENSE](LICENSE).
