export RUST_LOG := "wallhack=trace,wallhack::host=trace,wallhack::agent=trace"

host-connect:
  cargo run --bin host -- -t noclip0 connect localhost:6565