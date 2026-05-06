# Cyber Range — Fictional Content Notice

Everything under `range/` is content for a self-contained cyber range used to
test wallhack. The VMs run only inside an isolated, ephemeral pontoon network.
None of these credentials, keys, or hostnames are real and none of them grant
access to anything outside the range.

This includes, but is not limited to:

- Plaintext passwords in `range/layers/*/layer.yml` and discoverable "loot"
  files (e.g. `intranet/.../creds.txt`, `app-api/.../ssh.conf`).
- The ed25519 private key at `range/layers/ftp-loot/ftp/backup/id_ed25519`,
  generated specifically for the `ssh-leaked-key` challenge.
- Internal IPs in the `10.99.0.0/16` private range.

If your secret scanner pointed you here: this directory is excluded via
`.github/secret_scanning.yml`. The credentials are part of the test fixture,
not a leak.
