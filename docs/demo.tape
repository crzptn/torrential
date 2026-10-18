# Records docs/demo.gif with VHS 0.11. Run it from the repository root after
# `nix develop -c cargo build --release -p torrential`:
#
#   vhs docs/demo.tape
#
# It downloads one 59 MiB Creative Commons film from archive.org into /tmp and
# deletes it at the end.

Output docs/demo.gif

Set Shell "bash"
Set FontFamily "Menlo"
Set FontSize 18
Set Width 1200
Set Height 560
Set Padding 28
Set Margin 20
Set MarginFill "#837C70"
Set WindowBar Rings
Set WindowBarSize 40
Set BorderRadius 10
Set Framerate 24
Set TypingSpeed 40ms
Set PlaybackSpeed 1.35
Set CursorBlink false
Set WaitTimeout 120s
Set WaitPattern /\$$/
Set Theme {"name":"margin","background":"#14120F","foreground":"#F0E9DC","selection":"#3B3730","cursor":"#A6C79A","cursorAccent":"#14120F","black":"#3B3730","brightBlack":"#837C70","red":"#E2806E","brightRed":"#E2806E","green":"#A6C79A","brightGreen":"#A6C79A","yellow":"#D9BC8C","brightYellow":"#D9BC8C","blue":"#BDB5A6","brightBlue":"#F0E9DC","magenta":"#E2806E","brightMagenta":"#E2806E","cyan":"#A6C79A","brightCyan":"#A6C79A","white":"#BDB5A6","brightWhite":"#F0E9DC"}

# torrential prints plain agent output when it sees an agent's variables, so
# clear every one that crates/torrential/src/env.rs checks. A scratch home keeps
# the demo engine and its download away from yours.
Hide
Type "unset CLAUDECODE CLAUDE_CODE_CHILD_SESSION CODEX_THREAD_ID CODEX_CI GEMINI_CLI CURSOR_AGENT AI_AGENT CODEX_SANDBOX CODEX_SANDBOX_NETWORK_DISABLED"
Enter
Type `export PATH="$PWD/target/release:$PATH" TORRENTIAL_HOME=/tmp/torrential-demo/home`
Enter
Type "torrential daemon stop >/dev/null 2>&1; rm -rf /tmp/torrential-demo; mkdir -p $TORRENTIAL_HOME/config"
Enter
Type `printf '[downloads]\ndir = "/tmp/torrential-demo/downloads"\n' > $TORRENTIAL_HOME/config/config.toml`
Enter
Type `PS1='\[\e[38;2;166;199;154m\]$\[\e[0m\] '; clear`
Enter
Sleep 500ms
Show

Type "torrential search big buck bunny --source archive.org --limit 5"
Sleep 400ms
Enter
Wait
Sleep 3s

Type "torrential get b63eaa --files '*.mp4'"
Sleep 400ms
Enter
Wait
Sleep 3.5s

Hide
Type "torrential daemon stop; rm -rf /tmp/torrential-demo"
Enter
Sleep 2s
