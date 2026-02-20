Output demo.mp4
Output demo.gif

Set Shell "bash"
Set FontSize 13
Set Width 1400
Set Height 700
Set Theme "Catppuccin Mocha"
Set Padding 4
Set TypingSpeed 40ms

# ── SETUP (hidden) ────────────────────────────────────────────────────────────
Hide
Type "tmux kill-session -t wallhack-demo 2>/dev/null; true"
Enter
Sleep 300ms

# Two-pane session: left (wide) = entry REPL, right (narrow) = exit node
Type "tmux new-session -d -s wallhack-demo -x 215 -y 52"
Enter
Sleep 200ms
Type "tmux split-window -h -t wallhack-demo:0.0 -l 72"
Enter
Sleep 200ms
# Ensure left pane is focused on attach
Type "tmux select-pane -t wallhack-demo:0.0"
Enter
Sleep 200ms
Type "tmux attach -t wallhack-demo"
Enter
Sleep 500ms
Show

# ── LEFT PANE: ENTRY NODE (attacker, 10.99.1.100) ────────────────────────────
Type "# [attacker  10.99.1.100]  —  entry node"
Enter
Sleep 400ms
Type "docker exec -it wallhack-range-attacker-1 wallhack entry --listen :4444"
Enter
Sleep 3s

# ── RIGHT PANE: EXIT NODE (gateway-perimeter, 10.99.1.10) ────────────────────
Ctrl+B
Sleep 100ms
Right
Sleep 400ms

Type "# [gateway-perimeter  10.99.1.10]  —  exit node"
Enter
Sleep 400ms
Type "docker exec -it wallhack-range-gateway-perimeter-1 wallhack exit --connect 10.99.1.100:4444 --name peer1"
Enter
Sleep 6s

# ── ENTRY REPL: INSPECT + ROUTE ──────────────────────────────────────────────
Ctrl+B
Sleep 100ms
Left
Sleep 500ms

Type "peers"
Enter
Sleep 2s

Type "route add 10.99.2.0/24 via peer1"
Enter
Sleep 2s

Type "routes"
Enter
Sleep 2s

# ── VERIFY: PING THROUGH TUNNEL ───────────────────────────────────────────────
# Open a new tmux window (host shell) to test reachability
Ctrl+B
Sleep 100ms
Type "c"
Sleep 400ms

Type "# office network (10.99.2.0/24) is now reachable through the tunnel"
Enter
Sleep 300ms
Type "docker exec wallhack-range-attacker-1 ping -c 4 -W 2 10.99.2.10"
Enter
Sleep 8s

Sleep 2s
