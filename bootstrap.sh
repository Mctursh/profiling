#!/usr/bin/env bash
# bootstrap.sh: run on your Mac right after a fresh server comes up.
# Copies setup-server.sh over, runs it as root, checks your login, opens VS Code.
#
#   ./bootstrap.sh <server-ip> [repo-url]
#
# Keep it in the same folder as setup-server.sh (the profiling-lab repo).

set -euo pipefail

IP="${1:?usage: ./bootstrap.sh <server-ip> [repo-url]}"
USER_NAME="mctursh"
DIR="$(cd "$(dirname "$0")" && pwd)"

# Repo to clone: argument, else this folder's GitHub remote turned into an https URL
REPO="${2:-$(git -C "$DIR" remote get-url origin 2>/dev/null \
  | sed -E 's#^git@github.com:#https://github.com/#')}"
[[ -n "$REPO" ]] || { echo "no repo URL: pass it as the 2nd argument" >&2; exit 1; }
echo "server: $IP   user: $USER_NAME   repo: $REPO"

# A new server can reuse an old IP with a new host key: forget the old one
ssh-keygen -R "$IP" >/dev/null 2>&1 || true
SSH_OPTS=(-o StrictHostKeyChecking=accept-new)

echo "==> copying setup script"
scp "${SSH_OPTS[@]}" "$DIR/setup-server.sh" "root@$IP:/root/setup-server.sh"

echo "==> running setup (10-15 min)"
ssh "${SSH_OPTS[@]}" -t "root@$IP" "bash /root/setup-server.sh --user $USER_NAME --repo $REPO"

echo "==> checking login as $USER_NAME"
ssh "${SSH_OPTS[@]}" -o BatchMode=yes "$USER_NAME@$IP" true
echo "OK: ssh $USER_NAME@$IP works"

# Open VS Code straight into the lab on the server (needs the `code` command on your Mac)
if command -v code >/dev/null; then
  code --remote "ssh-remote+$USER_NAME@$IP" "/home/$USER_NAME/profiling-lab"
else
  echo "In VS Code: Remote-SSH: Connect to Host -> $USER_NAME@$IP, open ~/profiling-lab"
fi
