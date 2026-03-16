# Quickstart demo: help → entry node + exit node connection
# Run with: vhs docs/quickstart.tape
# NOTE: Requires two wallhack instances; use separate IPC socket paths or
#       containers if the default /run/user/.../wallhackd.sock conflicts.

Output website/public/quickstart.gif
Output website/public/quickstart.mp4

Set Shell "bash"
Set FontSize 14
Set Width 1400
Set Height 700
Set Theme "Catppuccin Mocha"
Set Padding 8
Set TypingSpeed 40ms

# ── PATH setup (hidden) ─────────────────────────────────────────────────────
Hide
Type `export PATH=./target/release:$PATH`
Enter
Sleep 500ms
Type "clear"
Enter
Sleep 300ms
Show

# ── HELP ─────────────────────────────────────────────────────────────────────
Type "wallhack --help"
Enter
Sleep 5s

# ── TMUX SETUP (hidden) ─────────────────────────────────────────────────────
Hide
Type `tmux kill-session -t demo 2>/dev/null; true`
Enter
Sleep 300ms

Type `tmux new-session -d -s demo -x 200 -y 48`
Enter
Sleep 300ms

# Configure left pane — local / entry node
Type `tmux send-keys -t demo:0.0 'export PATH=./target/release:$PATH && clear' Enter`
Enter
Sleep 500ms

# Split and configure right pane — remote / exit node
Type `tmux split-window -h -t demo:0.0`
Enter
Sleep 300ms
Type `tmux send-keys -t demo:0.1 'export PATH=./target/release:$PATH && clear' Enter`
Enter
Sleep 500ms

# Pane border titles so viewers know which is which
Type `tmux set -t demo pane-border-status top`
Enter
Sleep 200ms
Type `tmux select-pane -t demo:0.0 -T " LOCAL MACHINE "`
Enter
Sleep 200ms
Type `tmux select-pane -t demo:0.1 -T " REMOTE MACHINE "`
Enter
Sleep 200ms

# Focus left pane and attach
Type `tmux select-pane -t demo:0.0`
Enter
Sleep 200ms
Type `tmux attach -t demo`
Enter
Sleep 1s
Show

# ── LOCAL: start entry node on port 888 ─────────────────────────────────────
Type "wallhack entry --listen :888"
Enter
Sleep 4s

# ── REMOTE: connect exit node back to local ─────────────────────────────────
Ctrl+B
Sleep 100ms
Right
Sleep 500ms

Type "wallhack exit --connect 127.0.0.1:888 --name peer1"
Enter
Sleep 5s

# ── LOCAL: see the peer connect ─────────────────────────────────────────────
Ctrl+B
Sleep 100ms
Left
Sleep 3s

# ── QUIT ─────────────────────────────────────────────────────────────────────
Ctrl+C
Sleep 1s

Ctrl+B
Sleep 100ms
Right
Sleep 300ms
Ctrl+C
Sleep 2s

